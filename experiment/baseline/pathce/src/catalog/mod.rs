mod duck;
mod mock;

pub use duck::DuckCatalog;
pub use mock::MockCatalog;

use crate::common::{LabelId, TagId};
use crate::pattern::{encode_edge, encode_vertex, GeneralPattern, PathPattern};

pub trait Catalog {
    fn get_path_label_id(&self, code: &[u8]) -> Option<LabelId>;
    fn get_path(&self, label_id: LabelId) -> Option<&PathPattern>;
    fn get_star_label_id(&self, rank: TagId, code: &[u8]) -> Option<LabelId>;
    fn get_star(&self, label_id: LabelId) -> Option<&GeneralPattern>;
    fn get_edge_count(&self, label_id: LabelId) -> Option<usize>;

    fn get_edge_label_id(
        &self,
        src_label_id: LabelId,
        dst_label_id: LabelId,
        edge_label_id: LabelId,
    ) -> Option<LabelId> {
        let code = encode_edge(src_label_id, dst_label_id, edge_label_id);
        self.get_path_label_id(&code)
    }

    fn get_vertex_label_id(&self, vertex: LabelId) -> Option<LabelId> {
        let code = encode_vertex(vertex);
        self.get_star_label_id(0, &code)
    }
}
