//! Conflict rules: each rule inspects an (edit-from-A, edit-from-B)
//! pair with context and may yield a [`Conflict`]. The pushout applies
//! common edits once; these rules name the combinations that cannot
//! merge.

use std::collections::HashMap;

use crate::diff::{Edit, Matching};
use crate::merge::Conflict;
use crate::tree::{NodeId, Tree};

/// Everything a rule may consult: the three trees and the two diffs.
pub(crate) struct Ctx<'t> {
    pub o: &'t Tree,
    pub a: &'t Tree,
    pub b: &'t Tree,
    pub f: &'t Matching,
    pub g: &'t Matching,
}

/// A conflict rule over one (edit-from-A, edit-from-B) pair.
type PairRule = fn(&Edit, &Edit, &Ctx) -> Option<Conflict>;

const PAIR_RULES: &[PairRule] = &[relabel_relabel];

/// Runs the pairwise rules over every cross-branch edit pair and the
/// aggregate rules over the whole scripts, deduplicating identical
/// findings.
///
/// insert-insert is aggregate rather than pairwise: a branch's
/// insertion at one slot is the *sequence* of subtrees it put there
/// (a JSON insert is a comma and a value), and comparing elements
/// cross-wise would flag two branches making the identical multi-node
/// insertion.
pub(crate) fn conflicts(ctx: &Ctx, edits_a: &[Edit], edits_b: &[Edit]) -> Vec<Conflict> {
    let mut found: Vec<Conflict> = Vec::new();
    for ea in edits_a {
        for eb in edits_b {
            for rule in PAIR_RULES {
                if let Some(conflict) = rule(ea, eb, ctx)
                    && !found.contains(&conflict)
                {
                    found.push(conflict);
                }
            }
        }
    }
    insert_insert(ctx, edits_a, edits_b, &mut found);
    found
}

/// relabel-relabel: both branches relabeled the same O node, to
/// different labels. Identical relabels are one edit made twice and
/// merge silently.
fn relabel_relabel(ea: &Edit, eb: &Edit, ctx: &Ctx) -> Option<Conflict> {
    let (Edit::Relabel(xa, ya), Edit::Relabel(xb, yb)) = (ea, eb) else {
        return None;
    };
    if xa != xb || ctx.a.label(*ya) == ctx.b.label(*yb) {
        return None;
    }
    Some(Conflict {
        span_o: Some(ctx.o.span(*xa)),
        span_a: Some(ctx.a.span(*ya)),
        span_b: Some(ctx.b.span(*yb)),
        rule: "relabel-relabel",
    })
}

/// insert-insert: both branches inserted at the same slot — same
/// parent image, same nearest preceding matched sibling — but the
/// inserted subtree sequences differ. Equal sequences at the same
/// slot are one edit made twice and dedupe instead.
fn insert_insert(ctx: &Ctx, edits_a: &[Edit], edits_b: &[Edit], found: &mut Vec<Conflict>) {
    let slots_a = insert_slots(ctx.a, ctx.f, edits_a);
    let slots_b = insert_slots(ctx.b, ctx.g, edits_b);
    for (anchor, seq_a) in &slots_a {
        let Some(seq_b) = slots_b.get(anchor) else {
            continue;
        };
        let hashes = |tree: &Tree, seq: &[NodeId]| -> Vec<u64> {
            seq.iter().map(|&n| tree.hash(n)).collect()
        };
        if hashes(ctx.a, seq_a) == hashes(ctx.b, seq_b) {
            continue;
        }
        let conflict = Conflict {
            span_o: Some(ctx.o.span(anchor.0)),
            span_a: covering_span(ctx.a, seq_a),
            span_b: covering_span(ctx.b, seq_b),
            rule: "insert-insert",
        };
        if !found.contains(&conflict) {
            found.push(conflict);
        }
    }
}

/// Groups a branch's top-level insertions by slot, in sibling order
/// (the edit script lists inserts in pre-order, so same-slot siblings
/// arrive left to right).
fn insert_slots(
    tree: &Tree,
    matching: &Matching,
    script: &[Edit],
) -> HashMap<(NodeId, Option<NodeId>), Vec<NodeId>> {
    let mut slots: HashMap<(NodeId, Option<NodeId>), Vec<NodeId>> = HashMap::new();
    for edit in script {
        let Edit::Insert(node) = edit else {
            continue;
        };
        if let Some(anchor) = insert_anchor(tree, matching, *node) {
            slots.entry(anchor).or_default().push(*node);
        }
    }
    slots
}

/// The byte range covering a sequence of nodes.
fn covering_span(tree: &Tree, seq: &[NodeId]) -> Option<std::ops::Range<usize>> {
    let start = seq.iter().map(|&n| tree.span(n).start).min()?;
    let end = seq.iter().map(|&n| tree.span(n).end).max()?;
    Some(start..end)
}

/// The slot an inserted node lands in: its parent's O preimage plus
/// the O preimage of the nearest preceding matched sibling. `None`
/// for nested insertions (their parent is inserted too — they ride
/// along with the top-level graft).
fn insert_anchor(
    tree: &Tree,
    matching: &Matching,
    node: NodeId,
) -> Option<(NodeId, Option<NodeId>)> {
    let parent = tree.parent(node)?;
    let parent_o = matching.preimage(parent)?;
    let mut left = None;
    for &sibling in tree.children(parent) {
        if sibling == node {
            break;
        }
        if let Some(preimage) = matching.preimage(sibling) {
            left = Some(preimage);
        }
    }
    Some((parent_o, left))
}
