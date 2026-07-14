use crate::tree::{NodeId, Tree};

/// A matching between two trees: the order-preserving partial inclusion
/// map from the design doc. This is the diff's first-class output; edit
/// scripts are derived views.
///
/// Bijectivity is structural — [`Matching::insert`] unlinks whatever
/// pairing either endpoint had — so [`Matching::validate`] checks the
/// remaining invariants: matched pairs share a kind, ancestry is
/// preserved, and sibling order never crosses.
#[derive(Debug)]
pub struct Matching {
    src_to_dst: Vec<Option<NodeId>>,
    dst_to_src: Vec<Option<NodeId>>,
}

/// A way a [`Matching`] can break its invariants.
///
/// Produced only by [`Matching::validate`], which is test and
/// debug-assert machinery: the diff phases rely on constructing
/// matchings correctly, not on validating them at merge time.
#[derive(Debug, thiserror::Error)]
pub enum InvariantViolation {
    #[error("matched nodes differ in kind: src {src:?} vs dst {dst:?}")]
    Kind { src: NodeId, dst: NodeId },

    #[error(
        "ancestry inverted: src {ancestor:?} contains {descendant:?} \
         but their images do not nest"
    )]
    Ancestry {
        ancestor: NodeId,
        descendant: NodeId,
    },

    #[error("sibling order crossed between src {first:?} and {second:?}")]
    Order { first: NodeId, second: NodeId },
}

impl Matching {
    /// An empty matching sized for `src` and `dst`.
    pub fn new(src: &Tree, dst: &Tree) -> Self {
        Self {
            src_to_dst: vec![None; src.nodes().count()],
            dst_to_src: vec![None; dst.nodes().count()],
        }
    }

    /// Pairs `src` with `dst`, unlinking any pairing either node had.
    pub fn insert(&mut self, src: NodeId, dst: NodeId) {
        if let Some(old_dst) = self.image(src) {
            self.set_preimage(old_dst, None);
        }
        if let Some(old_src) = self.preimage(dst) {
            self.set_image(old_src, None);
        }
        self.set_image(src, Some(dst));
        self.set_preimage(dst, Some(src));
    }

    /// The dst node `src` is matched to, if any.
    pub fn image(&self, src: NodeId) -> Option<NodeId> {
        // Ids index the tree this side was sized for; out-of-range
        // means ids were mixed across trees — a logic bug.
        #[allow(clippy::indexing_slicing)]
        self.src_to_dst[src.index()]
    }

    /// The src node matched to `dst`, if any.
    pub fn preimage(&self, dst: NodeId) -> Option<NodeId> {
        #[allow(clippy::indexing_slicing)]
        self.dst_to_src[dst.index()]
    }

    /// All matched pairs, in src pre-order.
    pub fn pairs(&self) -> impl Iterator<Item = (NodeId, NodeId)> + '_ {
        self.src_to_dst
            .iter()
            .enumerate()
            .filter_map(|(i, dst)| dst.map(|dst| (NodeId::from_index(i), dst)))
    }

    /// Checks the matching invariants against the trees it was built
    /// from. Test and debug-assert machinery, not a merge-path check.
    pub fn validate(&self, src: &Tree, dst: &Tree) -> Result<(), InvariantViolation> {
        // Invariant 2: matched pairs share a kind.
        for (s, d) in self.pairs() {
            if src.kind_id(s) != dst.kind_id(d) {
                return Err(InvariantViolation::Kind { src: s, dst: d });
            }
        }

        // Invariant 3: every matched proper ancestor's image is a
        // proper ancestor of the image.
        for (s, d) in self.pairs() {
            let mut up = src.parent(s);
            while let Some(ancestor) = up {
                if let Some(ancestor_image) = self.image(ancestor)
                    && !is_ancestor(dst, ancestor_image, d)
                {
                    return Err(InvariantViolation::Ancestry {
                        ancestor,
                        descendant: s,
                    });
                }
                up = src.parent(ancestor);
            }
        }

        // Invariant 4: matched same-parent siblings keep their order
        // in the destination's document order (pre-order indices).
        for parent in src.nodes() {
            let mut prev: Option<(NodeId, NodeId)> = None;
            for &child in src.children(parent) {
                let Some(child_image) = self.image(child) else {
                    continue;
                };
                if let Some((prev_child, prev_image)) = prev
                    && child_image.index() <= prev_image.index()
                {
                    return Err(InvariantViolation::Order {
                        first: prev_child,
                        second: child,
                    });
                }
                prev = Some((child, child_image));
            }
        }

        Ok(())
    }

    fn set_image(&mut self, src: NodeId, dst: Option<NodeId>) {
        #[allow(clippy::indexing_slicing)]
        {
            self.src_to_dst[src.index()] = dst;
        }
    }

    fn set_preimage(&mut self, dst: NodeId, src: Option<NodeId>) {
        #[allow(clippy::indexing_slicing)]
        {
            self.dst_to_src[dst.index()] = src;
        }
    }
}

/// Whether `ancestor` is a proper ancestor of `node`.
fn is_ancestor(tree: &Tree, ancestor: NodeId, node: NodeId) -> bool {
    let mut up = tree.parent(node);
    while let Some(current) = up {
        if current == ancestor {
            return true;
        }
        up = tree.parent(current);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::lang::Lang;
    use crate::tree::Tree;

    fn parse_json(src: &str) -> Result<Tree, Error> {
        let lang = Lang::by_name("json").ok_or(Error::UnknownLanguage {
            path: "json".into(),
        })?;
        Tree::parse(src, lang)
    }

    /// Pairs nodes positionally; only meaningful when both trees parse
    /// the same source (identical shape).
    fn identity(src: &Tree, dst: &Tree) -> Matching {
        let mut m = Matching::new(src, dst);
        for (s, d) in src.nodes().zip(dst.nodes()) {
            m.insert(s, d);
        }
        m
    }

    fn find_number(t: &Tree, label: &str) -> Option<NodeId> {
        t.nodes()
            .find(|&n| t.kind(n) == "number" && t.label(n) == Some(label))
    }

    fn find_kind(t: &Tree, kind: &str) -> Vec<NodeId> {
        t.nodes().filter(|&n| t.kind(n) == kind).collect()
    }

    #[test]
    fn accepts_an_order_preserving_map() -> Result<(), Error> {
        let o = parse_json("[1, 2, 3]")?;
        let a = parse_json("[1, 2, 3]")?;
        let m = identity(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        Ok(())
    }

    #[test]
    fn accepts_a_partial_map() -> Result<(), Error> {
        let o = parse_json("[1, 2, 3]")?;
        let a = parse_json("[1, 9, 3]")?;
        let mut m = Matching::new(&o, &a);
        for label in ["1", "3"] {
            if let (Some(s), Some(d)) = (find_number(&o, label), find_number(&a, label)) {
                m.insert(s, d);
            }
        }
        assert_eq!(m.pairs().count(), 2);
        assert!(m.validate(&o, &a).is_ok());
        Ok(())
    }

    #[test]
    fn rejects_a_crossing_map() -> Result<(), Error> {
        // O and A both hold [1, 2]; matching 1↔1 and 2↔2 across
        // O=[1,2], A=[2,1] crosses sibling order.
        let o = parse_json("[1, 2]")?;
        let a = parse_json("[2, 1]")?;
        let mut m = Matching::new(&o, &a);
        for label in ["1", "2"] {
            if let (Some(s), Some(d)) = (find_number(&o, label), find_number(&a, label)) {
                m.insert(s, d);
            }
        }
        assert!(matches!(
            m.validate(&o, &a),
            Err(InvariantViolation::Order { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_a_kind_mismatch() -> Result<(), Error> {
        let o = parse_json("[1]")?;
        let a = parse_json("[[1]]")?;
        let mut m = Matching::new(&o, &a);
        let number = find_number(&o, "1");
        let inner_array = find_kind(&a, "array").pop();
        if let (Some(s), Some(d)) = (number, inner_array) {
            m.insert(s, d);
        }
        assert!(matches!(
            m.validate(&o, &a),
            Err(InvariantViolation::Kind { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_inverted_ancestry() -> Result<(), Error> {
        // Match O's outer array to A's inner and vice versa.
        let o = parse_json("[[1]]")?;
        let a = parse_json("[[1]]")?;
        let o_arrays = find_kind(&o, "array");
        let a_arrays = find_kind(&a, "array");
        let mut m = Matching::new(&o, &a);
        if let (Some(&o_outer), Some(&o_inner), Some(&a_outer), Some(&a_inner)) = (
            o_arrays.first(),
            o_arrays.last(),
            a_arrays.first(),
            a_arrays.last(),
        ) {
            m.insert(o_outer, a_inner);
            m.insert(o_inner, a_outer);
        }
        assert!(matches!(
            m.validate(&o, &a),
            Err(InvariantViolation::Ancestry { .. })
        ));
        Ok(())
    }

    #[test]
    fn insert_relinks_stale_pairs() -> Result<(), Error> {
        // Bijectivity is structural: re-inserting either endpoint
        // unlinks the pairing it replaces.
        let o = parse_json("[1, 2]")?;
        let a = parse_json("[1, 2]")?;
        let (Some(o1), Some(o2)) = (find_number(&o, "1"), find_number(&o, "2")) else {
            unreachable!("[1, 2] holds numbers 1 and 2");
        };
        let (Some(a1), Some(a2)) = (find_number(&a, "1"), find_number(&a, "2")) else {
            unreachable!("[1, 2] holds numbers 1 and 2");
        };

        let mut m = Matching::new(&o, &a);
        m.insert(o1, a1);
        m.insert(o1, a2); // replaces o1's image
        assert_eq!(m.image(o1), Some(a2));
        assert_eq!(m.preimage(a1), None);

        m.insert(o2, a2); // steals a2 from o1
        assert_eq!(m.image(o2), Some(a2));
        assert_eq!(m.image(o1), None);
        assert_eq!(m.preimage(a2), Some(o2));
        Ok(())
    }

    #[test]
    fn image_and_preimage_report_unmatched_nodes() -> Result<(), Error> {
        let o = parse_json("[1]")?;
        let a = parse_json("[1]")?;
        let m = Matching::new(&o, &a);
        assert_eq!(m.image(o.root()), None);
        assert_eq!(m.preimage(a.root()), None);
        assert_eq!(m.pairs().count(), 0);
        Ok(())
    }
}
