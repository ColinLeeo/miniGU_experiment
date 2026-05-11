use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::ops::RangeFrom;

use ahash::{HashSet, HashSetExt};
use duckdb::types::FromSql;
use duckdb::Connection;
use itertools::Itertools;
use log::{debug, trace};

use super::catalog_pattern::{CatalogEdge, CatalogEdgeKind, CatalogPattern};
use crate::common::{LabelId, TagId};
use crate::error::GCardResult;

pub fn estimate(
    pattern: CatalogPattern,
    conn: &Connection,
    id_generator: &mut RangeFrom<usize>,
    predefined_order: Option<Vec<u8>>,
) -> GCardResult<f64> {
    debug!("estimate: {:?}", pattern.edges().collect_vec());
    let card = EstimateState::new(pattern, conn, id_generator, predefined_order).estimate()?;
    debug!("card: {card}");
    Ok(card)
}

#[derive(Debug)]
struct EstimateState<'a> {
    pattern: CatalogPattern,
    conn: &'a Connection,
    id_generator: &'a mut RangeFrom<usize>,
    edge_table_map: HashMap<TagId, String>,
    predefined_order: Option<Vec<TagId>>,
}

impl<'a> EstimateState<'a> {
    fn new(
        pattern: CatalogPattern,
        conn: &'a Connection,
        id_generator: &'a mut RangeFrom<usize>,
        predefined_order: Option<Vec<TagId>>,
    ) -> Self {
        Self {
            pattern,
            conn,
            id_generator,
            edge_table_map: HashMap::new(),
            predefined_order,
        }
    }

    fn estimate(mut self) -> GCardResult<f64> {
        assert!(self.pattern.get_vertices_num() >= 1);
        assert!(self.pattern.get_edges_num() >= 1);
        for e in self.pattern.edges() {
            let table_name = create_temp_table(self.conn, e, self.id_generator)?;
            self.edge_table_map.insert(e.tag_id(), table_name);
        }
        if let Some(order) = self.predefined_order.take() {
            for victim in order {
                if self.pattern.get_vertices_num() <= 1 {
                    break;
                }
                self.eliminate_vertex(victim)?;
            }
        } else {
            while self.pattern.get_vertices_num() > 1 {
                let victim = self.choose_victim_vertex();
                self.eliminate_vertex(victim)?;
            }
        }
        self.finalize()
    }

    fn finalize(&mut self) -> GCardResult<f64> {
        assert_eq!(self.pattern.get_vertices_num(), 1);
        assert!(self.pattern.get_edges_num() > 0);
        let final_table_name = format!("temp_result_{}", self.id_generator.next().unwrap());
        let tables = self
            .pattern
            .edges()
            .map(|e| self.edge_table_map.get(&e.tag_id()).unwrap().as_str())
            .collect_vec();
        let vertex = self.pattern.vertices().next().unwrap();
        let sql = build_finalize_statement(&tables, vertex.tag_id());
        let sql = build_final_agg_statement(sql);
        let sql = build_view_statement(sql, &final_table_name);
        execute_sql(self.conn, &sql)?;
        read_scalar_table(self.conn, &final_table_name)
    }

    fn eliminate_vertex(&mut self, vertex_tag_id: TagId) -> GCardResult<()> {
        debug!("eliminate vertex: {vertex_tag_id}");
        let mut tables = Vec::new();
        let mut vertex_to_tables: BTreeMap<_, Vec<&str>> = BTreeMap::new();
        for e in self.pattern.incident_edges(vertex_tag_id).unwrap() {
            let table_name = self.edge_table_map.get(&e.tag_id()).unwrap();
            tables.push(table_name.as_str());
            match e.kind() {
                CatalogEdgeKind::Star { center } => {
                    vertex_to_tables
                        .entry(*center)
                        .or_default()
                        .push(table_name);
                }
                CatalogEdgeKind::Path { src, dst } => {
                    vertex_to_tables.entry(*src).or_default().push(table_name);
                    vertex_to_tables.entry(*dst).or_default().push(table_name);
                }
                CatalogEdgeKind::General(vertices) => {
                    for v in vertices {
                        vertex_to_tables.entry(*v).or_default().push(table_name);
                    }
                }
            }
        }
        let neighbors = vertex_to_tables
            .keys()
            .filter(|v| **v != vertex_tag_id)
            .copied()
            .collect_vec();
        let next_table_id = self.id_generator.next().unwrap();
        let next_table_name = format!("temp_table_{next_table_id}");
        let sql = build_match_statement(&tables, &vertex_to_tables, vertex_tag_id, &neighbors);
        let sql = build_agg_statement(sql, &neighbors);
        let sql = build_view_statement(sql, &next_table_name);
        execute_sql(self.conn, &sql)?;

        let next_edge_tag_id = self.pattern.next_edge_tag_id();
        let new_edge = match &neighbors[..] {
            [center] => CatalogEdge::star(next_edge_tag_id, 0, *center),
            [src, dst] => CatalogEdge::path(next_edge_tag_id, 0, *src, *dst),
            vertices => CatalogEdge::general(next_edge_tag_id, 0, vertices.to_vec()),
        };
        assert!(self
            .edge_table_map
            .insert(new_edge.tag_id(), next_table_name)
            .is_none());
        self.pattern.remove_vertex(vertex_tag_id);
        self.pattern.add_edge(new_edge);
        Ok(())
    }

    fn choose_victim_vertex(&self) -> TagId {
        let mut victim = None;
        let mut min_neighbors = usize::MAX;
        for v in self.pattern.vertices() {
            let mut neighbors = HashSet::new();
            for e in self.pattern.incident_edges(v.tag_id()).unwrap() {
                match e.kind() {
                    CatalogEdgeKind::Star { center } => {
                        neighbors.insert(*center);
                    }
                    CatalogEdgeKind::Path { src, dst } => {
                        neighbors.insert(*src);
                        neighbors.insert(*dst);
                    }
                    CatalogEdgeKind::General(vertices) => neighbors.extend(vertices),
                }
            }
            neighbors.remove(&v.tag_id());
            match neighbors.len().cmp(&min_neighbors) {
                Ordering::Less => {
                    min_neighbors = neighbors.len();
                    victim = Some(v.tag_id());
                }
                Ordering::Equal => {
                    victim = victim.min(Some(v.tag_id()));
                }
                Ordering::Greater => (),
            }
        }
        victim.unwrap()
    }
}

fn read_scalar_table<T: FromSql + Default>(conn: &Connection, table_name: &str) -> GCardResult<T> {
    let sql = format!("select * from {table_name}");
    let result: Option<_> = conn.query_row(&sql, [], |row| row.get(0))?;
    Ok(result.unwrap_or_default())
}

fn build_final_agg_statement(sql: String) -> String {
    format!("select sum(_count) as _count from ({sql})")
}

fn build_finalize_statement(tables: &[&str], vertex: TagId) -> String {
    if tables.len() == 1 {
        let table = tables.first().unwrap();
        return format!("select v{vertex}, _count from {table}");
    }
    let from_clause = tables.join(", ");
    let where_clause = tables
        .iter()
        .tuple_windows()
        .map(|(t1, t2)| format!("{}.v{vertex} = {}.v{vertex}", t1, t2))
        .join(" and ");

    let multipliers: BTreeMap<&str, String> = tables
        .iter()
        .enumerate()
        .map(|(i, t_i)| {
            let multiplier = tables
                .iter()
                .enumerate()
                .filter(|(j, _)| i != *j)
                .map(|(_, t_j)| format!("{t_j}.v{vertex}_mode"))
                .join(" * ");
            (*t_i, multiplier)
        })
        .collect();

    let new_count = multipliers
        .iter()
        .map(|(t, multiplier)| format!("{t}._count * {multiplier}"))
        .join(", ");
    let new_count = format!("least({new_count}) as _count");
    let first_table = tables.first().unwrap();
    format!("select {first_table}.v{vertex} as v{vertex}, {new_count} from {from_clause} where {where_clause}")
}

fn build_match_statement(
    tables: &[&str],
    vertex_to_tables: &BTreeMap<TagId, Vec<&str>>,
    victim: TagId,
    neighbors: &[TagId],
) -> String {
    let from_clause = tables.join(", ");
    let where_clause = vertex_to_tables
        .iter()
        .filter(|(_, t)| t.len() > 1)
        .flat_map(|(v, t)| {
            t.iter()
                .tuple_windows()
                .map(move |(t1, t2)| format!("{t1}.v{v} = {t2}.v{v}"))
        })
        .join(" and ");
    let multipliers: BTreeMap<_, _> = tables
        .iter()
        .enumerate()
        .map(|(i, t_i)| {
            let mut multiplier = tables
                .iter()
                .enumerate()
                .filter(|(j, _)| i != *j)
                .map(|(_, t_j)| format!("{t_j}.v{victim}_mode"))
                .join(" * ");
            if multiplier.is_empty() {
                multiplier = "1".to_string();
            }
            (*t_i, multiplier)
        })
        .collect();
    let new_count = multipliers
        .iter()
        .map(|(t, multiplier)| format!("{t}._count * {multiplier}"))
        .join(", ");
    let new_count = format!("least({new_count}) as _count");

    let new_modes = neighbors
        .iter()
        .map(|neighbor| {
            let table = vertex_to_tables
                .get(neighbor)
                .unwrap()
                .first()
                .copied()
                .unwrap();
            let multiplier = multipliers.get(table).unwrap();
            format!("{table}.v{neighbor}_mode * {multiplier} as v{neighbor}_mode")
        })
        .join(", ");

    let neighbors = neighbors
        .iter()
        .map(|neighbor| {
            let table = vertex_to_tables.get(neighbor).unwrap().first().unwrap();
            format!("{table}.v{neighbor} as v{neighbor}")
        })
        .join(", ");

    if where_clause.is_empty() {
        format!("select {neighbors}, {new_modes}, {new_count} from {from_clause}")
    } else {
        format!(
            "select {neighbors}, {new_modes}, {new_count} from {from_clause} where {where_clause}"
        )
    }
}

fn build_agg_statement(sql: String, neighbors: &[TagId]) -> String {
    assert!(!neighbors.is_empty());
    let modes = neighbors
        .iter()
        .map(|neighbor| format!("sum(v{neighbor}_mode) as v{neighbor}_mode"))
        .join(", ");
    let neighbors = neighbors.iter().map(|v| format!("v{v}")).join(", ");
    format!("select {neighbors}, {modes}, sum(_count) as _count from ({sql}) group by {neighbors}")
}

fn build_view_statement(sql: String, table_name: &str) -> String {
    format!("create temp view {table_name} as ({sql})")
}

/// Create temporary table for each edge to avoid naming conflict
fn create_temp_table(
    conn: &Connection,
    edge: &CatalogEdge,
    id_generator: &mut RangeFrom<usize>,
) -> GCardResult<String> {
    let table_id = id_generator.next().unwrap();
    let (sql, temp_table_name) = match edge.kind() {
        CatalogEdgeKind::Star { center } => {
            let temp_table_name = format!("temp_star_{}", table_id);
            let original_table_name = if edge.label_id() < LabelId::MAX / 2 {
                format!("star_{}", edge.label_id())
            } else {
                format!("star_{}", LabelId::MAX / 2)
            };
            let sql = format!(
                r"
CREATE TEMP VIEW {temp_table_name} AS (
SELECT
    id AS v{center},
    _mode::double AS v{center}_mode,
    _count::double AS _count
FROM
    {original_table_name}
)"
            );
            (sql, temp_table_name)
        }
        CatalogEdgeKind::Path { src, dst } if src != dst => {
            let temp_table_name = format!("temp_path_{}", table_id);
            let original_table_name = if edge.label_id() < LabelId::MAX / 2 {
                format!("path_{}", edge.label_id())
            } else {
                format!("path_{}", LabelId::MAX / 2)
            };
            let sql = format!(
                r"
CREATE TEMP VIEW {temp_table_name} AS (
SELECT
    s AS v{src},
    t AS v{dst},
    _mode_s::double AS v{src}_mode,
    _mode_t::double AS v{dst}_mode,
    _count::double AS _count
FROM
    {original_table_name}
)"
            );
            (sql, temp_table_name)
        }
        // Handle self-cycle case
        CatalogEdgeKind::Path { src, .. } => {
            let temp_table_name = format!("temp_path_{}", table_id);
            let original_table_name = if edge.label_id() < LabelId::MAX / 2 {
                format!("path_{}", edge.label_id())
            } else {
                format!("path_{}", LabelId::MAX / 2)
            };
            let sql = format!(
                r"
CREATE TEMP VIEW {temp_table_name} AS (
SELECT
    s AS v{src},
    least(_mode_s, _mode_t)::double AS v{src}_mode,
    _count::double AS _count
FROM
    {original_table_name}
WHERE
    s = t
)"
            );
            (sql, temp_table_name)
        }
        _ => unreachable!(),
    };
    execute_sql(conn, &sql)?;
    Ok(temp_table_name)
}

fn execute_sql(conn: &Connection, sql: &str) -> GCardResult<()> {
    trace!("{}", sql);
    Ok(conn.execute_batch(sql)?)
}
