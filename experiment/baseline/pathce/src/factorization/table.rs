use std::collections::HashMap;
use std::sync::Arc;

use super::{ColumnGroup, ColumnRef};
use crate::common::{DefaultVertexId, TagId};

type ColumnPos = (usize, usize);

#[derive(Debug, Clone, Default)]
pub struct Table {
    tag_id_to_column_pos: HashMap<TagId, ColumnPos>,
    groups: Vec<ColumnGroup>,
}

impl Table {
    pub fn tags(&self) -> impl Iterator<Item = TagId> + '_ {
        self.tag_id_to_column_pos.keys().copied()
    }

    pub fn get_column(&self, tag_id: TagId) -> Option<ColumnRef> {
        let (group_id, column_id) = self.tag_id_to_column_pos.get(&tag_id)?;
        self.groups.get(*group_id)?.get_column(*column_id)
    }

    pub fn get_column_pos(&self, tag_id: TagId) -> Option<(usize, usize)> {
        self.tag_id_to_column_pos.get(&tag_id).copied()
    }

    pub fn replace_column(
        &mut self,
        group_id: usize,
        column_id: usize,
        new_column: Arc<Vec<DefaultVertexId>>,
    ) -> Arc<Vec<DefaultVertexId>> {
        let group = self
            .groups
            .get_mut(group_id)
            .expect("group_id should be valid");
        group.replace_column(column_id, new_column)
    }

    pub fn add_column(&mut self, group_id: usize, column: Arc<Vec<DefaultVertexId>>) -> usize {
        let group = self
            .groups
            .get_mut(group_id)
            .expect("group_id should be valid");
        group.add_column(column)
    }

    pub fn add_group(&mut self, group: ColumnGroup) -> usize {
        if !self.groups.is_empty() {
            assert_eq!(self.groups[0].num_items(), group.num_items());
        }
        let index = self.groups.len();
        self.groups.push(group);
        index
    }

    pub fn num_tags(&self) -> usize {
        self.tag_id_to_column_pos.len()
    }

    pub fn num_groups(&self) -> usize {
        self.groups.len()
    }

    pub fn num_items(&self) -> usize {
        if self.groups.is_empty() {
            0
        } else {
            self.groups[0].num_items()
        }
    }

    pub fn add_tag(&mut self, tag_id: TagId, group_id: usize, column_id: usize) {
        let group = self.groups.get(group_id).expect("group_id should be valid");
        assert!(column_id < group.num_columns(), "column_id should be valid");
        assert!(
            self.tag_id_to_column_pos
                .insert(tag_id, (group_id, column_id))
                .is_none(),
            "no duplicate tag_id is allowed"
        )
    }

    pub fn count(&self) -> usize {
        if self.groups.is_empty() {
            return 0;
        }
        let num_items = self.groups[0].num_items();
        (0..num_items)
            .map(|i| {
                self.groups
                    .iter()
                    .map(|group| group.count(i).unwrap())
                    .product::<usize>()
            })
            .sum()
    }
}
