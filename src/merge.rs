//! The merge pushout: given f: O→A and g: O→B, construct M so both
//! branches' edits land exactly once.
//!
//! Survivors are O nodes with images under both diffs; relabels apply
//! to survivors (identical relabels dedupe); inserted branch subtrees
//! graft at their parent's image after the nearest preceding placed
//! sibling, A's before B's in the same slot, equal insertions deduped
//! by subtree hash. Conflict rules refine this construction; the
//! conflict-free path is built here.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::diff::{Matching, Shape, diff, edits};
use crate::error::Error;
use crate::rules;
use crate::tree::{NodeId, Tree};

/// Where a merge node's content comes from: a surviving O node, or a
/// node contributed by one branch (an insertion, or a relabel target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    O(NodeId),
    A(NodeId),
    B(NodeId),
}

/// An index into a [`MergedTree`] arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedId(usize);

#[derive(Debug)]
struct MergedNode {
    origin: Origin,
    children: Vec<MergedId>,
}

/// The merge result: every node tagged with the tree it came from, so
/// synthesis can emit original bytes span by span.
#[derive(Debug)]
pub struct MergedTree {
    nodes: Vec<MergedNode>,
    root: MergedId,
}

impl MergedTree {
    /// The root node.
    pub fn root(&self) -> MergedId {
        self.root
    }

    /// The node's origin tag.
    pub fn origin(&self, id: MergedId) -> Origin {
        self.node(id).origin
    }

    /// The node's children, in merged order.
    pub fn children(&self, id: MergedId) -> &[MergedId] {
        &self.node(id).children
    }

    /// The merged tree's shape, resolving each origin against the tree
    /// it points into.
    pub fn shape(&self, o: &Tree, a: &Tree, b: &Tree) -> Shape {
        self.shape_of(o, a, b, self.root)
    }

    fn shape_of(&self, o: &Tree, a: &Tree, b: &Tree, id: MergedId) -> Shape {
        let (tree, node) = match self.node(id).origin {
            Origin::O(n) => (o, n),
            Origin::A(n) => (a, n),
            Origin::B(n) => (b, n),
        };
        Shape {
            kind_id: tree.kind_id(node),
            label: tree.label(node).map(String::from),
            children: self
                .node(id)
                .children
                .iter()
                .map(|&child| self.shape_of(o, a, b, child))
                .collect(),
        }
    }

    fn node(&self, id: MergedId) -> &MergedNode {
        // MergedIds are arena indices handed out by this tree.
        #[allow(clippy::indexing_slicing)]
        &self.nodes[id.0]
    }
}

/// One merge conflict: the overlapping origin spans and the rule that
/// fired. Spans are `None` when the rule has no witness in that tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub span_o: Option<Range<usize>>,
    pub span_a: Option<Range<usize>>,
    pub span_b: Option<Range<usize>>,
    pub rule: &'static str,
}

/// A merge either produces an origin-tagged tree or a set of conflicts.
#[derive(Debug)]
pub enum MergeOutcome {
    Merged(MergedTree),
    Conflicts(Vec<Conflict>),
}

/// Merges A and B against their common origin O.
pub fn merge(o: &Tree, a: &Tree, b: &Tree) -> Result<MergeOutcome, Error> {
    let f = diff(o, a);
    let g = diff(o, b);

    let ctx = rules::Ctx {
        o,
        a,
        b,
        f: &f,
        g: &g,
    };
    let conflicts = rules::conflicts(&ctx, &edits(o, a, &f), &edits(o, b, &g));
    if !conflicts.is_empty() {
        return Ok(MergeOutcome::Conflicts(conflicts));
    }

    let mut builder = Builder {
        o,
        a,
        b,
        f: &f,
        g: &g,
        nodes: Vec::new(),
        placed: HashSet::new(),
        consumed: HashSet::new(),
    };
    let root = builder.build_survivor(o.root());
    Ok(MergeOutcome::Merged(MergedTree {
        nodes: builder.nodes,
        root,
    }))
}

/// Which branch a graft comes from, with its diff to O and to-M pull
/// direction bundled where needed.
#[derive(Clone, Copy)]
enum Branch {
    A,
    B,
}

/// A graft placed in a slot: its subtree hash for cross-branch dedupe
/// and whether it can still absorb a duplicate B insertion.
struct SlotEntry {
    hash: u64,
    id: MergedId,
    dedupes_b: bool,
}

struct Builder<'t> {
    o: &'t Tree,
    a: &'t Tree,
    b: &'t Tree,
    f: &'t Matching,
    g: &'t Matching,
    nodes: Vec<MergedNode>,
    /// Survivors already materialized at their O position.
    placed: HashSet<NodeId>,
    /// Survivors pulled inside a graft; their O position skips them.
    consumed: HashSet<NodeId>,
}

impl Builder<'_> {
    fn survives(&self, node: NodeId) -> bool {
        self.f.image(node).is_some() && self.g.image(node).is_some()
    }

    /// Materializes a survivor: origin picks the relabeling branch when
    /// one relabeled it (identical relabels dedupe by taking A's), and
    /// children interleave the spliced O base with branch grafts.
    fn build_survivor(&mut self, x: NodeId) -> MergedId {
        self.placed.insert(x);

        // Unwraps guarded by survives(); the root always survives
        // because align always pairs roots.
        let in_a = self.f.image(x);
        let in_b = self.g.image(x);
        let origin = match (in_a, in_b) {
            (Some(ya), _) if self.a.label(ya) != self.o.label(x) => Origin::A(ya),
            (_, Some(yb)) if self.b.label(yb) != self.o.label(x) => Origin::B(yb),
            _ => Origin::O(x),
        };

        // Base: the surviving frontier of x's O children, in O order.
        let base = self.splice(x);
        let base_position: HashMap<NodeId, usize> =
            base.iter().enumerate().map(|(i, &c)| (c, i)).collect();

        // Slots: grafts landing before base[i] live in slots[i]; the
        // final slot holds end-of-list grafts. Each graft records its
        // subtree hash so an identical B insertion in the same slot
        // dedupes against A's.
        let mut slots: Vec<Vec<SlotEntry>> = Vec::new();
        slots.resize_with(base.len().saturating_add(1), Vec::new);

        if let Some(ya) = in_a {
            self.graft_branch(Branch::A, ya, &base_position, &mut slots);
        }
        if let Some(yb) = in_b {
            self.graft_branch(Branch::B, yb, &base_position, &mut slots);
        }

        // Materialize: slot grafts, then the base survivor they precede.
        let mut children: Vec<MergedId> = Vec::new();
        for (i, &c) in base.iter().enumerate() {
            if let Some(slot) = slots.get(i) {
                children.extend(slot.iter().map(|entry| entry.id));
            }
            if !self.consumed.contains(&c) {
                children.push(self.build_survivor(c));
            }
        }
        if let Some(slot) = slots.last() {
            children.extend(slot.iter().map(|entry| entry.id));
        }

        self.push(MergedNode { origin, children })
    }

    /// Walks one branch image's children, grafting insertions into the
    /// slot after the nearest preceding child whose preimage sits in
    /// the base.
    ///
    /// Dedupe is cross-branch only: a B insertion equal (by subtree
    /// hash) to an A insertion in the same slot is the same edit made
    /// twice and lands once, but one branch inserting two equal
    /// subtrees is two real insertions. Each A entry can absorb at
    /// most one B duplicate so multiplicities still add up.
    fn graft_branch(
        &mut self,
        branch: Branch,
        parent_image: NodeId,
        base_position: &HashMap<NodeId, usize>,
        slots: &mut [Vec<SlotEntry>],
    ) {
        let (tree, matching) = match branch {
            Branch::A => (self.a, self.f),
            Branch::B => (self.b, self.g),
        };
        let mut cursor = 0usize;
        for &child in tree.children(parent_image) {
            match matching.preimage(child) {
                Some(c) => {
                    if let Some(&pos) = base_position.get(&c) {
                        cursor = pos.saturating_add(1);
                    }
                }
                None => {
                    let hash = tree.hash(child);
                    if matches!(branch, Branch::B)
                        && let Some(slot) = slots.get_mut(cursor)
                        && let Some(twin) = slot
                            .iter_mut()
                            .find(|entry| entry.dedupes_b && entry.hash == hash)
                    {
                        twin.dedupes_b = false;
                        continue;
                    }
                    let id = self.graft(branch, child);
                    if let Some(slot) = slots.get_mut(cursor) {
                        slot.push(SlotEntry {
                            hash,
                            id,
                            dedupes_b: matches!(branch, Branch::A),
                        });
                    }
                }
            }
        }
    }

    /// Materializes an inserted branch subtree. Matched descendants
    /// whose preimage survives are pulled in (marked consumed so their
    /// O position skips them); already-placed survivors stay where the
    /// O walk put them.
    fn graft(&mut self, branch: Branch, node: NodeId) -> MergedId {
        let (tree, matching) = match branch {
            Branch::A => (self.a, self.f),
            Branch::B => (self.b, self.g),
        };
        let mut children: Vec<MergedId> = Vec::new();
        for &child in tree.children(node) {
            match matching.preimage(child) {
                None => children.push(self.graft(branch, child)),
                Some(c) => {
                    if self.survives(c) && !self.placed.contains(&c) && !self.consumed.contains(&c)
                    {
                        self.consumed.insert(c);
                        children.push(self.build_survivor(c));
                    }
                    // Matched but not a survivor (the other branch
                    // deleted it), or already placed: contributes
                    // nothing here. The insert-delete conflict rule
                    // owns the first case.
                }
            }
        }
        let origin = match branch {
            Branch::A => Origin::A(node),
            Branch::B => Origin::B(node),
        };
        self.push(MergedNode { origin, children })
    }

    /// The surviving frontier of x's children: survivors stay, deleted
    /// interiors are spliced through to their surviving descendants.
    fn splice(&self, x: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for &c in self.o.children(x) {
            if self.survives(c) {
                out.push(c);
            } else {
                out.extend(self.splice(c));
            }
        }
        out
    }

    fn push(&mut self, node: MergedNode) -> MergedId {
        let id = MergedId(self.nodes.len());
        self.nodes.push(node);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;
    use crate::diff::Shape;
    use crate::error::Error;
    use crate::lang::Lang;

    fn parse(src: &str, lang_name: &str) -> Result<Tree, Error> {
        let lang = Lang::by_name(lang_name).ok_or(Error::UnknownLanguage {
            path: lang_name.into(),
        })?;
        Tree::parse(src, lang)
    }

    /// Merges and asserts the outcome is structurally the expected
    /// source, and that the expected source passes the checker.
    fn assert_merges(lang: &str, o: &str, a: &str, b: &str, expected: &str) -> Result<(), Error> {
        let o = parse(o, lang)?;
        let a = parse(a, lang)?;
        let b = parse(b, lang)?;
        let expected = parse(expected, lang)?;
        match merge(&o, &a, &b)? {
            MergeOutcome::Merged(m) => {
                assert_eq!(m.shape(&o, &a, &b), Shape::of(&expected));
            }
            MergeOutcome::Conflicts(conflicts) => {
                panic!("expected a clean merge, got conflicts: {conflicts:?}");
            }
        }
        let report = check::check(&o, &a, &b, &expected);
        assert!(report.is_correct(), "{:?}", report.violations);
        Ok(())
    }

    #[test]
    fn figure_2_3_disjoint_edits_both_land() -> Result<(), Error> {
        assert_merges(
            "json",
            "[1, 2, 3]",
            "[1, 2, 4, 5, 3]",
            "[1, 6, 3]",
            "[1, 6, 4, 5, 3]",
        )
    }

    #[test]
    fn identical_insertions_dedupe() -> Result<(), Error> {
        assert_merges("json", "[1, 2]", "[1, 9, 2]", "[1, 9, 2]", "[1, 9, 2]")
    }

    #[test]
    fn identical_deletions_merge_silently() -> Result<(), Error> {
        assert_merges("json", "[1, 2]", "[1]", "[1]", "[1]")
    }

    #[test]
    fn rename_and_insert_in_different_functions_both_land() -> Result<(), Error> {
        assert_merges(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn c() { x(); }\nfn b() { y(); }",
            "fn a() { x(); }\nfn b() { y(); z(); }",
            "fn c() { x(); }\nfn b() { y(); z(); }",
        )
    }

    #[test]
    fn one_sided_edits_pass_through() -> Result<(), Error> {
        assert_merges("json", "[1, 2]", "[1, 2, 3]", "[1, 2]", "[1, 2, 3]")?;
        assert_merges("json", "[1, 2]", "[1, 2]", "[2]", "[2]")
    }

    fn assert_conflicts(
        lang: &str,
        o: &str,
        a: &str,
        b: &str,
        rule: &str,
    ) -> Result<Vec<Conflict>, Error> {
        let o = parse(o, lang)?;
        let a = parse(a, lang)?;
        let b = parse(b, lang)?;
        match merge(&o, &a, &b)? {
            MergeOutcome::Merged(m) => {
                panic!("expected conflicts, got a merge: {:?}", m.shape(&o, &a, &b));
            }
            MergeOutcome::Conflicts(conflicts) => {
                assert!(
                    conflicts.iter().any(|c| c.rule == rule),
                    "expected a {rule} conflict: {conflicts:?}"
                );
                Ok(conflicts)
            }
        }
    }

    #[test]
    fn conflicting_renames_conflict() -> Result<(), Error> {
        let conflicts = assert_conflicts(
            "rust",
            "fn a() {}",
            "fn b() {}",
            "fn c() {}",
            "relabel-relabel",
        )?;
        let a = parse("fn b() {}", "rust")?;
        let witnessed = conflicts.iter().any(|c| {
            c.span_a
                .clone()
                .and_then(|span| a.source_slice(span))
                .is_some_and(|text| text == "b")
        });
        assert!(witnessed);
        Ok(())
    }

    #[test]
    fn identical_renames_still_merge() -> Result<(), Error> {
        assert_merges("rust", "fn a() {}", "fn b() {}", "fn b() {}", "fn b() {}")
    }

    #[test]
    fn conflicting_insertions_at_one_slot_conflict() -> Result<(), Error> {
        assert_conflicts(
            "rust",
            "fn f() { x(); }",
            "fn f() { x(); y(); }",
            "fn f() { x(); z(); }",
            "insert-insert",
        )?;
        Ok(())
    }

    #[test]
    fn different_value_insertions_at_one_slot_conflict() -> Result<(), Error> {
        assert_conflicts("json", "[1]", "[1, 9]", "[1, 8]", "insert-insert")?;
        Ok(())
    }

    #[test]
    fn overlapping_deletions_conflict() -> Result<(), Error> {
        // The paper's f(c) example: A rewrites f(c) to x = c, deleting
        // the call wrapper; B deletes the whole statement. The deletion
        // regions overlap without coinciding.
        assert_conflicts(
            "rust",
            "fn main() { f(c); }",
            "fn main() { x = c; }",
            "fn main() { }",
            "delete-delete",
        )?;
        Ok(())
    }

    #[test]
    fn insert_under_a_deleted_node_conflicts() -> Result<(), Error> {
        // A grows fn a's body; B deletes fn a outright.
        assert_conflicts(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); z(); }\nfn b() { y(); }",
            "fn b() { y(); }",
            "insert-delete",
        )?;
        Ok(())
    }

    #[test]
    fn insert_into_a_deleted_inner_class_conflicts() -> Result<(), Error> {
        assert_conflicts(
            "java",
            "class O { class I { } }",
            "class O { class I { void m() { } } }",
            "class O { }",
            "insert-delete",
        )?;
        Ok(())
    }

    #[test]
    fn insert_and_delete_in_different_functions_merge() -> Result<(), Error> {
        assert_merges(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); z(); }\nfn b() { y(); }",
            "fn a() { x(); }",
            "fn a() { x(); z(); }",
        )
    }

    #[test]
    fn a_wrap_pulls_the_survivor_into_the_graft() -> Result<(), Error> {
        // A wraps the array one level deeper; B leaves it alone.
        assert_merges(
            "json",
            "{\"k\": [1, 2]}",
            "{\"k\": [[1, 2]]}",
            "{\"k\": [1, 2]}",
            "{\"k\": [[1, 2]]}",
        )
    }
}
