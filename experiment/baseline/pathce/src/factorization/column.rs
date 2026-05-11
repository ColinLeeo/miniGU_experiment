use std::sync::Arc;
use std::{mem, slice};

use itertools::{Itertools, TupleWindows};
use rayon::iter::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator, ParallelExtend,
    ParallelIterator,
};
use rayon_scan::ScanParallelIterator;

use crate::common::{DefaultVertexId, VertexId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRef {
    offsets: Arc<Offsets>,
    values: Arc<Vec<DefaultVertexId>>,
}

impl ColumnRef {
    pub fn values(&self) -> &[DefaultVertexId] {
        &self.values
    }

    pub fn num_items(&self) -> usize {
        match self.offsets.as_ref() {
            Offsets::Single => self.values.len(),
            Offsets::Multiple(offsets) => offsets.len() - 1,
        }
    }

    pub fn num_values(&self) -> usize {
        self.values.len()
    }

    pub fn count_valid(&self, index: usize) -> Option<usize> {
        match self.offsets.as_ref() {
            Offsets::Single => {
                let value = &self.values.get(index)?;
                if value.is_valid() {
                    Some(1)
                } else {
                    Some(0)
                }
            }
            Offsets::Multiple(offsets) => {
                let start = *offsets.get(index)?;
                let end = *offsets.get(index + 1)?;
                Some(
                    self.values[start..end]
                        .iter()
                        .filter(|value| value.is_valid())
                        .count(),
                )
            }
        }
    }

    pub fn par_count_valid(&self, index: usize) -> Option<usize> {
        match self.offsets.as_ref() {
            Offsets::Single => {
                let value = &self.values.get(index)?;
                if value.is_valid() {
                    Some(1)
                } else {
                    Some(0)
                }
            }
            Offsets::Multiple(offsets) => {
                let start = *offsets.get(index)?;
                let end = *offsets.get(index + 1)?;
                Some(
                    self.values[start..end]
                        .par_iter()
                        .filter(|value| value.is_valid())
                        .count(),
                )
            }
        }
    }

    pub fn get_item(&self, index: usize) -> Option<&[DefaultVertexId]> {
        Some(match self.offsets.as_ref() {
            Offsets::Single => {
                let value = &self.values.get(index)?;
                slice::from_ref(value)
            }
            Offsets::Multiple(offsets) => {
                let start = *offsets.get(index)?;
                let end = *offsets.get(index + 1)?;
                &self.values[start..end]
            }
        })
    }

    // unsafe fn get_item_unchecked(&self, index: usize) -> &[DefaultVertexId] {
    //     match self.offsets.as_ref() {
    //         Offsets::Single => {
    //             let value = &self.values.get_unchecked(index);
    //             slice::from_ref(value)
    //         }
    //         Offsets::Multiple(offsets) => {
    //             let start = *offsets.get_unchecked(index);
    //             let end = *offsets.get_unchecked(index + 1);
    //             &self.values[start..end]
    //         }
    //     }
    // }

    pub fn items(&self) -> Items<'_> {
        let inner = match self.offsets.as_ref() {
            Offsets::Single => ItemsInner::Single(self.values.iter()),
            Offsets::Multiple(offsets) => {
                let offsets_iter = offsets.iter().tuple_windows();
                let values = &self.values;
                ItemsInner::Multiple {
                    offsets_iter,
                    values,
                }
            }
        };
        Items(inner)
    }
}

#[derive(Debug, Clone)]
enum ItemsInner<'a> {
    Single(slice::Iter<'a, DefaultVertexId>),
    Multiple {
        offsets_iter: TupleWindows<slice::Iter<'a, usize>, (&'a usize, &'a usize)>,
        values: &'a [DefaultVertexId],
    },
}

#[derive(Debug, Clone)]
pub struct Items<'a>(ItemsInner<'a>);

impl<'a> Iterator for Items<'a> {
    type Item = &'a [DefaultVertexId];

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            ItemsInner::Single(iter) => iter.next().map(slice::from_ref),
            ItemsInner::Multiple {
                offsets_iter,
                values,
            } => {
                let (start, end) = offsets_iter.next()?;
                Some(&values[*start..*end])
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleColumnGroup {
    offsets: Offsets,
    values: Vec<DefaultVertexId>,
}

impl SingleColumnGroup {
    pub fn single() -> Self {
        Self {
            offsets: Offsets::Single,
            values: vec![],
        }
    }

    pub fn multiple() -> Self {
        Self {
            offsets: Offsets::Multiple(vec![0]),
            values: vec![],
        }
    }

    pub fn num_items(&self) -> usize {
        match &self.offsets {
            Offsets::Single => self.values.len(),
            Offsets::Multiple(offsets) => offsets.len() - 1,
        }
    }

    pub fn num_values(&self) -> usize {
        self.values.len()
    }

    pub fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = DefaultVertexId>,
    {
        self.values.extend(iter);
        if let Offsets::Multiple(offsets) = &mut self.offsets {
            offsets.push(self.values.len());
        }
    }

    pub fn par_extend<I>(&mut self, iter: I)
    where
        I: IntoParallelIterator<Item = DefaultVertexId>,
    {
        self.values.par_extend(iter);
        if let Offsets::Multiple(offsets) = &mut self.offsets {
            offsets.push(self.values.len());
        }
    }

    pub fn extend_from_slice(&mut self, values: &[DefaultVertexId]) {
        self.values.extend_from_slice(values);
        if let Offsets::Multiple(offsets) = &mut self.offsets {
            offsets.push(self.values.len());
        }
    }

    pub fn extend_one(&mut self, value: DefaultVertexId) {
        self.extend(Some(value))
    }

    pub fn par_extend_from_segments<'a, I>(&mut self, segments: I)
    where
        I: IndexedParallelIterator<Item = &'a [DefaultVertexId]> + Clone,
    {
        if let Offsets::Multiple(offsets) = &mut self.offsets {
            let init_len = self.values.len();
            offsets.par_extend(
                segments
                    .clone()
                    .into_par_iter()
                    .with_min_len(8192)
                    .map(|segment| segment.len())
                    .scan(|a, b| *a + *b, init_len),
            );
        }
        self.values.par_extend(
            segments
                .into_par_iter()
                .with_min_len(32)
                .flat_map(|segment| segment.par_iter().with_min_len(8192)),
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Offsets {
    Single,
    Multiple(Vec<usize>),
}

#[derive(Debug, Clone)]
pub struct ColumnGroup {
    offsets: Arc<Offsets>,
    columns: Vec<Arc<Vec<DefaultVertexId>>>,
}

impl ColumnGroup {
    pub fn count(&self, item_id: usize) -> Option<usize> {
        if item_id >= self.num_items() {
            return None;
        }
        let count = match self.offsets.as_ref() {
            Offsets::Single => 1,
            Offsets::Multiple(offsets) => offsets[item_id + 1] - offsets[item_id],
        };
        Some(count)
    }

    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    pub fn num_items(&self) -> usize {
        match self.offsets.as_ref() {
            Offsets::Single => self.columns[0].len(),
            Offsets::Multiple(offsets) => offsets.len() - 1,
        }
    }

    pub fn num_values(&self) -> usize {
        self.columns[0].len()
    }

    pub fn add_column(&mut self, column: Arc<Vec<DefaultVertexId>>) -> usize {
        assert_eq!(self.columns[0].len(), column.len());
        let index = self.columns.len();
        self.columns.push(column);
        index
    }

    pub fn get_column(&self, index: usize) -> Option<ColumnRef> {
        let offsets = self.offsets.clone();
        let values = self.columns.get(index)?.clone();
        Some(ColumnRef { offsets, values })
    }

    pub fn replace_column(
        &mut self,
        index: usize,
        new_column: Arc<Vec<DefaultVertexId>>,
    ) -> Arc<Vec<DefaultVertexId>> {
        mem::replace(self.columns.get_mut(index).unwrap(), new_column)
    }
}

impl From<SingleColumnGroup> for ColumnGroup {
    fn from(SingleColumnGroup { offsets, values }: SingleColumnGroup) -> Self {
        let offsets = Arc::new(offsets);
        let columns = vec![Arc::new(values)];
        Self { offsets, columns }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_column_group() {
        let mut group = SingleColumnGroup::single();
        group.extend([1, 2, 3]);
        group.extend([4, 5, 6]);
        assert_eq!(group.num_items(), 6);
        assert_eq!(group.num_values(), 6);

        let mut group = SingleColumnGroup::multiple();
        group.extend([1, 2, 3]);
        group.extend([4, 5, 6]);
        assert_eq!(group.num_items(), 2);
        assert_eq!(group.num_values(), 6);
    }

    #[test]
    fn test_column_items() {
        let mut group = SingleColumnGroup::single();
        group.extend([1, 2, 3]);
        group.extend([4, 5, 6]);

        let group = ColumnGroup::from(group);
        let col1 = group.get_column(0).unwrap();
        let items = col1.items().collect_vec();
        assert_eq!(items, vec![&[1], &[2], &[3], &[4], &[5], &[6]]);
        assert_eq!(col1.get_item(3).unwrap(), &[4]);

        let mut group = SingleColumnGroup::multiple();
        group.extend([1, 2, 3]);
        group.extend([4, 5, 6]);

        let group = ColumnGroup::from(group);
        let col1 = group.get_column(0).unwrap();
        let items = col1.items().collect_vec();
        assert_eq!(items, vec![&[1, 2, 3], &[4, 5, 6]]);
        assert_eq!(col1.get_item(1).unwrap(), &[4, 5, 6]);
    }
}
