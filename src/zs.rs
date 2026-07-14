//! Zhang–Shasha tree edit mapping.
//!
//! Textbook implementation (keyroots + forest-distance DP) over plain
//! value + children trees, standalone from the arena [`crate::Tree`] so
//! it can be tested against toy trees and reused on residue forests.
//!
//! Zhang & Shasha, "Simple fast algorithms for the editing distance
//! between trees and related problems", SIAM J. Comput. 18(6), 1989.

/// A tree for Zhang–Shasha: a value and ordered children.
#[derive(Clone)]
pub struct ZsTree<T> {
    pub value: T,
    pub children: Vec<ZsTree<T>>,
}

/// Infinite cost: forbids a relabel without risking overflow in
/// saturating arithmetic.
const INF: u32 = u32::MAX / 2;

/// Computes the optimal edit mapping between two trees.
///
/// `relabel` prices matching a src value to a dst value: `Some(0)` for
/// identical, a positive cost for a relabel, `None` for pairs that must
/// never match (cross-kind). Insertions and deletions cost 1. Returns
/// the matched value pairs; unmatched src values are deletions,
/// unmatched dst values insertions.
pub fn mapping<T: Copy>(
    src: &ZsTree<T>,
    dst: &ZsTree<T>,
    relabel: impl Fn(T, T) -> Option<u32>,
) -> Vec<(T, T)> {
    mapping_with_cost(src, dst, relabel).0
}

/// [`mapping`] plus the optimal edit cost the DP computed, so tests
/// can hold the backtrack to account: the mapping's implied cost must
/// equal the distance.
pub(crate) fn mapping_with_cost<T: Copy>(
    src: &ZsTree<T>,
    dst: &ZsTree<T>,
    relabel: impl Fn(T, T) -> Option<u32>,
) -> (Vec<(T, T)>, u32) {
    let src_flat = Flat::build(src);
    let dst_flat = Flat::build(dst);
    let solver = Solver {
        src: src_flat,
        dst: dst_flat,
        relabel,
    };
    solver.solve()
}

/// A tree flattened to 1-based post-order arrays: `values[i-1]` is node
/// i's value, `l[i-1]` the post-order index of its leftmost leaf.
struct Flat<T> {
    values: Vec<T>,
    l: Vec<usize>,
    keyroots: Vec<usize>,
}

impl<T: Copy> Flat<T> {
    fn build(tree: &ZsTree<T>) -> Self {
        let mut values = Vec::new();
        let mut l = Vec::new();
        Self::flatten(tree, &mut values, &mut l);

        // A keyroot is a node no later node shares a leftmost leaf
        // with — the root and every node with a left sibling.
        let mut seen = std::collections::HashSet::new();
        let mut keyroots: Vec<usize> = (1..=values.len())
            .rev()
            .filter(|&k| seen.insert(l.get(k.wrapping_sub(1)).copied()))
            .collect();
        keyroots.reverse();

        Self {
            values,
            l,
            keyroots,
        }
    }

    /// Appends `tree` in post-order; returns its root's 1-based index.
    fn flatten(tree: &ZsTree<T>, values: &mut Vec<T>, l: &mut Vec<usize>) -> usize {
        let child_roots: Vec<usize> = tree
            .children
            .iter()
            .map(|child| Self::flatten(child, values, l))
            .collect();
        values.push(tree.value);
        let index = values.len();
        let leftmost = child_roots
            .first()
            .and_then(|&c| l.get(c.wrapping_sub(1)).copied())
            .unwrap_or(index);
        l.push(leftmost);
        index
    }

    fn len(&self) -> usize {
        self.values.len()
    }
}

struct Solver<T, F> {
    src: Flat<T>,
    dst: Flat<T>,
    relabel: F,
}

// The DP below is the textbook algorithm on 1-based indices: every
// index is bounded by the table dimensions it was sized with, and
// costs use saturating arithmetic, so the panic-discipline lints are
// waived for fidelity to the published pseudocode.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
impl<T: Copy, F: Fn(T, T) -> Option<u32>> Solver<T, F> {
    fn solve(&self) -> (Vec<(T, T)>, u32) {
        let (n, m) = (self.src.len(), self.dst.len());
        let mut treedist = vec![vec![0u32; m + 1]; n + 1];
        for &i in &self.src.keyroots {
            for &j in &self.dst.keyroots {
                self.forest_dist(i, j, &mut treedist);
            }
        }
        let distance = treedist[n][m];

        // Backtrack: walk each subtree pair's forest table from the
        // corner, emitting pairs on diagonal steps that close a whole
        // subtree and deferring other subtree pairs as subproblems.
        let mut pairs = Vec::new();
        let mut subproblems = vec![(n, m)];
        while let Some((i, j)) = subproblems.pop() {
            let fd = self.forest_dist(i, j, &mut treedist);
            let (li, lj) = (self.src.l[i - 1], self.dst.l[j - 1]);
            let (mut di, mut dj) = (i, j);
            while di >= li || dj >= lj {
                if di >= li && fd[di][dj] == fd[di - 1][dj].saturating_add(1) {
                    di -= 1; // di deleted
                } else if dj >= lj && fd[di][dj] == fd[di][dj - 1].saturating_add(1) {
                    dj -= 1; // dj inserted
                } else if self.src.l[di - 1] == li && self.dst.l[dj - 1] == lj {
                    pairs.push((self.src.values[di - 1], self.dst.values[dj - 1]));
                    di -= 1;
                    dj -= 1;
                } else {
                    subproblems.push((di, dj));
                    di = self.src.l[di - 1] - 1;
                    dj = self.dst.l[dj - 1] - 1;
                }
            }
        }
        (pairs, distance)
    }

    /// The forest-distance DP for the subtree pair (i, j), 1-based.
    /// Fills `treedist` for every subtree pair it closes; returns the
    /// forest table for backtracking.
    fn forest_dist(&self, i: usize, j: usize, treedist: &mut [Vec<u32>]) -> Vec<Vec<u32>> {
        let (li, lj) = (self.src.l[i - 1], self.dst.l[j - 1]);
        let mut fd = vec![vec![0u32; j + 1]; i + 1];
        for di in li..=i {
            fd[di][lj - 1] = fd[di - 1][lj - 1].saturating_add(1);
        }
        for dj in lj..=j {
            fd[li - 1][dj] = fd[li - 1][dj - 1].saturating_add(1);
        }
        for di in li..=i {
            for dj in lj..=j {
                let delete = fd[di - 1][dj].saturating_add(1);
                let insert = fd[di][dj - 1].saturating_add(1);
                if self.src.l[di - 1] == li && self.dst.l[dj - 1] == lj {
                    let cost = (self.relabel)(self.src.values[di - 1], self.dst.values[dj - 1])
                        .unwrap_or(INF);
                    let replace = fd[di - 1][dj - 1].saturating_add(cost);
                    fd[di][dj] = delete.min(insert).min(replace);
                    treedist[di][dj] = fd[di][dj];
                } else {
                    let closed = fd[self.src.l[di - 1] - 1][self.dst.l[dj - 1] - 1]
                        .saturating_add(treedist[di][dj]);
                    fd[di][dj] = delete.min(insert).min(closed);
                }
            }
        }
        fd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t<'a>(label: &'a str, children: Vec<ZsTree<&'a str>>) -> ZsTree<&'a str> {
        ZsTree {
            value: label,
            children,
        }
    }

    fn leaf(label: &str) -> ZsTree<&str> {
        t(label, Vec::new())
    }

    /// Every label is the same "kind": equal is free, different costs 1.
    fn any_relabel(a: &str, b: &str) -> Option<u32> {
        if a == b { Some(0) } else { Some(1) }
    }

    /// Labels are "kinds": different labels never match.
    fn never_relabel(a: &str, b: &str) -> Option<u32> {
        if a == b { Some(0) } else { None }
    }

    #[test]
    fn identical_trees_map_completely() {
        let src = t("f", vec![leaf("a"), leaf("b")]);
        let dst = t("f", vec![leaf("a"), leaf("b")]);
        let pairs = mapping(&src, &dst, any_relabel);
        assert_eq!(pairs.len(), 3);
        for pair in [("f", "f"), ("a", "a"), ("b", "b")] {
            assert!(pairs.contains(&pair));
        }
    }

    #[test]
    fn relabels_map_in_place() {
        let src = t("f", vec![leaf("a"), leaf("b")]);
        let dst = t("f", vec![leaf("a"), leaf("c")]);
        let pairs = mapping(&src, &dst, any_relabel);
        assert!(pairs.contains(&("b", "c")));
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn nested_restructure_maps_leaves() {
        // f(a, b) → f(g(a, b)): inserting g is cheaper than losing the
        // leaves, so a and b must survive one level deeper.
        let src = t("f", vec![leaf("a"), leaf("b")]);
        let dst = t("f", vec![t("g", vec![leaf("a"), leaf("b")])]);
        let pairs = mapping(&src, &dst, any_relabel);
        for pair in [("f", "f"), ("a", "a"), ("b", "b")] {
            assert!(pairs.contains(&pair));
        }
        assert!(pairs.iter().all(|&(_, d)| d != "g"));
    }

    #[test]
    fn incompatible_nodes_never_map() {
        // With relabeling forbidden, x → y is a delete plus an insert.
        let pairs = mapping(&leaf("x"), &leaf("y"), never_relabel);
        assert!(pairs.is_empty());
    }

    #[test]
    fn deletions_and_insertions_surround_a_match() {
        let src = t("f", vec![leaf("a"), leaf("b"), leaf("c")]);
        let dst = t("f", vec![leaf("b")]);
        let pairs = mapping(&src, &dst, never_relabel);
        assert_eq!(pairs.len(), 2);
        for pair in [("f", "f"), ("b", "b")] {
            assert!(pairs.contains(&pair));
        }
    }

    /// All trees with exactly `nodes` nodes over the given labels.
    fn trees_with(nodes: usize, labels: &[u8]) -> Vec<ZsTree<u8>> {
        if nodes == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for &label in labels {
            for children in forests(nodes.saturating_sub(1), labels) {
                out.push(ZsTree {
                    value: label,
                    children,
                });
            }
        }
        out
    }

    /// All ordered forests totalling `total` nodes.
    fn forests(total: usize, labels: &[u8]) -> Vec<Vec<ZsTree<u8>>> {
        if total == 0 {
            return vec![Vec::new()];
        }
        let mut out = Vec::new();
        for first in 1..=total {
            for head in trees_with(first, labels) {
                for tail in forests(total.saturating_sub(first), labels) {
                    let mut forest = vec![head.clone()];
                    forest.extend(tail);
                    out.push(forest);
                }
            }
        }
        out
    }

    /// Clones a label tree into an identity-carrying tree with
    /// pre-order ids, returning the node count.
    fn retag(tree: &ZsTree<u8>, next: &mut usize) -> ZsTree<(usize, u8)> {
        let id = *next;
        *next = next.saturating_add(1);
        ZsTree {
            value: (id, tree.value),
            children: tree.children.iter().map(|c| retag(c, next)).collect(),
        }
    }

    #[test]
    fn mapping_cost_matches_the_dp_on_all_small_trees() {
        // Exhaustive internal-consistency oracle: the backtracked
        // mapping's implied cost (relabels + unmatched nodes on both
        // sides) must equal the DP's optimal distance, and the mapping
        // must be a bijection. Kills backtrack mutants that hand-picked
        // cases let through.
        let labels = [0u8, 1u8];
        let mut all: Vec<(ZsTree<(usize, u8)>, usize)> = Vec::new();
        for nodes in 1..=3usize {
            for tree in trees_with(nodes, &labels) {
                let mut next = 0usize;
                let tagged = retag(&tree, &mut next);
                all.push((tagged, next));
            }
        }
        for (src, src_count) in &all {
            for (dst, dst_count) in &all {
                let (pairs, distance) =
                    mapping_with_cost(src, dst, |a, b| if a.1 == b.1 { Some(0) } else { Some(1) });
                let relabels = pairs.iter().filter(|(a, b)| a.1 != b.1).count();
                let matched = pairs.len();
                let implied = relabels
                    .saturating_add(src_count.saturating_sub(matched))
                    .saturating_add(dst_count.saturating_sub(matched));
                assert_eq!(
                    u32::try_from(implied).ok(),
                    Some(distance),
                    "implied cost diverged for {matched} pairs"
                );
                let mut src_ids: Vec<usize> = pairs.iter().map(|(a, _)| a.0).collect();
                let mut dst_ids: Vec<usize> = pairs.iter().map(|(_, b)| b.0).collect();
                src_ids.sort_unstable();
                src_ids.dedup();
                dst_ids.sort_unstable();
                dst_ids.dedup();
                assert_eq!(src_ids.len(), matched);
                assert_eq!(dst_ids.len(), matched);
            }
        }
    }

    #[test]
    fn mapping_preserves_sibling_order() {
        // f(a, b) vs f(b, a): only one leaf can survive; crossing both
        // would break order, which a valid edit mapping never does.
        let src = t("f", vec![leaf("a"), leaf("b")]);
        let dst = t("f", vec![leaf("b"), leaf("a")]);
        let pairs = mapping(&src, &dst, never_relabel);
        let leaves = pairs.iter().filter(|&&(s, _)| s != "f").count();
        assert_eq!(leaves, 1);
    }
}
