use ahash::HashMap;
use bimap::BiHashMap;

pub type LabelId = u32;
pub type DefaultVertexId = usize;
pub type InternalId = u32;
pub type InternalVertexMap = BiHashMap<DefaultVertexId, InternalId>;
pub type TagId = u8;
pub type BucketId = usize;
pub type GlobalBucketMap = HashMap<LabelId, LocalBucketMap>;
pub type LocalBucketMap = HashMap<DefaultVertexId, BucketId>;

pub const INVALID_TAG_ID: TagId = u8::MAX;

const INVALID_VERTEX_ID: DefaultVertexId = usize::MAX;

pub trait VertexId: Default + Clone + Copy + Send {
    fn invalid() -> Self;
    fn is_valid(&self) -> bool;
}

impl VertexId for DefaultVertexId {
    fn invalid() -> Self {
        INVALID_VERTEX_ID
    }

    fn is_valid(&self) -> bool {
        *self != INVALID_VERTEX_ID
    }
}
