use std::collections::HashMap;

use crate::tree::{NodeId, Tree};
use crate::zs;

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

/// Diff phase 1: anchor subtrees whose hash occurs exactly once in each
/// tree, largest first.
///
/// Every anchored subtree matches wholesale (all descendants pairwise).
/// A candidate that would cross an already-placed anchor in document
/// order is demoted — skipped, never an invariant violation. Processing
/// largest-first means a candidate inside a placed anchor is already
/// matched, so surviving anchor roots always head disjoint subtrees.
pub fn anchor(src: &Tree, dst: &Tree) -> Matching {
    let mut m = Matching::new(src, dst);
    let src_sizes = subtree_sizes(src);
    let src_unique = unique_subtrees(src);
    let dst_unique = unique_subtrees(dst);

    // Anchors assert content identity, so a candidate must contain at
    // least one labeled leaf. Structure-only subtrees — empty blocks,
    // bare token runs — collide across unrelated positions (two `{}`
    // class bodies at different nesting levels hash equal) and would
    // pin the diff to the wrong twin.
    let has_label = labeled_subtrees(src);

    let mut candidates: Vec<(NodeId, NodeId)> = src_unique
        .iter()
        .filter_map(|(hash, &s)| {
            let s = s?;
            if !has_label.get(s.index()).copied().unwrap_or(false) {
                return None;
            }
            let d = dst_unique.get(hash).copied().flatten()?;
            Some((s, d))
        })
        .collect();
    candidates.sort_by_key(|&(s, _)| {
        let size = src_sizes.get(s.index()).copied().unwrap_or(0);
        (std::cmp::Reverse(size), s.index())
    });

    let mut placed: Vec<(NodeId, NodeId)> = Vec::new();
    for (s, d) in candidates {
        // Already inside a larger anchor's subtree on either side.
        if m.image(s).is_some() || m.preimage(d).is_some() {
            continue;
        }
        // Demote candidates that cross a placed anchor: disjoint
        // subtrees must keep the same document order on both sides.
        let crosses = placed
            .iter()
            .any(|&(x, y)| (x.index() < s.index()) != (y.index() < d.index()));
        if crosses {
            continue;
        }
        // Equal hashes virtually always mean equal structure, but a
        // 64-bit collision would corrupt the matching; verify before
        // committing the subtree. The rejection itself is pinned by
        // structural_eq's own tests.
        if !structural_eq(src, s, dst, d) {
            continue; // cov-excl-line: unreachable through real hashes.
        }
        match_subtree(&mut m, src, s, dst, d);
        placed.push((s, d));
    }
    m
}

/// Diff phase 2: top-down recursive alignment from the root pair.
///
/// For each matched pair, the child sequences align in two LCS passes
/// per gap between fixed points (children phase 1 already matched):
/// first keyed on subtree hash (exact-equal subtrees match wholesale),
/// then keyed on (kind, label) — which pairs same-kind interior nodes to
/// recurse into, but never labeled leaves whose text differs. Those stay
/// unmatched: whether a 2/6 pair is a relabel or a delete+insert is
/// phase 3's call, made under Zhang–Shasha's cost model.
pub fn align(src: &Tree, dst: &Tree, m: &mut Matching) {
    // O, A, and B always share a language, and these grammars use their
    // root kind nowhere else, so root pairing is unconditional.
    debug_assert_eq!(src.kind_id(src.root()), dst.kind_id(dst.root()));
    m.insert(src.root(), dst.root());

    let mut work = vec![(src.root(), dst.root())];
    while let Some((x, y)) = work.pop() {
        for (gap_x, gap_y) in gaps(m, src.children(x), dst.children(y)) {
            let kx: Vec<u64> = gap_x.iter().map(|&n| src.hash(n)).collect();
            let ky: Vec<u64> = gap_y.iter().map(|&n| dst.hash(n)).collect();
            for (i, j) in lcs_pairs(&kx, &ky) {
                // In-bounds: lcs_pairs only yields indices into its inputs.
                #[allow(clippy::indexing_slicing)]
                let (cx, cy) = (gap_x[i], gap_y[j]);
                // Same collision guard as anchoring: equal hashes are
                // trusted only after a structural check.
                if structural_eq(src, cx, dst, cy) {
                    match_subtree(m, src, cx, dst, cy);
                }
            }
        }
        // The hash pass narrowed the gaps; recompute them before the
        // same-kind pass so its matches cannot cross a hash match.
        for (gap_x, gap_y) in gaps(m, src.children(x), dst.children(y)) {
            let kx: Vec<(u16, Option<&str>)> = gap_x
                .iter()
                .map(|&n| (src.kind_id(n), src.label(n)))
                .collect();
            let ky: Vec<(u16, Option<&str>)> = gap_y
                .iter()
                .map(|&n| (dst.kind_id(n), dst.label(n)))
                .collect();
            for (i, j) in lcs_pairs(&kx, &ky) {
                #[allow(clippy::indexing_slicing)]
                let (cx, cy) = (gap_x[i], gap_y[j]);
                // A same-kind pair that contradicts the anchors around
                // it (a duplicate's twin holding them, say) would break
                // the invariants — demote it; phase 3 gets another look
                // at the residue.
                if !admissible(src, dst, m, cx, cy) {
                    continue;
                }
                m.insert(cx, cy);
                work.push((cx, cy));
            }
        }
    }
}

/// Runs all three diff phases: hash anchors, recursive alignment, and
/// bounded Zhang–Shasha on the residues.
pub fn diff(o: &Tree, a: &Tree) -> Matching {
    let mut m = anchor(o, a);
    align(o, a, &mut m);
    refine(o, a, &mut m);
    m
}

/// One edit in the script derived from a matching. The matching is the
/// first-class object; this is a view of its holes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// A src node with no image.
    Delete(NodeId),
    /// A dst node with no preimage.
    Insert(NodeId),
    /// A matched pair whose labels differ.
    Relabel(NodeId, NodeId),
}

/// Derives the edit script from a matching: deletes in src pre-order,
/// then inserts in dst pre-order, then relabels.
pub fn edits(o: &Tree, a: &Tree, m: &Matching) -> Vec<Edit> {
    let mut script = Vec::new();
    script.extend(
        o.nodes()
            .filter(|&n| m.image(n).is_none())
            .map(Edit::Delete),
    );
    script.extend(
        a.nodes()
            .filter(|&n| m.preimage(n).is_none())
            .map(Edit::Insert),
    );
    script.extend(
        m.pairs()
            .filter(|&(s, d)| o.label(s) != a.label(d))
            .map(|(s, d)| Edit::Relabel(s, d)),
    );
    script
}

/// A tree reduced to what structural equality means here: kinds,
/// labels, and shape — spans excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub kind_id: u16,
    pub label: Option<String>,
    pub children: Vec<Shape>,
}

impl Shape {
    /// The shape of a parsed tree.
    pub fn of(tree: &Tree) -> Self {
        Self::of_node(tree, tree.root())
    }

    fn of_node(tree: &Tree, node: NodeId) -> Self {
        Self {
            kind_id: tree.kind_id(node),
            label: tree.label(node).map(String::from),
            children: tree
                .children(node)
                .iter()
                .map(|&child| Self::of_node(tree, child))
                .collect(),
        }
    }
}

/// Applies an edit script to `o`, reconstructing the destination shape.
///
/// A test oracle for the round-trip property, kept in the library
/// because the merge pushout reuses the same grafting shape: survivors
/// contribute kind and label from the src side (relabels rewrite them),
/// inserts contribute whole dst subtrees, and placement is read from
/// the dst tree's (parent-image, sibling-index) positions. The oracle
/// asserts the script is exactly the matching's holes as it goes, so a
/// diff that drops or invents an edit fails here rather than comparing
/// equal by accident.
pub fn apply(o: &Tree, a: &Tree, m: &Matching, script: &[Edit]) -> Shape {
    let mut deletes: Vec<NodeId> = Vec::new();
    let mut inserts: Vec<NodeId> = Vec::new();
    let mut relabels: HashMap<NodeId, NodeId> = HashMap::new();
    for edit in script {
        match *edit {
            Edit::Delete(n) => deletes.push(n),
            Edit::Insert(n) => inserts.push(n),
            Edit::Relabel(s, d) => {
                relabels.insert(s, d);
            }
        }
    }
    assert!(
        deletes.iter().all(|&n| m.image(n).is_none()),
        "a delete names a matched src node"
    );
    assert!(
        inserts.iter().all(|&n| m.preimage(n).is_none()),
        "an insert names a matched dst node"
    );
    let survivors = o.nodes().filter(|&n| m.image(n).is_some()).count();
    assert_eq!(
        survivors.saturating_add(deletes.len()),
        o.nodes().count(),
        "deletes must cover exactly the unmatched src nodes"
    );
    assert_eq!(
        survivors.saturating_add(inserts.len()),
        a.nodes().count(),
        "inserts must cover exactly the unmatched dst nodes"
    );

    build(o, a, m, &relabels, &inserts, a.root())
}

/// Builds the reconstructed shape for the dst position `node`: from the
/// matched src node's kind and label when one exists, from the dst
/// insert otherwise.
fn build(
    o: &Tree,
    a: &Tree,
    m: &Matching,
    relabels: &HashMap<NodeId, NodeId>,
    inserts: &[NodeId],
    node: NodeId,
) -> Shape {
    let (kind_id, label) = match m.preimage(node) {
        Some(src) => {
            let label = match relabels.get(&src) {
                Some(&target) => a.label(target).map(String::from),
                None => o.label(src).map(String::from),
            };
            (o.kind_id(src), label)
        }
        None => {
            assert!(
                inserts.contains(&node),
                "an unmatched dst node has no insert edit"
            );
            (a.kind_id(node), a.label(node).map(String::from))
        }
    };
    Shape {
        kind_id,
        label,
        children: a
            .children(node)
            .iter()
            .map(|&child| build(o, a, m, relabels, inserts, child))
            .collect(),
    }
}

/// The largest residue (total nodes across both sides of a gap) that
/// phase 3 will hand to Zhang–Shasha, whose DP is quartic in the worst
/// case. Over budget, a gap stays unmatched: delete+insert is
/// conservative, never wrong.
const ZS_BUDGET: usize = 400;

/// Diff phase 3: bounded Zhang–Shasha on the residues.
///
/// For every matched pair whose child gaps still hold unmatched nodes
/// on both sides, the gap's forests — pruned of matched descendants,
/// whose residues belong to their own pair's gap — run through the
/// optimal edit mapping under phase costs: relabel 1 within a kind,
/// never across kinds, insert/delete 1. Running per gap keeps every
/// folded pair inside the fixed points that bound it, so the fold
/// cannot cross an anchor and needs no demotion pass.
pub fn refine(src: &Tree, dst: &Tree, m: &mut Matching) {
    let pairs: Vec<(NodeId, NodeId)> = m.pairs().collect();
    for (p, q) in pairs {
        for (gap_x, gap_y) in gaps(m, src.children(p), dst.children(q)) {
            if gap_x.is_empty() || gap_y.is_empty() {
                continue;
            }
            let src_size: usize = gap_x.iter().map(|&n| pruned_size(src, n, m, true)).sum();
            let dst_size: usize = gap_y.iter().map(|&n| pruned_size(dst, n, m, false)).sum();
            if src_size.saturating_add(dst_size) > ZS_BUDGET {
                continue;
            }
            let src_forest = zs::ZsTree {
                value: None,
                children: gap_x
                    .iter()
                    .map(|&n| pruned_tree(src, n, m, true))
                    .collect(),
            };
            let dst_forest = zs::ZsTree {
                value: None,
                children: gap_y
                    .iter()
                    .map(|&n| pruned_tree(dst, n, m, false))
                    .collect(),
            };
            let mapped = zs::mapping(&src_forest, &dst_forest, |s, d| match (s, d) {
                // The virtual forest roots pair freely with each other.
                (None, None) => Some(0),
                (Some(x), Some(y)) if src.kind_id(x) == dst.kind_id(y) => {
                    if src.label(x) == dst.label(y) {
                        Some(0)
                    } else {
                        Some(1)
                    }
                }
                // Cross-kind (or a real node against a virtual root):
                // never matched.
                _ => None,
            });
            for (s, d) in mapped {
                if let (Some(x), Some(y)) = (s, d) {
                    // The forests were pruned of matched descendants,
                    // so ZS cannot see anchors that pin x (or y)
                    // elsewhere; demote pairs that would break the
                    // invariants around them.
                    if admissible(src, dst, m, x, y) {
                        m.insert(x, y);
                    }
                }
            }
        }
    }
}

/// Node count of `node`'s subtree with matched descendants pruned.
fn pruned_size(tree: &Tree, node: NodeId, m: &Matching, src_side: bool) -> usize {
    let mut count = 0usize;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        count = count.saturating_add(1);
        stack.extend(tree.children(n).iter().copied().filter(|&c| {
            let matched = if src_side {
                m.image(c).is_some()
            } else {
                m.preimage(c).is_some()
            };
            !matched
        }));
    }
    count
}

/// `node`'s subtree as a ZS tree, matched descendants pruned. A matched
/// descendant's own residues belong to that pair's gaps, not this one.
fn pruned_tree(
    tree: &Tree,
    node: NodeId,
    m: &Matching,
    src_side: bool,
) -> zs::ZsTree<Option<NodeId>> {
    zs::ZsTree {
        value: Some(node),
        children: tree
            .children(node)
            .iter()
            .copied()
            .filter(|&c| {
                let matched = if src_side {
                    m.image(c).is_some()
                } else {
                    m.preimage(c).is_some()
                };
                !matched
            })
            .map(|c| pruned_tree(tree, c, m, src_side))
            .collect(),
    }
}

/// Splits two child sequences into alignable gaps between fixed points —
/// `xs` children whose image lies in `ys`. Children matched elsewhere
/// (into another part of the tree) are excluded from alignment entirely.
fn gaps(m: &Matching, xs: &[NodeId], ys: &[NodeId]) -> Vec<(Vec<NodeId>, Vec<NodeId>)> {
    let ypos: HashMap<NodeId, usize> = ys.iter().enumerate().map(|(i, &y)| (y, i)).collect();
    let unmatched_ys = |range: std::ops::Range<usize>| -> Vec<NodeId> {
        ys.get(range)
            .unwrap_or(&[])
            .iter()
            .copied()
            .filter(|&y| m.preimage(y).is_none())
            .collect()
    };

    let mut out = Vec::new();
    let mut gap_x: Vec<NodeId> = Vec::new();
    let mut y_start = 0usize;
    for &xi in xs {
        match m.image(xi) {
            Some(image) => {
                if let Some(&pos) = ypos.get(&image) {
                    // Sibling order makes fixed-point positions
                    // increase; the max() guards match anyway so a
                    // violation cannot produce a crossing alignment.
                    out.push((
                        std::mem::take(&mut gap_x),
                        unmatched_ys(y_start..pos.max(y_start)),
                    ));
                    y_start = y_start.max(pos.saturating_add(1));
                }
            }
            None => gap_x.push(xi),
        }
    }
    out.push((std::mem::take(&mut gap_x), unmatched_ys(y_start..ys.len())));
    out
}

/// Longest common subsequence over two key sequences, as index pairs in
/// increasing order on both sides.
fn lcs_pairs<K: PartialEq>(xs: &[K], ys: &[K]) -> Vec<(usize, usize)> {
    // Textbook suffix-length DP. Indices stay within the table, which
    // is sized (n+1)×(m+1), and child-sequence lengths are nowhere near
    // usize overflow.
    #[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
    {
        let (n, m) = (xs.len(), ys.len());
        let mut table = vec![vec![0usize; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i][j] = if xs[i] == ys[j] {
                    table[i + 1][j + 1] + 1
                } else {
                    table[i + 1][j].max(table[i][j + 1])
                };
            }
        }
        let mut pairs = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if xs[i] == ys[j] && table[i][j] == table[i + 1][j + 1] + 1 {
                pairs.push((i, j));
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        pairs
    }
}

/// Maps each subtree hash to its node when it occurs exactly once, or
/// `None` when duplicated.
fn unique_subtrees(tree: &Tree) -> HashMap<u64, Option<NodeId>> {
    let mut map: HashMap<u64, Option<NodeId>> = HashMap::new();
    for n in tree.nodes() {
        map.entry(tree.hash(n))
            .and_modify(|entry| *entry = None)
            .or_insert(Some(n));
    }
    map
}

/// Whether each subtree contains a labeled leaf, indexed like the
/// arena.
fn labeled_subtrees(tree: &Tree) -> Vec<bool> {
    let ids: Vec<_> = tree.nodes().collect();
    let mut labeled = vec![false; ids.len()];
    for &id in ids.iter().rev() {
        let own = tree.label(id).is_some()
            || tree
                .children(id)
                .iter()
                .any(|child| labeled.get(child.index()).copied().unwrap_or(false));
        if let Some(slot) = labeled.get_mut(id.index()) {
            *slot = own;
        }
    }
    labeled
}

/// Node counts per subtree, indexed like the arena.
fn subtree_sizes(tree: &Tree) -> Vec<usize> {
    let ids: Vec<_> = tree.nodes().collect();
    let mut sizes = vec![1usize; ids.len()];
    // Reverse pre-order: children are summed before their parent reads.
    for &id in ids.iter().rev() {
        let total = tree
            .children(id)
            .iter()
            .map(|child| sizes.get(child.index()).copied().unwrap_or(0))
            .fold(1usize, usize::saturating_add);
        if let Some(slot) = sizes.get_mut(id.index()) {
            *slot = total;
        }
    }
    sizes
}

/// Whether two subtrees are equal in kind, label, and shape.
fn structural_eq(src: &Tree, s: NodeId, dst: &Tree, d: NodeId) -> bool {
    let mut stack = vec![(s, d)];
    while let Some((x, y)) = stack.pop() {
        if src.kind_id(x) != dst.kind_id(y)
            || src.label(x) != dst.label(y)
            || src.children(x).len() != dst.children(y).len()
        {
            return false;
        }
        stack.extend(
            src.children(x)
                .iter()
                .copied()
                .zip(dst.children(y).iter().copied()),
        );
    }
    true
}

/// Matches two structurally equal subtrees pairwise, top to bottom.
fn match_subtree(m: &mut Matching, src: &Tree, s: NodeId, dst: &Tree, d: NodeId) {
    let mut stack = vec![(s, d)];
    while let Some((x, y)) = stack.pop() {
        m.insert(x, y);
        stack.extend(
            src.children(x)
                .iter()
                .copied()
                .zip(dst.children(y).iter().copied()),
        );
    }
}

/// Whether pairing `cx` with `cy` keeps the matching's invariants
/// intact around nodes that are already matched. Two ways a candidate
/// can break them, both found by the round-trip property on duplicated
/// JSON keys:
///
/// - ancestry: a matched descendant of `cx` whose image lies outside
///   `cy` (or vice versa) — pairing onto a duplicate whose twin holds
///   the anchored descendants;
/// - sibling order: a matched sibling of `cx` whose image falls on the
///   wrong side of `cy` in document order (or vice versa) — pairing
///   deep residue nodes around an anchor "hole" the pruned forests
///   cannot see.
fn admissible(src: &Tree, dst: &Tree, m: &Matching, cx: NodeId, cy: NodeId) -> bool {
    let mut stack: Vec<NodeId> = src.children(cx).to_vec();
    while let Some(w) = stack.pop() {
        if let Some(z) = m.image(w)
            && !is_ancestor(dst, cy, z)
        {
            return false;
        }
        stack.extend(src.children(w));
    }
    let mut stack: Vec<NodeId> = dst.children(cy).to_vec();
    while let Some(z) = stack.pop() {
        if let Some(w) = m.preimage(z)
            && !is_ancestor(src, cx, w)
        {
            return false;
        }
        stack.extend(dst.children(z));
    }
    sibling_order_holds(src, |n| m.image(n), cx, cy)
        && sibling_order_holds(dst, |n| m.preimage(n), cy, cx)
}

/// Whether matching `x` to `y` keeps `x`'s matched siblings on the
/// same side of `y` (by document order) as they are of `x`.
fn sibling_order_holds(
    tree: &Tree,
    map: impl Fn(NodeId) -> Option<NodeId>,
    x: NodeId,
    y: NodeId,
) -> bool {
    let Some(parent) = tree.parent(x) else {
        return true; // cov-excl-line: candidates come from gaps, never the root.
    };
    let mut before = true;
    for &sibling in tree.children(parent) {
        if sibling == x {
            before = false;
            continue;
        }
        if let Some(image) = map(sibling)
            && (image.index() < y.index()) != before
        {
            return false;
        }
    }
    true
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

    fn parse_rust(src: &str) -> Result<Tree, Error> {
        let lang = Lang::by_name("rust").ok_or(Error::UnknownLanguage {
            path: "rust".into(),
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
    fn candidate_with_a_crossing_sibling_image_is_inadmissible() -> Result<(), Error> {
        // src's "1" sits before the candidate "2", but its image lands
        // after cy inside a nested array — a crossing only the src-side
        // sibling-order check can see, since the image is not a sibling
        // of cy on the dst side.
        let src = parse_json("[1, 2]")?;
        let dst = parse_json("[2, [1]]")?;
        let (Some(s1), Some(d1)) = (find_number(&src, "1"), find_number(&dst, "1")) else {
            unreachable!("both sources hold a number 1");
        };
        let (Some(cx), Some(cy)) = (find_number(&src, "2"), find_number(&dst, "2")) else {
            unreachable!("both sources hold a number 2");
        };
        let mut m = Matching::new(&src, &dst);
        m.insert(s1, d1);
        assert!(!admissible(&src, &dst, &m, cx, cy));
        Ok(())
    }

    #[test]
    fn candidate_with_sibling_images_in_order_is_admissible() -> Result<(), Error> {
        // Same shape as the crossing case, but the nested "1" stays on
        // the same side of the candidate as its preimage.
        let src = parse_json("[1, 2]")?;
        let dst = parse_json("[[1], 2]")?;
        let (Some(s1), Some(d1)) = (find_number(&src, "1"), find_number(&dst, "1")) else {
            unreachable!("both sources hold a number 1");
        };
        let (Some(cx), Some(cy)) = (find_number(&src, "2"), find_number(&dst, "2")) else {
            unreachable!("both sources hold a number 2");
        };
        let mut m = Matching::new(&src, &dst);
        m.insert(s1, d1);
        assert!(admissible(&src, &dst, &m, cx, cy));
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
    fn unique_subtrees_anchor() -> Result<(), Error> {
        let o = parse_rust("fn a() {}\nfn b() {}")?;
        let a = parse_rust("fn a() {}\nfn c() {}\nfn b() {}")?;
        let m = anchor(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        // Both fn a and fn b anchor wholesale: the function_items match
        // and so do the identifiers inside them.
        let fns = m
            .pairs()
            .filter(|&(s, _)| o.kind(s) == "function_item")
            .count();
        assert_eq!(fns, 2);
        let ids: Vec<_> = m
            .pairs()
            .filter(|&(s, _)| o.kind(s) == "identifier")
            .map(|(s, d)| (o.label(s), a.label(d)))
            .collect();
        assert!(ids.contains(&(Some("a"), Some("a"))));
        assert!(ids.contains(&(Some("b"), Some("b"))));
        Ok(())
    }

    #[test]
    fn crossing_anchors_are_demoted() -> Result<(), Error> {
        // O = [a, b], A = [b, a]: whichever anchors second would cross
        // the first, so it must be dropped, not create a crossing map.
        let o = parse_rust("fn a() {}\nfn b() {}")?;
        let a = parse_rust("fn b() {}\nfn a() {}")?;
        let m = anchor(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        let fns = m
            .pairs()
            .filter(|&(s, _)| o.kind(s) == "function_item")
            .count();
        assert_eq!(fns, 1);
        Ok(())
    }

    #[test]
    fn identical_trees_anchor_wholesale_at_the_root() -> Result<(), Error> {
        let o = parse_rust("fn a() { x(); }")?;
        let a = parse_rust("fn a() { x(); }")?;
        let m = anchor(&o, &a);
        assert_eq!(m.pairs().count(), o.nodes().count());
        assert!(m.validate(&o, &a).is_ok());
        Ok(())
    }

    #[test]
    fn structure_only_subtrees_do_not_anchor() -> Result<(), Error> {
        // O's inner [] and A's array hash equal and occur once each,
        // but an empty array carries no content identity — anchoring
        // them would pin the diff across nesting levels.
        let o = parse_json("[[]]")?;
        let a = parse_json("[]")?;
        let m = anchor(&o, &a);
        assert_eq!(m.pairs().count(), 0);
        Ok(())
    }

    #[test]
    fn larger_anchors_win_over_crossing_smaller_ones() -> Result<(), Error> {
        // The number and the inner array swap places; only one can
        // anchor. Largest first means the subtree wins and the lone
        // number is demoted.
        let o = parse_json("[5, [1, 2, 3]]")?;
        let a = parse_json("[[1, 2, 3], 5]")?;
        let m = anchor(&o, &a);
        let inner = find_kind(&o, "array").last().copied();
        assert!(inner.is_some_and(|n| m.image(n).is_some()));
        assert_eq!(find_number(&o, "5").and_then(|n| m.image(n)), None);
        Ok(())
    }

    #[test]
    fn duplicated_subtrees_do_not_anchor() -> Result<(), Error> {
        // The number 1 appears twice in O, so its hash is not unique
        // there and it must not anchor — even though it is unique in A.
        let o = parse_json("[1, 1]")?;
        let a = parse_json("[1]")?;
        let m = anchor(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        assert!(m.pairs().all(|(s, _)| o.kind(s) != "number"));
        Ok(())
    }

    fn phase12(o: &Tree, a: &Tree) -> Matching {
        let mut m = anchor(o, a);
        align(o, a, &mut m);
        m
    }

    #[test]
    fn alignment_matches_structure_around_insertions() -> Result<(), Error> {
        // The paper's Figure 2: [1,2,3] → [1,2,4,5,3].
        let o = parse_json("[1, 2, 3]")?;
        let a = parse_json("[1, 2, 4, 5, 3]")?;
        let m = phase12(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        assert_eq!(m.image(o.root()), Some(a.root()));
        let arrays_matched = find_kind(&o, "array")
            .first()
            .and_then(|&arr| m.image(arr))
            .map(|img| a.kind(img));
        assert_eq!(arrays_matched, Some("array"));
        for label in ["1", "2", "3"] {
            let image_label = find_number(&o, label)
                .and_then(|s| m.image(s))
                .and_then(|d| a.label(d));
            assert_eq!(image_label, Some(label));
        }
        for label in ["4", "5"] {
            let preimage = find_number(&a, label).and_then(|d| m.preimage(d));
            assert_eq!(preimage, None);
        }
        Ok(())
    }

    #[test]
    fn alignment_leaves_replaced_leaves_unmatched() -> Result<(), Error> {
        // The paper's Figure 3: [1,2,3] → [1,6,3]. The 2/6 pair differs
        // in label, so phase 2 leaves both unmatched — whether that is
        // a relabel or a delete+insert is phase 3's call.
        let o = parse_json("[1, 2, 3]")?;
        let a = parse_json("[1, 6, 3]")?;
        let m = phase12(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        assert_eq!(m.image(o.root()), Some(a.root()));
        for label in ["1", "3"] {
            let image_label = find_number(&o, label)
                .and_then(|s| m.image(s))
                .and_then(|d| a.label(d));
            assert_eq!(image_label, Some(label));
        }
        assert_eq!(find_number(&o, "2").and_then(|s| m.image(s)), None);
        assert_eq!(find_number(&a, "6").and_then(|d| m.preimage(d)), None);
        Ok(())
    }

    #[test]
    fn alignment_exposes_renames_as_relabel_candidates() -> Result<(), Error> {
        // Same-kind alignment matches the function_item wrapper, so the
        // differing identifiers become phase 3's relabel candidates.
        let o = parse_rust("fn a() {}")?;
        let a = parse_rust("fn b() {}")?;
        let m = phase12(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        let item_matched = find_kind(&o, "function_item")
            .first()
            .and_then(|&f| m.image(f))
            .map(|img| a.kind(img));
        assert_eq!(item_matched, Some("function_item"));
        let o_ident = o.nodes().find(|&n| o.label(n) == Some("a"));
        let a_ident = a.nodes().find(|&n| a.label(n) == Some("b"));
        assert_eq!(o_ident.and_then(|n| m.image(n)), None);
        assert_eq!(a_ident.and_then(|n| m.preimage(n)), None);
        Ok(())
    }

    fn phase123(o: &Tree, a: &Tree) -> Matching {
        diff(o, a)
    }

    #[test]
    fn identical_trees_produce_zero_edits() -> Result<(), Error> {
        let o = parse_json("[1, 2, 3]")?;
        let a = parse_json("[1, 2, 3]")?;
        let m = diff(&o, &a);
        assert_eq!(edits(&o, &a, &m).len(), 0);
        Ok(())
    }

    #[test]
    fn a_comment_edit_produces_exactly_one_relabel() -> Result<(), Error> {
        // Comments are opaque labeled leaves, so editing one is a
        // relabel like any rename — the edit the old trivia-only
        // representation could not see.
        let o = parse_rust("// old\nfn a() {}")?;
        let a = parse_rust("// new\nfn a() {}")?;
        let m = diff(&o, &a);
        let script = edits(&o, &a, &m);
        assert_eq!(script.len(), 1);
        let relabels_comment = script.iter().any(|e| {
            matches!(e, Edit::Relabel(s, d)
                if o.label(*s) == Some("// old") && a.label(*d) == Some("// new"))
        });
        assert!(relabels_comment, "{script:?}");
        Ok(())
    }

    #[test]
    fn a_rename_produces_exactly_one_relabel() -> Result<(), Error> {
        let o = parse_rust("fn a() {}")?;
        let a = parse_rust("fn b() {}")?;
        let m = diff(&o, &a);
        let script = edits(&o, &a, &m);
        assert_eq!(script.len(), 1);
        assert!(matches!(script.first(), Some(Edit::Relabel(_, _))));
        Ok(())
    }

    #[test]
    fn insertions_and_deletions_are_derived() -> Result<(), Error> {
        let o = parse_json("[1, 3]")?;
        let a = parse_json("[1, 2, 3]")?;
        let m = diff(&o, &a);
        let script = edits(&o, &a, &m);
        // The number 2 and its separating comma arrive as inserts;
        // nothing is deleted or relabeled.
        assert!(script.iter().all(|e| matches!(e, Edit::Insert(_))));
        let inserted_number = script
            .iter()
            .any(|e| matches!(e, Edit::Insert(n) if a.label(*n) == Some("2")));
        assert!(inserted_number);

        let reversed = edits(&a, &o, &diff(&a, &o));
        assert!(reversed.iter().all(|e| matches!(e, Edit::Delete(_))));
        Ok(())
    }

    #[test]
    fn apply_reconstructs_the_target_shape() -> Result<(), Error> {
        let cases = [
            ("[1, 2, 3]", "[1, 2, 4, 5, 3]"),
            ("[1, 2, 3]", "[1, 6, 3]"),
            ("[[1, 2], [1, 2]]", "[[[1, 2, 9]], [1, 2]]"),
            ("[]", "[[[]]]"),
        ];
        for (src, dst) in cases {
            let o = parse_json(src)?;
            let a = parse_json(dst)?;
            let m = diff(&o, &a);
            let script = edits(&o, &a, &m);
            assert_eq!(apply(&o, &a, &m, &script), Shape::of(&a), "{src} -> {dst}");
        }
        let o = parse_rust("fn a() { x(); }\nfn c() {}")?;
        let a = parse_rust("fn b() { x(); y(); }")?;
        let m = diff(&o, &a);
        let script = edits(&o, &a, &m);
        assert_eq!(apply(&o, &a, &m, &script), Shape::of(&a));
        Ok(())
    }

    #[test]
    fn zhang_shasha_matches_a_nested_restructure() -> Result<(), Error> {
        // A subtree moved one level deeper with an edit inside it. The
        // duplicated [1, 2] defeats hash anchoring and the alignment
        // passes stop at the array/leaf kind wall, so only phase 3 can
        // still match the 1 and 2 leaves into the deeper array.
        let o = parse_json("[[1, 2], [1, 2]]")?;
        let a = parse_json("[[[1, 2, 9]], [1, 2]]")?;
        let m = phase123(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        // Every O number has an image with the same label...
        for s in find_kind(&o, "number") {
            let image_label = m.image(s).and_then(|d| a.label(d));
            assert_eq!(image_label, o.label(s));
        }
        // ...while the freshly inserted 9 has no preimage.
        assert_eq!(find_number(&a, "9").and_then(|d| m.preimage(d)), None);
        Ok(())
    }

    #[test]
    fn zhang_shasha_relabels_a_renamed_function() -> Result<(), Error> {
        // Phase 2 leaves the differing identifiers unmatched; phase 3
        // matches them as a relabel (cost 1 beats delete+insert at 2).
        let o = parse_rust("fn a() {}")?;
        let a = parse_rust("fn b() {}")?;
        let m = phase123(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        let o_ident = o.nodes().find(|&n| o.label(n) == Some("a"));
        let image_label = o_ident.and_then(|n| m.image(n)).and_then(|d| a.label(d));
        assert_eq!(image_label, Some("b"));
        Ok(())
    }

    #[test]
    fn over_budget_residues_stay_unmatched() -> Result<(), Error> {
        // The nested-restructure scenario scaled past the ZS budget:
        // the oversized gap is left as delete+insert — conservative,
        // never wrong. Only the intact second copy's numbers match.
        let ns = (1000..1200)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let o = parse_json(&format!("[[{ns}], [{ns}]]"))?;
        let a = parse_json(&format!("[[[{ns}, 9]], [{ns}]]"))?;
        let m = phase123(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        let matched_numbers = m.pairs().filter(|&(s, _)| o.kind(s) == "number").count();
        assert_eq!(matched_numbers, 200);
        Ok(())
    }

    #[test]
    fn duplicated_object_pairs_do_not_invert_ancestry() -> Result<(), Error> {
        // Found by the round-trip property: with "items" and "flag"
        // duplicated, alignment's same-kind LCS used to match O's
        // "nested" pair to A's second "items" pair even though the
        // anchored "nested" string inside it maps into a different
        // subtree — inverting ancestry.
        let o = parse_json(
            r#"{"name": 1, "items": [1, 2], "nested": {"flag": true, "values": ["a", "b"]}, "count": 10}"#,
        )?;
        let a = parse_json(
            r#"{"name": 1, "items": [1, 2], "items": [1, 2], "nested": {"flag": true, "flag": true, "values": ["a", "b"]}, "count": 10}"#,
        )?;
        let m = diff(&o, &a);
        assert!(m.validate(&o, &a).is_ok());
        let script = edits(&o, &a, &m);
        assert_eq!(apply(&o, &a, &m, &script), Shape::of(&a));
        Ok(())
    }

    /// Naive recursive LCS length, the oracle for the DP.
    fn reference_lcs(xs: &[char], ys: &[char]) -> usize {
        let (Some((x, x_rest)), Some((y, y_rest))) = (xs.split_first(), ys.split_first()) else {
            return 0;
        };
        if x == y {
            reference_lcs(x_rest, y_rest).saturating_add(1)
        } else {
            reference_lcs(x_rest, ys).max(reference_lcs(xs, y_rest))
        }
    }

    /// All strings over {a, b, c} up to `depth` more characters.
    fn all_strings(prefix: Vec<char>, depth: usize, out: &mut Vec<Vec<char>>) {
        out.push(prefix.clone());
        if depth == 0 {
            return;
        }
        for c in ['a', 'b', 'c'] {
            let mut next = prefix.clone();
            next.push(c);
            all_strings(next, depth.saturating_sub(1), out);
        }
    }

    #[test]
    fn lcs_pairs_matches_a_reference_on_all_small_inputs() {
        // Exhaustive over both sides: the alignment tests alone
        // under-constrain the DP because phase 3 rescues small cases,
        // and hand-picked cases let index-arithmetic mutants slip
        // through the backtrack's compensating paths.
        let mut strings = Vec::new();
        all_strings(Vec::new(), 4, &mut strings);
        for xs in &strings {
            for ys in &strings {
                let pairs = lcs_pairs(xs, ys);
                assert_eq!(pairs.len(), reference_lcs(xs, ys), "LCS({xs:?}, {ys:?})");
                for (i, j) in &pairs {
                    assert_eq!(xs.get(*i), ys.get(*j), "LCS({xs:?}, {ys:?})");
                }
                for window in pairs.windows(2) {
                    let [(i1, j1), (i2, j2)] = window else {
                        unreachable!("windows(2) yields pairs");
                    };
                    assert!(i1 < i2 && j1 < j2, "LCS({xs:?}, {ys:?}) crosses: {pairs:?}");
                }
            }
        }
    }

    #[test]
    fn structural_eq_distinguishes_kind_label_and_shape() -> Result<(), Error> {
        // The collision guard in anchor() is unreachable through real
        // hashes, so its rejection paths are pinned directly here.
        let one = parse_json("[1]")?;
        let same = parse_json("[1]")?;
        let two = parse_json("[2]")?;
        let longer = parse_json("[1, 2]")?;
        let object = parse_json("{}")?;
        assert!(structural_eq(&one, one.root(), &same, same.root()));
        assert!(!structural_eq(&one, one.root(), &two, two.root()));
        assert!(!structural_eq(&one, one.root(), &longer, longer.root()));
        assert!(!structural_eq(&one, one.root(), &object, object.root()));
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
