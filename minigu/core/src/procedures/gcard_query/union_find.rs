//! 简单并查集实现。
//!
//! 主要用于查询图里的生成树构造、环检测和连通性判断。

use std::collections::HashMap;

use minigu_common::types::VertexId;

pub struct UnionFind {
    /// 每个元素当前指向的父节点。
    parent: HashMap<VertexId, VertexId>,
    /// 按秩合并时使用的秩。
    rank: HashMap<VertexId, usize>,
}

impl UnionFind {
    pub fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
        }
    }

    pub fn make_set(&mut self, x: VertexId) {
        if !self.parent.contains_key(&x) {
            self.parent.insert(x, x);
            self.rank.insert(x, 0);
        }
    }

    pub fn find(&mut self, x: VertexId) -> VertexId {
        // 路径压缩：查找根的同时把路径上的点都直接挂到根上。
        if let Some(&parent) = self.parent.get(&x) {
            if parent != x {
                let root = self.find(parent);
                self.parent.insert(x, root);
                return root;
            }
        } else {
            self.make_set(x);
        }
        x
    }

    pub fn union(&mut self, x: VertexId, y: VertexId) -> bool {
        // 返回值语义很适合做环检测：
        // `false` 表示两个点原本就连通，再合并就会成环。
        let root_x = self.find(x);
        let root_y = self.find(y);

        if root_x == root_y {
            return false;
        }

        let rank_x = *self.rank.get(&root_x).unwrap_or(&0);
        let rank_y = *self.rank.get(&root_y).unwrap_or(&0);

        if rank_x < rank_y {
            self.parent.insert(root_x, root_y);
        } else if rank_x > rank_y {
            self.parent.insert(root_y, root_x);
        } else {
            self.parent.insert(root_y, root_x);
            *self.rank.entry(root_x).or_insert(0) += 1;
        }

        true
    }
}
