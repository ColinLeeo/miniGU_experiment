use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EdgeDirection {
    Out,
    In,
}

impl EdgeDirection {
    pub fn reverse(self) -> Self {
        match self {
            EdgeDirection::Out => EdgeDirection::In,
            EdgeDirection::In => EdgeDirection::Out,
        }
    }
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum EdgeCardinality {
    #[default]
    ManyToMany,

    ManyToOne,
    OneToMany,
    OneToOne,
}
