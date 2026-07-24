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

use crate::diff::{Edit, Matching, Shape, diff, edits};
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

/// A finished merge: the output text, or the conflicts that stopped it.
#[derive(Debug)]
pub enum MergeResult {
    Merged(String),
    Conflicts(Vec<Conflict>),
}

/// Merges A and B against O all the way to output text, self-checking
/// the result before handing it over: the synthesized text must
/// re-parse cleanly and the re-parsed tree must pass the universality
/// checker against the inputs.
///
/// A self-check failure means a d3j bug, not a user problem — it is
/// logged as an error and surfaced as a `self-check` pseudo-conflict
/// so the user still gets a safe (conflicted) outcome instead of a
/// silently wrong merge.
pub fn merge_to_text(o: &Tree, a: &Tree, b: &Tree) -> Result<MergeResult, Error> {
    merge_to_text_with(o, a, b, crate::synth::synthesize)
}

/// [`merge_to_text`] with the synthesis step injectable, so tests can
/// feed the guard defective output.
fn merge_to_text_with(
    o: &Tree,
    a: &Tree,
    b: &Tree,
    synthesizer: impl Fn(&MergedTree, &Tree, &Tree, &Tree) -> String,
) -> Result<MergeResult, Error> {
    let merged = match merge(o, a, b)? {
        MergeOutcome::Merged(merged) => merged,
        MergeOutcome::Conflicts(conflicts) => return Ok(MergeResult::Conflicts(conflicts)),
    };
    let text = synthesizer(&merged, o, a, b);

    let reparsed = match Tree::parse(&text, o.lang()) {
        Ok(tree) => tree,
        Err(_) => {
            tracing::error!("self-check failed: synthesized merge does not re-parse");
            return Ok(MergeResult::Conflicts(vec![Conflict {
                span_o: None,
                span_a: None,
                span_b: None,
                rule: "self-check",
            }]));
        }
    };

    let report = crate::check::check(o, a, b, &reparsed);
    if !report.is_correct() {
        tracing::error!(
            violations = ?report.violations,
            "self-check failed: merged output violates universality"
        );
        let conflicts = report
            .violations
            .iter()
            .map(|violation| {
                use crate::check::Violation;
                let (span_o, span_a, span_b) = match violation {
                    Violation::ExtraDeletion { span, .. }
                    | Violation::MissedDeletion { span, .. } => (Some(span.clone()), None, None),
                    Violation::MissedInsertion {
                        branch: crate::check::Branch::A,
                        span,
                        ..
                    }
                    | Violation::MissedRelabel {
                        branch: crate::check::Branch::A,
                        span,
                        ..
                    } => (None, Some(span.clone()), None),
                    Violation::MissedInsertion {
                        branch: crate::check::Branch::B,
                        span,
                        ..
                    }
                    | Violation::MissedRelabel {
                        branch: crate::check::Branch::B,
                        span,
                        ..
                    } => (None, None, Some(span.clone())),
                    // The witness lives in M, which has no span slot.
                    Violation::ExtraInsertion { .. } | Violation::ExtraRelabel { .. } => {
                        (None, None, None)
                    }
                };
                Conflict {
                    span_o,
                    span_a,
                    span_b,
                    rule: "self-check",
                }
            })
            .collect();
        return Ok(MergeResult::Conflicts(conflicts));
    }

    Ok(MergeResult::Merged(text))
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
    let edits_a = edits(o, a, &f);
    let edits_b = edits(o, b, &g);
    let conflicts = rules::conflicts(&ctx, &edits_a, &edits_b);
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
    let mut merged = MergedTree {
        nodes: builder.nodes,
        root,
    };

    // The grammar backstop: individually clean edits can still combine
    // into a structurally invalid tree.
    let arity_conflicts = rules::arity(o, a, b, &merged);
    if !arity_conflicts.is_empty() {
        return Ok(MergeOutcome::Conflicts(arity_conflicts));
    }

    prefer_reformatted_origins(&mut merged, o, (a, &f, &edits_a), (b, &g, &edits_b));

    Ok(MergeOutcome::Merged(merged))
}

/// Adopts a reformat-only branch's layout. A branch that only changed
/// trivia is invisible to the pushout — its tree is isomorphic to O —
/// so every survivor keeps its O origin and synthesis emits O's stale
/// formatting, silently discarding the reformat. Remapping surviving
/// O origins to that branch's image makes synthesis emit the
/// reformatted bytes instead. When both branches are edit-free, A
/// wins if it reformatted at all: span synthesis cannot honor two
/// different reformats of the same code at once.
fn prefer_reformatted_origins(
    merged: &mut MergedTree,
    o: &Tree,
    a: (&Tree, &Matching, &[Edit]),
    b: (&Tree, &Matching, &[Edit]),
) {
    fn text(tree: &Tree) -> Option<&str> {
        tree.source_slice(0..tree.source_len())
    }
    let reformatted = |(branch, _, edits): (&Tree, &Matching, &[Edit])| {
        edits.is_empty() && text(branch) != text(o)
    };
    let (matching, tag): (&Matching, fn(NodeId) -> Origin) = if reformatted(a) {
        (a.1, Origin::A)
    } else if reformatted(b) {
        (b.1, Origin::B)
    } else {
        return;
    };
    for node in &mut merged.nodes {
        // An empty edit script means the matching is total, so every
        // surviving O node has an image to adopt.
        if let Origin::O(n) = node.origin
            && let Some(image) = matching.image(n)
        {
            node.origin = tag(image);
        }
    }
}

/// Which branch a graft comes from, with its diff to O and to-M pull
/// direction bundled where needed.
#[derive(Clone, Copy)]
enum Branch {
    A,
    B,
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

        // Slots: grafts landing before base[i] live in runs[i]; the
        // final slot holds end-of-list grafts. Each branch's inserted
        // runs are collected first so cross-branch dedupe can compare
        // whole runs, then grafted A before B — within a shared slot
        // A's insertions come first.
        let slot_count = base.len().saturating_add(1);
        let collect = |builder: &Self, branch, image: Option<NodeId>| match image {
            Some(image) => builder.insert_runs(branch, image, &base_position, slot_count),
            None => vec![Vec::new(); slot_count], // cov-excl-line: survivors have images in both branches.
        };
        let runs_a = collect(self, Branch::A, in_a);
        let runs_b = collect(self, Branch::B, in_b);
        let kept_b: Vec<Vec<NodeId>> = runs_a
            .iter()
            .zip(&runs_b)
            .map(|(run_a, run_b)| self.dedupe_run(run_a, run_b))
            .collect();

        let graft_all = |builder: &mut Self, branch, runs: &[Vec<NodeId>]| -> Vec<Vec<MergedId>> {
            runs.iter()
                .map(|run| run.iter().map(|&n| builder.graft(branch, n)).collect())
                .collect()
        };
        let grafted_a = graft_all(self, Branch::A, &runs_a);
        let grafted_b = graft_all(self, Branch::B, &kept_b);

        // Materialize: slot grafts, then the base survivor they
        // precede. Runs are sized base.len()+1, so the indexing below
        // stays in bounds.
        let mut children: Vec<MergedId> = Vec::new();
        #[allow(clippy::indexing_slicing)]
        for (i, &c) in base.iter().enumerate() {
            children.extend(&grafted_a[i]);
            children.extend(&grafted_b[i]);
            if !self.consumed.contains(&c) {
                children.push(self.build_survivor(c));
            }
        }
        #[allow(clippy::indexing_slicing)]
        {
            children.extend(&grafted_a[base.len()]);
            children.extend(&grafted_b[base.len()]);
        }

        self.push(MergedNode { origin, children })
    }

    /// Walks one branch image's children, collecting insertions into
    /// the slot after the nearest preceding child whose preimage sits
    /// in the base.
    fn insert_runs(
        &self,
        branch: Branch,
        parent_image: NodeId,
        base_position: &HashMap<NodeId, usize>,
        slot_count: usize,
    ) -> Vec<Vec<NodeId>> {
        let (tree, matching) = match branch {
            Branch::A => (self.a, self.f),
            Branch::B => (self.b, self.g),
        };
        let mut runs: Vec<Vec<NodeId>> = vec![Vec::new(); slot_count];
        let mut cursor = 0usize;
        for &child in tree.children(parent_image) {
            match matching.preimage(child) {
                Some(c) => {
                    if let Some(&pos) = base_position.get(&c) {
                        cursor = pos.saturating_add(1);
                    }
                }
                None => {
                    // Cursor is at most base.len(); runs has one more.
                    #[allow(clippy::indexing_slicing)]
                    runs[cursor].push(child);
                }
            }
        }
        runs
    }

    /// B's insertions in one slot that survive cross-branch dedupe
    /// against A's. An equal run is the same edit made twice and lands
    /// zero of B's nodes. Differing runs dedupe element-group-wise —
    /// each A group absorbs at most one equal B group so
    /// multiplicities still add up, and the groups B alone inserted
    /// survive; this is what merges two branches' insertions under a
    /// commutative parent. Runs that do not decompose into groups fall
    /// back to per-node dedupe (equal separators must not absorb one
    /// another mid-run, but such runs only co-occur in a slot when the
    /// anchor was deleted, where per-node matching is the old, safe
    /// behavior). Either way an equal run dedupes completely.
    fn dedupe_run(&self, run_a: &[NodeId], run_b: &[NodeId]) -> Vec<NodeId> {
        let hashes = |tree: &Tree, nodes: &[NodeId]| -> Vec<u64> {
            nodes.iter().map(|&n| tree.hash(n)).collect()
        };
        match (
            rules::element_groups(self.a, run_a),
            rules::element_groups(self.b, run_b),
        ) {
            (Some(groups_a), Some(groups_b)) => {
                let mut available: Vec<Vec<u64>> =
                    groups_a.iter().map(|group| hashes(self.a, group)).collect();
                let mut kept = Vec::new();
                for group in groups_b {
                    let group_hashes = hashes(self.b, &group);
                    match available.iter().position(|twin| *twin == group_hashes) {
                        Some(i) => {
                            available.swap_remove(i);
                        }
                        None => kept.extend(group),
                    }
                }
                kept
            }
            _ => {
                let mut available = hashes(self.a, run_a);
                run_b
                    .iter()
                    .copied()
                    .filter(|&node| {
                        let hash = self.b.hash(node);
                        match available.iter().position(|&twin| twin == hash) {
                            Some(i) => {
                                available.swap_remove(i);
                                false
                            }
                            None => true,
                        }
                    })
                    .collect()
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
            MergeOutcome::Conflicts(conflicts) => panic!("unexpected conflicts: {conflicts:?}"),
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
    fn duplicate_identical_insertions_dedupe_pairwise() -> Result<(), Error> {
        // Two equal insertions per branch: each A graft absorbs
        // exactly one B twin, so the pair lands twice, not four times.
        assert_merges("json", "[1]", "[1, 9, 9]", "[1, 9, 9]", "[1, 9, 9]")?;
        // The same with a single-hash slot (no separators): dedupe
        // must match twins by hash, not merely consume any entry.
        assert_merges(
            "rust",
            "fn f() { }",
            "fn f() { x(); x(); }",
            "fn f() { x(); x(); }",
            "fn f() { x(); x(); }",
        )
    }

    #[test]
    fn a_graft_does_not_resurrect_what_the_other_branch_deleted() -> Result<(), Error> {
        // A wraps y() in a block; B deletes y() outright. The block
        // survives from A, but the statement inside it died in B and
        // must not be pulled into the graft.
        assert_merges(
            "rust",
            "fn f() { x(); y(); }",
            "fn f() { x(); { y(); } }",
            "fn f() { x(); }",
            "fn f() { x(); { } }",
        )
    }

    #[test]
    fn a_reformatting_only_branch_wins_the_bytes() -> Result<(), Error> {
        // B reformats without changing the tree, so no relabel fires;
        // the reformat preference adopts B's layout instead of
        // silently discarding it with O's stale bytes.
        let o_text = "fn keep() {\n    x(y);\n}\n";
        let b_text = "fn keep(  ) {  x( y ) ;  }\n";
        let o = parse(o_text, "rust")?;
        let a = parse(o_text, "rust")?;
        let b = parse(b_text, "rust")?;
        match merge_to_text(&o, &a, &b)? {
            MergeResult::Merged(text) => assert_eq!(text, b_text),
            MergeResult::Conflicts(conflicts) => panic!("unexpected conflicts: {conflicts:?}"),
        }
        Ok(())
    }

    #[test]
    fn concurrent_distinct_reformats_prefer_a() -> Result<(), Error> {
        // Span synthesis cannot honor two reformats of the same code
        // at once; the tie breaks to A.
        let o_text = "fn keep() {\n    x(y);\n}\n";
        let a_text = "fn keep() { x(y); }\n";
        let b_text = "fn keep(  ) {  x( y ) ;  }\n";
        let o = parse(o_text, "rust")?;
        let a = parse(a_text, "rust")?;
        let b = parse(b_text, "rust")?;
        match merge_to_text(&o, &a, &b)? {
            MergeResult::Merged(text) => assert_eq!(text, a_text),
            MergeResult::Conflicts(conflicts) => panic!("unexpected conflicts: {conflicts:?}"),
        }
        Ok(())
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
            MergeOutcome::Merged(m) => panic!("unexpected merge: {:?}", m.shape(&o, &a, &b)),
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
        let conflicts = assert_conflicts(
            "rust",
            "fn f() { x(); }",
            "fn f() { x(); y(); }",
            "fn f() { x(); z(); }",
            "insert-insert",
        )?;
        // The conflict carries both branches' inserted spans.
        let a = parse("fn f() { x(); y(); }", "rust")?;
        let witnessed = conflicts.iter().any(|c| {
            c.span_a
                .clone()
                .and_then(|span| a.source_slice(span))
                .is_some_and(|text| text.contains("y()"))
                && c.span_b.is_some()
        });
        assert!(witnessed);
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
    fn insert_under_a_deleted_node_conflicts_reversed() -> Result<(), Error> {
        // Same broken dependency with the branches swapped: B grows
        // the body A deletes.
        assert_conflicts(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn b() { y(); }",
            "fn a() { x(); z(); }\nfn b() { y(); }",
            "insert-delete",
        )?;
        Ok(())
    }

    #[test]
    fn disjoint_deletions_emptying_required_children_conflict() -> Result<(), Error> {
        // A drops E1, B drops E2 — individually fine, disjoint (so no
        // delete-delete), but together they empty the throws list,
        // whose children the grammar requires.
        assert_conflicts(
            "java",
            "class C { void m() throws E1, E2 { } }",
            "class C { void m() throws E2 { } }",
            "class C { void m() throws E1 { } }",
            "arity",
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
    fn relabel_under_a_deleted_node_conflicts() -> Result<(), Error> {
        // A deletes fn b outright; B edits inside its body. Without
        // the rule, deletion wins as a valid pushout and B's edit
        // silently vanishes.
        let conflicts = assert_conflicts(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); }",
            "fn a() { x(); }\nfn b() { z(); }",
            "relabel-delete",
        )?;
        // The conflict carries B's relabeled span.
        let b = parse("fn a() { x(); }\nfn b() { z(); }", "rust")?;
        let witnessed = conflicts.iter().any(|c| {
            c.rule == "relabel-delete"
                && c.span_b
                    .clone()
                    .and_then(|span| b.source_slice(span))
                    .is_some_and(|text| text == "z")
        });
        assert!(witnessed);
        Ok(())
    }

    #[test]
    fn relabel_under_a_deleted_node_conflicts_reversed() -> Result<(), Error> {
        // Mirrored: A edits the body B deletes.
        assert_conflicts(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); }\nfn b() { z(); }",
            "fn a() { x(); }",
            "relabel-delete",
        )?;
        Ok(())
    }

    #[test]
    fn same_key_inserted_at_different_slots_conflicts() -> Result<(), Error> {
        // A adds "k" at the front, B adds "k" at the back, values
        // differ. No shared slot, so insert-insert is blind; without
        // name-collision the merge holds the key twice.
        let conflicts = assert_conflicts(
            "json",
            r#"{"a": 1, "b": 2}"#,
            r#"{"k": 8, "a": 1, "b": 2}"#,
            r#"{"a": 1, "b": 2, "k": 9}"#,
            "name-collision",
        )?;
        // The conflict carries both inserted pairs.
        let a = parse(r#"{"k": 8, "a": 1, "b": 2}"#, "json")?;
        let witnessed = conflicts.iter().any(|c| {
            c.rule == "name-collision"
                && c.span_a
                    .clone()
                    .and_then(|span| a.source_slice(span))
                    .is_some_and(|text| text.contains("\"k\""))
                && c.span_b.is_some()
        });
        assert!(witnessed);
        Ok(())
    }

    #[test]
    fn identical_key_inserted_at_different_slots_conflicts() -> Result<(), Error> {
        // Identical values still conflict: dedupe only merges grafts
        // sharing a slot, so accepting this would keep both copies.
        assert_conflicts(
            "json",
            r#"{"a": 1, "b": 2}"#,
            r#"{"k": 8, "a": 1, "b": 2}"#,
            r#"{"a": 1, "b": 2, "k": 8}"#,
            "name-collision",
        )?;
        Ok(())
    }

    #[test]
    fn same_class_inserted_at_different_slots_conflicts() -> Result<(), Error> {
        assert_conflicts(
            "java",
            "class P { }\nclass Q { }",
            "class N { int x; }\nclass P { }\nclass Q { }",
            "class P { }\nclass Q { }\nclass N { boolean y; }",
            "name-collision",
        )?;
        Ok(())
    }

    #[test]
    fn same_key_inserted_into_different_objects_merges() -> Result<(), Error> {
        // Same name under different parents is no collision.
        assert_merges(
            "json",
            r#"{"x": {"a": 1}, "y": {"b": 2}}"#,
            r#"{"x": {"k": 1, "a": 1}, "y": {"b": 2}}"#,
            r#"{"x": {"a": 1}, "y": {"b": 2, "k": 2}}"#,
            r#"{"x": {"k": 1, "a": 1}, "y": {"b": 2, "k": 2}}"#,
        )
    }

    #[test]
    fn relabel_and_delete_in_different_functions_merge() -> Result<(), Error> {
        // The rule must not fire when the relabel is outside the
        // deleted subtree: A deletes fn b, B renames a call in fn a.
        assert_merges(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); }",
            "fn a() { z(); }\nfn b() { y(); }",
            "fn a() { z(); }",
        )?;
        // And mirrored: A renames while B deletes, exercising the
        // rule's other arm.
        assert_merges(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { z(); }\nfn b() { y(); }",
            "fn a() { x(); }",
            "fn a() { z(); }",
        )
    }

    #[test]
    fn insert_and_delete_in_different_functions_merge() -> Result<(), Error> {
        assert_merges(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); z(); }\nfn b() { y(); }",
            "fn a() { x(); }",
            "fn a() { x(); z(); }",
        )?;
        // And mirrored: A deletes fn b while B grows fn a. The
        // insert-delete rule must not fire on unrelated deletions in
        // either direction.
        assert_merges(
            "rust",
            "fn a() { x(); }\nfn b() { y(); }",
            "fn a() { x(); }",
            "fn a() { x(); z(); }\nfn b() { y(); }",
            "fn a() { x(); z(); }",
        )
    }

    #[test]
    fn merge_to_text_passes_clean_scenarios() -> Result<(), Error> {
        let o = parse("[1, 2, 3]", "json")?;
        let a = parse("[1, 2, 4, 5, 3]", "json")?;
        let b = parse("[1, 6, 3]", "json")?;
        match merge_to_text(&o, &a, &b)? {
            MergeResult::Merged(text) => {
                let merged = parse(&text, "json")?;
                let expected = parse("[1, 6, 4, 5, 3]", "json")?;
                assert_eq!(Shape::of(&merged), Shape::of(&expected));
            }
            MergeResult::Conflicts(conflicts) => panic!("unexpected conflicts: {conflicts:?}"),
        }
        Ok(())
    }

    #[test]
    fn merge_to_text_passes_conflicts_through() -> Result<(), Error> {
        let o = parse("fn a() {}", "rust")?;
        let a = parse("fn b() {}", "rust")?;
        let b = parse("fn c() {}", "rust")?;
        let MergeResult::Conflicts(conflicts) = merge_to_text(&o, &a, &b)? else {
            panic!("expected conflicts");
        };
        assert!(conflicts.iter().any(|c| c.rule == "relabel-relabel"));
        Ok(())
    }

    #[test]
    fn unparsable_synthesis_becomes_a_self_check_conflict() -> Result<(), Error> {
        let o = parse("[1, 2]", "json")?;
        let a = parse("[1, 2, 3]", "json")?;
        let b = parse("[1, 2]", "json")?;
        let result = merge_to_text_with(&o, &a, &b, |_, _, _, _| "[1, 2,".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected a self-check conflict");
        };
        assert!(conflicts.iter().all(|c| c.rule == "self-check"));
        Ok(())
    }

    #[test]
    fn self_check_maps_each_violation_kind_to_its_span_slot() -> Result<(), Error> {
        // Extra deletion: both branches kept 2, the fake output lost it.
        let o = parse("[1, 2]", "json")?;
        let same = parse("[1, 2]", "json")?;
        let same2 = parse("[1, 2]", "json")?;
        let result = merge_to_text_with(&o, &same, &same2, |_, _, _, _| "[1]".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected self-check conflicts");
        };
        assert!(conflicts.iter().any(|c| c.span_o.is_some()));

        // Missed insertion from B: the fake output drops B's 3.
        let b = parse("[1, 2, 3]", "json")?;
        let o = parse("[1, 2]", "json")?;
        let a = parse("[1, 2]", "json")?;
        let result = merge_to_text_with(&o, &a, &b, |_, _, _, _| "[1, 2]".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected self-check conflicts");
        };
        assert!(conflicts.iter().any(|c| c.span_b.is_some()));

        // Missed relabel from A: A renamed 1 to 2, the fake output
        // kept 1.
        let o = parse("[1]", "json")?;
        let a = parse("[2]", "json")?;
        let b = parse("[1]", "json")?;
        let result = merge_to_text_with(&o, &a, &b, |_, _, _, _| "[1]".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected self-check conflicts");
        };
        assert!(conflicts.iter().any(|c| c.span_a.is_some()));

        // Missed relabel from B, symmetrically.
        let o = parse("[1]", "json")?;
        let a = parse("[1]", "json")?;
        let b = parse("[2]", "json")?;
        let result = merge_to_text_with(&o, &a, &b, |_, _, _, _| "[1]".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected self-check conflicts");
        };
        assert!(conflicts.iter().any(|c| c.span_b.is_some()));

        // Extra insertion: nobody wrote 9; the witness lives in M, so
        // no span slot is filled.
        let o = parse("[1]", "json")?;
        let a = parse("[1]", "json")?;
        let b = parse("[1]", "json")?;
        let result = merge_to_text_with(&o, &a, &b, |_, _, _, _| "[1, 9]".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected self-check conflicts");
        };
        assert!(
            conflicts
                .iter()
                .any(|c| c.span_o.is_none() && c.span_a.is_none() && c.span_b.is_none())
        );
        Ok(())
    }

    #[test]
    fn a_dropped_node_becomes_a_self_check_conflict() -> Result<(), Error> {
        let o = parse("[1, 2]", "json")?;
        let a = parse("[1, 2, 3]", "json")?;
        let b = parse("[1, 2]", "json")?;
        // Parses fine but silently drops A's insertion; the
        // universality re-check must catch it.
        let result = merge_to_text_with(&o, &a, &b, |_, _, _, _| "[1, 2]".into())?;
        let MergeResult::Conflicts(conflicts) = result else {
            panic!("expected a self-check conflict");
        };
        assert!(!conflicts.is_empty());
        assert!(conflicts.iter().all(|c| c.rule == "self-check"));
        Ok(())
    }

    /// Clones O into a candidate MergedTree, skipping one node (and
    /// its subtree).
    fn clone_skipping(o: &Tree, skip: Option<crate::tree::NodeId>) -> MergedTree {
        fn walk(
            o: &Tree,
            node: crate::tree::NodeId,
            skip: Option<crate::tree::NodeId>,
            nodes: &mut Vec<MergedNode>,
        ) -> MergedId {
            let children = o
                .children(node)
                .iter()
                .filter(|&&c| Some(c) != skip)
                .map(|&c| walk(o, c, skip, nodes))
                .collect();
            let id = MergedId(nodes.len());
            nodes.push(MergedNode {
                origin: Origin::O(node),
                children,
            });
            id
        }
        let mut nodes = Vec::new();
        let root = walk(o, o.root(), skip, &mut nodes);
        MergedTree { nodes, root }
    }

    #[test]
    fn an_emptied_required_field_fires_the_arity_rule() -> Result<(), Error> {
        // Branch edits that are individually fine can combine into a
        // grammar-invalid tree. The pairwise rules usually fire first
        // in real merges, so drive the backstop directly: a candidate
        // M that is O minus a required field (the if-condition).
        let o = parse("fn f() { if c { } }", "rust")?;
        let lang = Lang::by_name("rust").ok_or(Error::UnknownLanguage {
            path: "rust".into(),
        })?;
        let condition_field = lang.language().field_id_for_name("condition");
        let condition = o
            .nodes()
            .find(|&n| o.field_id(n).is_some() && o.field_id(n) == condition_field);
        assert!(condition.is_some());

        let broken = clone_skipping(&o, condition);
        let conflicts = crate::rules::arity(&o, &o, &o, &broken);
        assert!(!conflicts.is_empty());
        assert!(conflicts.iter().all(|c| c.rule == "arity"));

        // The intact clone passes.
        let intact = clone_skipping(&o, None);
        assert_eq!(crate::rules::arity(&o, &o, &o, &intact), Vec::new());
        Ok(())
    }

    #[test]
    fn arity_attributes_grafted_nodes_to_their_branch() -> Result<(), Error> {
        // A branch-origin node emptied of its children is attributed
        // to that branch's span slot.
        let tree = parse("fn f() { if c { } }", "rust")?;
        let if_node = tree.nodes().find(|&n| tree.kind(n) == "if_expression");
        assert!(if_node.is_some());
        let as_root = |origin: Origin| MergedTree {
            nodes: vec![MergedNode {
                origin,
                children: Vec::new(),
            }],
            root: MergedId(0),
        };

        for (origin, expect_a, expect_b) in [
            (Origin::A(if_node.unwrap_or(tree.root())), true, false),
            (Origin::B(if_node.unwrap_or(tree.root())), false, true),
        ] {
            let conflicts = crate::rules::arity(&tree, &tree, &tree, &as_root(origin));
            assert_eq!(conflicts.len(), 1);
            let conflict = conflicts.first();
            assert_eq!(conflict.is_some_and(|c| c.span_a.is_some()), expect_a);
            assert_eq!(conflict.is_some_and(|c| c.span_b.is_some()), expect_b);
        }
        Ok(())
    }

    #[test]
    fn arity_rejects_a_doubled_single_slot_field() -> Result<(), Error> {
        // Two children in the non-multiple condition field.
        let tree = parse("fn f() { if c { } }", "rust")?;
        let lang = Lang::by_name("rust").ok_or(Error::UnknownLanguage {
            path: "rust".into(),
        })?;
        let condition_field = lang.language().field_id_for_name("condition");
        let condition = tree
            .nodes()
            .find(|&n| tree.field_id(n).is_some() && tree.field_id(n) == condition_field);
        let if_node = tree.nodes().find(|&n| tree.kind(n) == "if_expression");
        let (Some(condition), Some(if_node)) = (condition, if_node) else {
            panic!("if expression with a condition parses");
        };

        // Clone the whole tree and add a second condition child, so
        // every other slot stays valid and the multiplicity check is
        // what fires.
        let mut doubled = clone_skipping(&tree, None);
        let extra = MergedId(doubled.nodes.len());
        doubled.nodes.push(MergedNode {
            origin: Origin::O(condition),
            children: Vec::new(),
        });
        for node in &mut doubled.nodes {
            if node.origin == Origin::O(if_node) {
                node.children.push(extra);
            }
        }
        let conflicts = crate::rules::arity(&tree, &tree, &tree, &doubled);
        assert!(!conflicts.is_empty());
        Ok(())
    }

    #[test]
    fn arity_rejects_a_wrong_kind_in_a_field() -> Result<(), Error> {
        // A struct's type_identifier fills the "name" field too, but
        // function_item's name slot does not admit that kind.
        // Clone the whole tree, then swap only the function's name for
        // the struct's type_identifier, so every other field stays
        // filled and the kind check is what fires.
        let tree = parse("fn f() {}\nstruct S;", "rust")?;
        let fn_name = tree.nodes().find(|&n| tree.label(n) == Some("f"));
        let type_name = tree.nodes().find(|&n| tree.kind(n) == "type_identifier");
        let (Some(fn_name), Some(type_name)) = (fn_name, type_name) else {
            panic!("both items parse");
        };
        let mut wrong = clone_skipping(&tree, None);
        for node in &mut wrong.nodes {
            if node.origin == Origin::O(fn_name) {
                node.origin = Origin::O(type_name);
            }
        }
        let conflicts = crate::rules::arity(&tree, &tree, &tree, &wrong);
        assert!(!conflicts.is_empty());
        Ok(())
    }

    #[test]
    fn arity_rejects_a_loose_child_of_the_wrong_kind() -> Result<(), Error> {
        // A parameters node is not a declaration; source_file's
        // children slot rejects it. Only the root's child list changes
        // (a full clone plus one extra child), so this pins the
        // loose-kind branch specifically.
        let tree = parse("fn f() { x(); }", "rust")?;
        let parameters = tree.nodes().find(|&n| tree.kind(n) == "parameters");
        let Some(parameters) = parameters else {
            panic!("the function's parameters parse");
        };
        let mut wrong = clone_skipping(&tree, None);
        let extra = MergedId(wrong.nodes.len());
        wrong.nodes.push(MergedNode {
            origin: Origin::O(parameters),
            children: Vec::new(),
        });
        let root = wrong.root;
        if let Some(root_node) = wrong.nodes.get_mut(root.0) {
            root_node.children.push(extra);
        }
        let conflicts = crate::rules::arity(&tree, &tree, &tree, &wrong);
        assert!(!conflicts.is_empty());
        Ok(())
    }

    #[test]
    fn variable_arity_growth_does_not_fire_the_arity_rule() -> Result<(), Error> {
        // Two statements inserted into one block is legal growth; the
        // full merge pipeline (now ending in the arity check) stays
        // clean.
        assert_merges(
            "rust",
            "fn f() { x(); }",
            "fn f() { x(); y(); }",
            "fn f() { w(); x(); }",
            "fn f() { w(); x(); y(); }",
        )
    }

    #[test]
    fn concurrent_insertions_under_a_commutative_parent_merge() -> Result<(), Error> {
        // Both branches add a use declaration at the same slot; item
        // order carries no meaning at the top level, so the runs merge
        // as a union, A's first.
        assert_merges(
            "rust",
            "use a;\nfn main() {}",
            "use a;\nuse b;\nfn main() {}",
            "use a;\nuse c;\nfn main() {}",
            "use a;\nuse b;\nuse c;\nfn main() {}",
        )?;
        // The same for JSON object entries, where each run carries its
        // separator: the commas travel with their pairs.
        assert_merges(
            "json",
            r#"{"a": 1}"#,
            r#"{"a": 1, "b": 2}"#,
            r#"{"a": 1, "c": 3}"#,
            r#"{"a": 1, "b": 2, "c": 3}"#,
        )?;
        // And for members of a Java class body.
        assert_merges(
            "java",
            "class C { void a() { } }",
            "class C { void a() { } void b() { } }",
            "class C { void a() { } void c() { } }",
            "class C { void a() { } void b() { } void c() { } }",
        )
    }

    #[test]
    fn bound_and_interface_lists_union_too() -> Result<(), Error> {
        // Trait bounds and implements clauses are commutative: each
        // branch's addition carries its separator, and order carries
        // no meaning.
        assert_merges(
            "rust",
            "fn f<T: Clone>() {}",
            "fn f<T: Clone + Send>() {}",
            "fn f<T: Clone + Sync>() {}",
            "fn f<T: Clone + Send + Sync>() {}",
        )?;
        assert_merges(
            "java",
            "class C implements Foo { }",
            "class C implements Foo, Bar { }",
            "class C implements Foo, Baz { }",
            "class C implements Foo, Bar, Baz { }",
        )
    }

    #[test]
    fn overlapping_commutative_insertions_dedupe_element_wise() -> Result<(), Error> {
        // Both branches add `use c;`; each also adds its own. The
        // shared element lands once, group-wise — its separator
        // dedupes with it instead of being absorbed by an unrelated
        // twin mid-run.
        assert_merges(
            "rust",
            "use a;\nfn main() {}",
            "use a;\nuse c;\nuse d;\nfn main() {}",
            "use a;\nuse c;\nuse e;\nfn main() {}",
            "use a;\nuse c;\nuse d;\nuse e;\nfn main() {}",
        )?;
        assert_merges(
            "json",
            r#"{"a": 1}"#,
            r#"{"a": 1, "k": 9, "b": 2}"#,
            r#"{"a": 1, "k": 9, "c": 3}"#,
            r#"{"a": 1, "k": 9, "b": 2, "c": 3}"#,
        )
    }

    #[test]
    fn commutative_insertions_at_different_slots_merge() -> Result<(), Error> {
        // No shared slot, nothing identical: both land where anchored.
        // The comma each branch carries is a separator, not a
        // duplicate element — duplicate-insert must stay silent.
        assert_merges(
            "json",
            r#"{"a": 1, "z": 9}"#,
            r#"{"a": 1, "b": 2, "z": 9}"#,
            r#"{"a": 1, "z": 9, "c": 3}"#,
            r#"{"a": 1, "b": 2, "z": 9, "c": 3}"#,
        )
    }

    #[test]
    fn same_named_commutative_insertions_at_one_slot_conflict() -> Result<(), Error> {
        // Without name-collision covering the shared slot, the union
        // merge would emit two `fn foo` definitions.
        assert_conflicts(
            "rust",
            "fn main() {}",
            "fn main() {}\nfn foo() { a(); }",
            "fn main() {}\nfn foo() { b(); }",
            "name-collision",
        )?;
        // Identical same-named insertions dedupe instead.
        assert_merges(
            "rust",
            "fn main() {}",
            "fn main() {}\nfn foo() { a(); }",
            "fn main() {}\nfn foo() { a(); }",
            "fn main() {}\nfn foo() { a(); }",
        )
    }

    #[test]
    fn an_attributed_insertion_unions_as_one_element() -> Result<(), Error> {
        // A's attribute travels with the function it governs, so the
        // union keeps them adjacent and B's insertion lands after.
        assert_merges(
            "rust",
            "mod m {\n    pub fn run() {}\n}\n",
            "mod m {\n    #[inline]\n    pub fn setup() {}\n\n    pub fn run() {}\n}\n",
            "mod m {\n    pub fn teardown() {}\n\n    pub fn run() {}\n}\n",
            "mod m {\n    #[inline]\n    pub fn setup() {}\n\n    pub fn teardown() {}\n\n    pub fn run() {}\n}\n",
        )
    }

    #[test]
    fn a_trailing_attribute_blocks_the_union() -> Result<(), Error> {
        // A attributes the *surviving* function below the slot, so its
        // inserted run ends with the attribute; splicing B's insertion
        // in between would silently move the attribute onto B's code.
        assert_conflicts(
            "rust",
            "mod m {\n    pub fn run() {}\n}\n",
            "mod m {\n    pub fn setup() {}\n\n    #[cfg(test)]\n    pub fn run() {}\n}\n",
            "mod m {\n    pub fn teardown() {}\n\n    pub fn run() {}\n}\n",
            "insert-insert",
        )?;
        Ok(())
    }

    #[test]
    fn an_attributed_and_a_bare_insertion_of_one_name_conflict() -> Result<(), Error> {
        // The named nodes hash equal but the element groups differ, so
        // the builder would land both copies; name-collision must
        // compare groups, not names alone.
        assert_conflicts(
            "rust",
            "fn main() {}\n",
            "fn main() {}\n#[inline]\nfn helper() {}\n",
            "fn main() {}\nfn helper() {}\n",
            "name-collision",
        )?;
        Ok(())
    }

    #[test]
    fn identical_nameless_insertions_at_different_slots_conflict() -> Result<(), Error> {
        // Use declarations carry no name field, so name-collision is
        // blind to them; without duplicate-insert the union would land
        // `use c;` twice — a compile error neither branch wrote.
        assert_conflicts(
            "rust",
            "use a;\nuse b;\n\nfn main() {}\n",
            "use c;\nuse a;\nuse b;\n\nfn main() {}\n",
            "use a;\nuse b;\nuse c;\n\nfn main() {}\n",
            "duplicate-insert",
        )?;
        Ok(())
    }

    #[test]
    fn commutative_insertions_with_trailing_separators_conflict() -> Result<(), Error> {
        // Prepending into an object leaves each run's comma trailing
        // (`"b": 2` then `,`); a union would need a separator neither
        // branch wrote, so this stays an insert-insert conflict.
        assert_conflicts(
            "json",
            r#"{"z": 0}"#,
            r#"{"b": 2, "z": 0}"#,
            r#"{"c": 3, "z": 0}"#,
            "insert-insert",
        )?;
        Ok(())
    }

    #[test]
    fn ungroupable_runs_fall_back_to_per_node_dedupe() -> Result<(), Error> {
        // dedupe_run's fallback, driven directly: no rule-clean merge
        // reaches it without a slot-anchor mismatch between the rules
        // and the builder. A run ending in a separator cannot group;
        // per-node matching still dedupes B's comma against A's and
        // keeps the value A never inserted.
        let o = parse("[1]", "json")?;
        let a = parse("[1, 2]", "json")?;
        let b = parse("[1, 3]", "json")?;
        let f = diff(&o, &a);
        let g = diff(&o, &b);
        let builder = Builder {
            o: &o,
            a: &a,
            b: &b,
            f: &f,
            g: &g,
            nodes: Vec::new(),
            placed: HashSet::new(),
            consumed: HashSet::new(),
        };
        let comma_and = |tree: &Tree, digit: &str| -> Vec<NodeId> {
            let comma = tree.nodes().find(|&n| tree.kind(n) == ",");
            let value = tree.nodes().find(|&n| tree.label(n) == Some(digit));
            match (comma, value) {
                (Some(comma), Some(value)) => vec![comma, value],
                _ => panic!("array literal parses"),
            }
        };
        // A's run is comma-only (ungroupable); B's comma dedupes
        // against it per-node and B's value survives.
        let run_a: Vec<NodeId> = comma_and(&a, "2").first().copied().into_iter().collect();
        let run_b = comma_and(&b, "3");
        let kept = builder.dedupe_run(&run_a, &run_b);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept.first().map(|&n| b.label(n)), Some(Some("3")));
        Ok(())
    }

    #[test]
    fn element_groups_split_on_named_nodes() -> Result<(), Error> {
        // A run of [comma value comma value] groups pairwise; a run
        // with a trailing separator does not group at all.
        let tree = parse("[1, 2, 3]", "json")?;
        let array = tree
            .nodes()
            .find(|&n| tree.kind(n) == "array")
            .unwrap_or(tree.root());
        let children = tree.children(array);
        // children: [ 1 , 2 , 3 ] — drop the brackets.
        let interior = children
            .get(1..children.len().saturating_sub(1))
            .unwrap_or(&[]);
        let groups = crate::rules::element_groups(&tree, interior);
        assert_eq!(
            groups.as_ref().map(Vec::len),
            Some(3),
            "1 | ,2 | ,3: {groups:?}"
        );
        // Trailing comma (no closing element) refuses to group.
        let trailing = children
            .get(1..children.len().saturating_sub(2))
            .unwrap_or(&[]);
        assert_eq!(crate::rules::element_groups(&tree, trailing), None);
        Ok(())
    }

    #[test]
    fn element_groups_attach_attributes_to_the_next_item() -> Result<(), Error> {
        // An attribute is a forward-binding prefix: it joins the
        // following item's group, and a run it ends refuses to group.
        let tree = parse("#[inline]\nfn f() {}\nfn g() {}", "rust")?;
        let children = tree.children(tree.root());
        // [attribute_item, function_item, function_item]
        let groups = crate::rules::element_groups(&tree, children);
        assert_eq!(
            groups.as_ref().map(Vec::len),
            Some(2),
            "attr+f | g: {groups:?}"
        );
        assert_eq!(
            groups.as_ref().and_then(|g| g.first()).map(Vec::len),
            Some(2)
        );
        // A run ending in the attribute (its target elsewhere) refuses
        // to group.
        let mut trailing: Vec<NodeId> = children.get(1..).unwrap_or(&[]).to_vec();
        trailing.extend(children.first());
        assert_eq!(crate::rules::element_groups(&tree, &trailing), None);
        Ok(())
    }

    #[test]
    fn a_comment_edit_merges_with_a_code_edit() -> Result<(), Error> {
        // A rewrites the comment, B renames the call: disjoint edits,
        // both land — the merge the trivia representation silently
        // dropped to one side.
        let o = parse("// call the thing\nfn f() { x(); }\n", "rust")?;
        let a = parse("// call the improved thing\nfn f() { x(); }\n", "rust")?;
        let b = parse("// call the thing\nfn f() { y(); }\n", "rust")?;
        match merge_to_text(&o, &a, &b)? {
            MergeResult::Merged(text) => {
                assert_eq!(text, "// call the improved thing\nfn f() { y(); }\n");
            }
            MergeResult::Conflicts(conflicts) => panic!("unexpected conflicts: {conflicts:?}"),
        }
        Ok(())
    }

    #[test]
    fn concurrent_distinct_comment_edits_conflict() -> Result<(), Error> {
        assert_conflicts(
            "rust",
            "// v1\nfn f() {}",
            "// v2\nfn f() {}",
            "// v3\nfn f() {}",
            "relabel-relabel",
        )?;
        // The identical edit made twice still merges.
        assert_merges(
            "rust",
            "// v1\nfn f() {}",
            "// v2\nfn f() {}",
            "// v2\nfn f() {}",
            "// v2\nfn f() {}",
        )
    }

    #[test]
    fn a_comment_edit_under_a_deletion_conflicts() -> Result<(), Error> {
        // A deletes fn b and its comment; B rewrites that comment.
        // Deletion-wins would silently drop B's edit — relabel-delete
        // now sees the comment like any other node.
        assert_conflicts(
            "rust",
            "fn a() {}\n// about b\nfn b() {}\n",
            "fn a() {}\n",
            "fn a() {}\n// all about b\nfn b() {}\n",
            "relabel-delete",
        )?;
        Ok(())
    }

    #[test]
    fn an_inserted_comment_does_not_fire_the_arity_rule() -> Result<(), Error> {
        // Extras fill no grammar slot and node-types.json admits them
        // nowhere, so a changed node holding a comment child must be
        // vetted with the comment skipped, not rejected.
        assert_merges(
            "rust",
            "fn f() { x(); }",
            "fn f() { /* note */ x(); }",
            "fn f() { x(); }",
            "fn f() { /* note */ x(); }",
        )
    }

    #[test]
    fn concurrent_distinct_comment_inserts_conflict() -> Result<(), Error> {
        // The motivating probe from the mergiraf comparison: each
        // branch appends its own comment after the comma. The runs end
        // in a comment — a forward-binding prefix with its target
        // outside the run — so they refuse to group and insert-insert
        // fires, where the trivia representation deduped the two
        // "identical" comma-inserts and dropped both comments.
        assert_conflicts(
            "rust",
            "use crate::{foo};\n",
            "use crate::{foo, /* bar */};\n",
            "use crate::{foo, /* baz */};\n",
            "insert-insert",
        )?;
        Ok(())
    }

    #[test]
    fn doc_comments_travel_with_their_item_in_a_union() -> Result<(), Error> {
        // Both branches insert a documented function at the same slot.
        // Each comment binds forward into its function's element group,
        // so the union cannot splice B's function between A's doc
        // comment and A's function.
        let o = parse("fn main() {}\n", "rust")?;
        let a = parse("/// A.\nfn a() {}\nfn main() {}\n", "rust")?;
        let b = parse("/// B.\nfn b() {}\nfn main() {}\n", "rust")?;
        match merge_to_text(&o, &a, &b)? {
            MergeResult::Merged(text) => {
                let a_pos = text.find("/// A.");
                let b_pos = text.find("/// B.");
                assert!(a_pos.is_some() && b_pos.is_some(), "{text:?}");
                let merged = parse(&text, "rust")?;
                // Doc-comment spans include their trailing newline.
                let order: Vec<_> = merged
                    .nodes()
                    .filter_map(|n| merged.label(n))
                    .filter(|l| l.starts_with("///") || *l == "a" || *l == "b" || *l == "main")
                    .collect();
                assert_eq!(
                    order,
                    vec!["/// A.\n", "a", "/// B.\n", "b", "main"],
                    "{text:?}"
                );
            }
            MergeResult::Conflicts(conflicts) => panic!("unexpected conflicts: {conflicts:?}"),
        }
        Ok(())
    }

    #[test]
    fn identical_documented_insertions_dedupe_as_one_group() -> Result<(), Error> {
        // The comment is part of the element group's hash sequence, so
        // the group dedupes whole: one comment, one function.
        assert_merges(
            "rust",
            "fn main() {}\n",
            "/// Shared.\nfn util() {}\nfn main() {}\n",
            "/// Shared.\nfn util() {}\nfn main() {}\n",
            "/// Shared.\nfn util() {}\nfn main() {}\n",
        )
    }

    #[test]
    fn identical_comments_at_different_slots_merge() -> Result<(), Error> {
        // duplicate-insert exempts extras: repeating the same marker
        // comment in two places is not a duplicate definition, so both
        // copies land where their branch put them.
        assert_merges(
            "rust",
            "fn a() {}\nfn b() {}\n",
            "// marker\nfn a() {}\nfn b() {}\n",
            "fn a() {}\n// marker\nfn b() {}\n",
            "// marker\nfn a() {}\n// marker\nfn b() {}\n",
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
