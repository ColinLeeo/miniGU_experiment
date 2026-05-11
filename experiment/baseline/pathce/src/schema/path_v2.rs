use super::Schema;
use crate::pattern::{PathPattern, PatternEdge, PatternVertex, RawPattern};

#[derive(Debug, Clone)]
pub struct PathTree(Vec<PathTreeNode>);

impl PathTree {
    pub fn root(&self) -> PathTreeNodeRef {
        let tree = self;
        let node = self.0.first().unwrap();
        PathTreeNodeRef { tree, node }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone)]
struct PathTreeNode {
    id: usize,
    path: PathPattern,
    children: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct PathTreeNodeRef<'a> {
    tree: &'a PathTree,
    node: &'a PathTreeNode,
}

impl<'a> PathTreeNodeRef<'a> {
    pub fn id(&self) -> usize {
        self.node.id
    }

    pub fn path(&self) -> &PathPattern {
        &self.node.path
    }

    pub fn children(&self) -> Vec<PathTreeNodeRef<'a>> {
        self.node
            .children
            .iter()
            .map(|id| {
                let node = self.tree.0.get(*id).unwrap();
                PathTreeNodeRef { node, ..*self }
            })
            .collect()
    }
}

impl Schema {
    pub fn generate_path_tree_from_path_end(
        &self,
        path: &PathPattern,
        max_depth: usize,
    ) -> PathTree {
        let mut nodes = vec![PathTreeNode {
            id: 0,
            path: path.clone(),
            children: vec![],
        }];
        self.generate_path_tree_from_path_end_recursive(path, max_depth, &mut nodes, 0);
        PathTree(nodes)
    }

    fn generate_path_tree_from_path_end_recursive(
        &self,
        path: &PathPattern,
        max_depth: usize,
        nodes: &mut Vec<PathTreeNode>,
        root: usize,
    ) {
        if max_depth == 0 {
            return;
        }
        let path_end = path.end();
        let raw = RawPattern::from(path);
        for e in self.outgoing_edges(path_end.label_id()).unwrap() {
            let next_edge_tag_id = raw.next_edge_tag_id();
            let next_vertex_tag_id = raw.next_vertex_tag_id();
            let new_path_end_label = e.to;
            let new_path = raw
                .clone()
                .push_back_vertex(PatternVertex::new(next_vertex_tag_id, new_path_end_label))
                .push_back_edge(PatternEdge::new(
                    next_edge_tag_id,
                    path_end.tag_id(),
                    next_vertex_tag_id,
                    e.label,
                ))
                .to_path()
                .unwrap();
            let next_node_id = nodes.len();
            let node = PathTreeNode {
                id: next_node_id,
                path: new_path.clone(),
                children: vec![],
            };
            let root_node = nodes.get_mut(root).unwrap();
            root_node.children.push(next_node_id);
            nodes.push(node);

            self.generate_path_tree_from_path_end_recursive(
                &new_path,
                max_depth - 1,
                nodes,
                next_node_id,
            );
        }

        for e in self.incoming_edges(path_end.label_id()).unwrap() {
            let next_edge_tag_id = raw.next_edge_tag_id();
            let next_vertex_tag_id = raw.next_vertex_tag_id();
            let new_path_end_label = e.from;
            let new_path = raw
                .clone()
                .push_back_vertex(PatternVertex::new(next_vertex_tag_id, new_path_end_label))
                .push_back_edge(PatternEdge::new(
                    next_edge_tag_id,
                    next_vertex_tag_id,
                    path_end.tag_id(),
                    e.label,
                ))
                .to_path()
                .unwrap();
            let next_node_id = nodes.len();
            let node = PathTreeNode {
                id: next_node_id,
                path: new_path.clone(),
                children: vec![],
            };
            let root_node = nodes.get_mut(root).unwrap();
            root_node.children.push(next_node_id);
            nodes.push(node);

            self.generate_path_tree_from_path_end_recursive(
                &new_path,
                max_depth - 1,
                nodes,
                next_node_id,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::build_ldbc_schema;

    #[test]
    fn test_generate_path_tree() {
        let schema = build_ldbc_schema();
        let base = RawPattern::new()
            .push_back_vertex(PatternVertex::new(0, 6))
            .to_path()
            .unwrap();
        let tree = schema.generate_path_tree_from_path_end(&base, 1);
        assert_eq!(tree.len(), 13);
    }
}
