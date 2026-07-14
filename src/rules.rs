//! Conflict rules: each rule inspects an (edit-from-A, edit-from-B)
//! pair with context and may yield a [`Conflict`]. The pushout applies
//! common edits once; these rules name the combinations that cannot
//! merge.

use std::collections::{HashMap, HashSet};

use crate::diff::{Edit, Matching};
use crate::merge::{Conflict, MergedId, MergedTree, Origin};
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

/// Where an insertion lands: the parent's O preimage plus the O
/// preimage of the nearest preceding matched sibling.
type Slot = (NodeId, Option<NodeId>);

const PAIR_RULES: &[PairRule] = &[relabel_relabel, relabel_delete, insert_delete];

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
    name_collision(ctx, edits_a, edits_b, &mut found);
    duplicate_insert(ctx, edits_a, edits_b, &mut found);
    delete_delete(ctx, edits_a, edits_b, &mut found);
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

/// relabel-delete: one branch relabeled an O node the other branch
/// deleted (git's delete/modify). Deletion-wins is a valid pushout,
/// so without this rule the merge is clean and the relabel silently
/// vanishes with the deleted subtree — surface it instead. Deletes
/// are per-node, so a relabel anywhere inside a deleted subtree pairs
/// with the deletion of that same node; no ancestry walk is needed.
fn relabel_delete(ea: &Edit, eb: &Edit, ctx: &Ctx) -> Option<Conflict> {
    match (ea, eb) {
        (Edit::Relabel(x, y), Edit::Delete(deleted)) if x == deleted => Some(Conflict {
            span_o: Some(ctx.o.span(*deleted)),
            span_a: Some(ctx.a.span(*y)),
            span_b: None,
            rule: "relabel-delete",
        }),
        (Edit::Delete(deleted), Edit::Relabel(x, y)) if x == deleted => Some(Conflict {
            span_o: Some(ctx.o.span(*deleted)),
            span_a: None,
            span_b: Some(ctx.b.span(*y)),
            rule: "relabel-delete",
        }),
        _ => None,
    }
}

/// insert-delete (broken dependency): one branch inserted under an O
/// node the other branch deleted. The graft has no surviving anchor to
/// re-attach to.
///
/// Stricter than the plan's sketch, which softened this through
/// node-types.json ("no surviving ancestor admits the inserted kind"):
/// the pushout builder cannot re-anchor a graft to a different
/// ancestor yet, so a softened rule would declare merges clean and
/// then silently drop the insertion. Revisit together with builder
/// re-anchoring.
fn insert_delete(ea: &Edit, eb: &Edit, ctx: &Ctx) -> Option<Conflict> {
    match (ea, eb) {
        (Edit::Insert(insert), Edit::Delete(deleted)) => {
            let (parent_o, _) = insert_anchor(ctx.a, ctx.f, *insert)?;
            (parent_o == *deleted).then(|| Conflict {
                span_o: Some(ctx.o.span(*deleted)),
                span_a: Some(ctx.a.span(*insert)),
                span_b: None,
                rule: "insert-delete",
            })
        }
        (Edit::Delete(deleted), Edit::Insert(insert)) => {
            let (parent_o, _) = insert_anchor(ctx.b, ctx.g, *insert)?;
            (parent_o == *deleted).then(|| Conflict {
                span_o: Some(ctx.o.span(*deleted)),
                span_a: None,
                span_b: Some(ctx.b.span(*insert)),
                rule: "insert-delete",
            })
        }
        _ => None,
    }
}

/// delete-delete (split deletion): connected regions of deleted O
/// nodes that overlap across branches without coinciding. Each branch
/// deleted part of something the other deleted differently — neither
/// deletion subsumes cleanly.
fn delete_delete(ctx: &Ctx, edits_a: &[Edit], edits_b: &[Edit], found: &mut Vec<Conflict>) {
    let components_a = deletion_components(ctx.o, edits_a);
    let components_b = deletion_components(ctx.o, edits_b);
    for ca in &components_a {
        for cb in &components_b {
            if ca == cb || ca.is_disjoint(cb) {
                continue;
            }
            let union: Vec<NodeId> = ca.union(cb).copied().collect();
            let conflict = Conflict {
                span_o: covering_span(ctx.o, &union),
                span_a: None,
                span_b: None,
                rule: "delete-delete",
            };
            if !found.contains(&conflict) {
                found.push(conflict);
            }
        }
    }
}

/// Connected components (by parent-child edges within the deleted set)
/// of one branch's deleted O nodes.
fn deletion_components(o: &Tree, script: &[Edit]) -> Vec<HashSet<NodeId>> {
    let deleted: HashSet<NodeId> = script
        .iter()
        .filter_map(|edit| match edit {
            Edit::Delete(n) => Some(*n),
            _ => None,
        })
        .collect();
    let mut component_of: HashMap<NodeId, usize> = HashMap::new();
    let mut components: Vec<HashSet<NodeId>> = Vec::new();
    // Pre-order: a deleted parent's component exists before its
    // children look it up.
    for node in o.nodes() {
        if !deleted.contains(&node) {
            continue;
        }
        let component = o
            .parent(node)
            .and_then(|parent| component_of.get(&parent).copied())
            .unwrap_or_else(|| {
                components.push(HashSet::new());
                components.len().saturating_sub(1)
            });
        if let Some(set) = components.get_mut(component) {
            set.insert(node);
        }
        component_of.insert(node, component);
    }
    components
}

/// The arity/category backstop: every merged node whose child list
/// changed relative to its origin must still satisfy the grammar's
/// node-types.json — required fields present exactly as often as
/// allowed, every child's kind admitted by its slot. This is the
/// grammar-driven stand-in for per-language syntactic-consistency
/// rules and catches structurally invalid combinations no pairwise
/// rule names.
pub(crate) fn arity(o: &Tree, a: &Tree, b: &Tree, merged: &MergedTree) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let mut stack = vec![merged.root()];
    while let Some(id) = stack.pop() {
        stack.extend(merged.children(id).iter().copied());
        let (tree, node) = resolve(o, a, b, merged.origin(id));
        if !children_changed(o, a, b, merged, id) {
            continue;
        }
        if let Some(node_type) = tree.lang().node_types().get(tree.kind(node))
            && !node_satisfies(o, a, b, merged, id, tree, node_type)
        {
            let span = tree.span(node);
            let conflict = match merged.origin(id) {
                Origin::O(_) => Conflict {
                    span_o: Some(span),
                    span_a: None,
                    span_b: None,
                    rule: "arity",
                },
                Origin::A(_) => Conflict {
                    span_o: None,
                    span_a: Some(span),
                    span_b: None,
                    rule: "arity",
                },
                Origin::B(_) => Conflict {
                    span_o: None,
                    span_a: None,
                    span_b: Some(span),
                    rule: "arity",
                },
            };
            if !conflicts.contains(&conflict) {
                conflicts.push(conflict);
            }
        }
    }
    conflicts
}

/// Resolves an origin tag to the tree and node it points at.
fn resolve<'t>(o: &'t Tree, a: &'t Tree, b: &'t Tree, origin: Origin) -> (&'t Tree, NodeId) {
    match origin {
        Origin::O(n) => (o, n),
        Origin::A(n) => (a, n),
        Origin::B(n) => (b, n),
    }
}

/// Whether a merged node's child list differs from its origin node's —
/// only changed nodes are validated, so grammar corners our
/// node-types model is too strict about cannot flag untouched code.
fn children_changed(o: &Tree, a: &Tree, b: &Tree, merged: &MergedTree, id: MergedId) -> bool {
    let (tree, node) = resolve(o, a, b, merged.origin(id));
    let expected = tree.children(node);
    let actual = merged.children(id);
    if actual.len() != expected.len() {
        return true;
    }
    actual.iter().zip(expected).any(|(&mc, &ec)| {
        let same_tree = match (merged.origin(id), merged.origin(mc)) {
            (Origin::O(_), Origin::O(n))
            | (Origin::A(_), Origin::A(n))
            | (Origin::B(_), Origin::B(n)) => Some(n),
            _ => None,
        };
        same_tree != Some(ec)
    })
}

/// Whether a merged node's children satisfy its node-types entry.
fn node_satisfies(
    o: &Tree,
    a: &Tree,
    b: &Tree,
    merged: &MergedTree,
    id: MergedId,
    parent_tree: &Tree,
    node_type: &crate::lang::NodeType,
) -> bool {
    let node_types = parent_tree.lang().node_types();
    let language = parent_tree.lang().language();

    // Each merged child resolved to (field name, kind, named). A
    // child's field id travels with its origin node; a field this
    // parent kind does not define (a graft that came from a different
    // context) counts as loose so the children slot still vets it.
    let members: Vec<(Option<&str>, &str, bool)> = merged
        .children(id)
        .iter()
        .map(|&child| {
            let (tree, node) = resolve(o, a, b, merged.origin(child));
            let field = tree
                .field_id(node)
                .and_then(|fid| language.field_name_for_id(fid.get()))
                .filter(|name| node_type.fields.contains_key(*name));
            (field, tree.kind(node), tree.is_named(node))
        })
        .collect();

    for (name, slot) in &node_type.fields {
        let filled: Vec<_> = members
            .iter()
            .filter(|(field, _, _)| field.is_some_and(|f| f == name))
            .collect();
        if slot.required && filled.is_empty() {
            return false;
        }
        if !slot.multiple && filled.len() > 1 {
            return false;
        }
        if filled
            .iter()
            .any(|(_, kind, _)| !node_types.admits(&slot.types, kind))
        {
            return false;
        }
    }

    let loose: Vec<_> = members
        .iter()
        .filter(|(field, _, named)| field.is_none() && *named)
        .collect();
    if let Some(slot) = &node_type.children {
        if slot.required && loose.is_empty() {
            return false;
        }
        if loose
            .iter()
            .any(|(_, kind, _)| !node_types.admits(&slot.types, kind))
        {
            return false;
        }
    }

    true
}

/// insert-insert: both branches inserted at the same slot — same
/// parent image, same nearest preceding matched sibling — but the
/// inserted subtree sequences differ. Equal sequences at the same
/// slot are one edit made twice and dedupe instead.
///
/// Under a commutative parent (imports, use declarations, type
/// members, JSON object entries) differing sequences are no conflict
/// either: child order carries no meaning there, so the builder
/// merges the two runs as a union — provided both runs decompose into
/// element groups. A run that does not (a trailing separator, or a
/// trailing attribute with its target outside the run) cannot be
/// unioned without breaking what a branch wrote.
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
        if ctx.o.lang().is_commutative(ctx.o.kind(anchor.0))
            && element_groups(ctx.a, seq_a).is_some()
            && element_groups(ctx.b, seq_b).is_some()
        {
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

/// name-collision: both branches inserted same-named definitions
/// under the same O parent at different slots. The builder grafts
/// both, yielding duplicate definitions — two `class Hello`, a JSON
/// object holding the same key twice — that no slot-based rule sees:
/// insert-insert wants a shared slot, and dedupe cannot merge grafts
/// landing in different places. A node's "name" is its `name` or
/// `key` field child, keeping the rule grammar-driven; that is
/// deliberately coarser than real signatures, so concurrently
/// inserting two distinct same-name Java overloads conflicts too.
/// Same-slot pairs are skipped under a non-commutative parent (equal
/// sequences dedupe and unequal ones are already insert-insert
/// conflicts); under a commutative parent unequal sequences merge, so
/// two same-named insertions must conflict here — unless their whole
/// element groups are equal, which is exactly what the builder's
/// dedupe merges into one copy. Comparing groups rather than the
/// named nodes alone keeps an attributed insertion distinct from a
/// bare one.
fn name_collision(ctx: &Ctx, edits_a: &[Edit], edits_b: &[Edit], found: &mut Vec<Conflict>) {
    let names_a = insert_names(ctx.a, ctx.f, edits_a);
    let names_b = insert_names(ctx.b, ctx.g, edits_b);
    let slots_a = insert_slots(ctx.a, ctx.f, edits_a);
    let slots_b = insert_slots(ctx.b, ctx.g, edits_b);
    for (name, inserts_a) in &names_a {
        let Some(inserts_b) = names_b.get(name) else {
            continue;
        };
        for (slot_a, node_a) in inserts_a {
            for (slot_b, node_b) in inserts_b {
                if slot_a == slot_b
                    && (!ctx.o.lang().is_commutative(ctx.o.kind(slot_a.0))
                        || dedupes_together(
                            (ctx.a, &slots_a, *node_a),
                            (ctx.b, &slots_b, *node_b),
                            slot_a,
                        ))
                {
                    continue;
                }
                let conflict = Conflict {
                    span_o: Some(ctx.o.span(name.0)),
                    span_a: Some(ctx.a.span(*node_a)),
                    span_b: Some(ctx.b.span(*node_b)),
                    rule: "name-collision",
                };
                if !found.contains(&conflict) {
                    found.push(conflict);
                }
            }
        }
    }
}

/// Whether two same-slot insertions sit in equal element groups, so
/// the builder's group-wise dedupe merges them into one copy.
fn dedupes_together(
    a: (&Tree, &HashMap<Slot, Vec<NodeId>>, NodeId),
    b: (&Tree, &HashMap<Slot, Vec<NodeId>>, NodeId),
    slot: &Slot,
) -> bool {
    let group_hashes = |(tree, slots, node): (&Tree, &HashMap<Slot, Vec<NodeId>>, NodeId)| {
        let run = slots.get(slot)?;
        let groups = element_groups(tree, run)?;
        let group = groups.into_iter().find(|group| group.contains(&node))?;
        Some(
            group
                .iter()
                .map(|&member| tree.hash(member))
                .collect::<Vec<u64>>(),
        )
    };
    let hashes_a = group_hashes(a);
    hashes_a.is_some() && hashes_a == group_hashes(b)
}

/// duplicate-insert: both branches inserted the identical nameless
/// element — a use declaration, an import — under the same
/// commutative O parent at different slots. Cross-branch dedupe only
/// merges grafts sharing a slot and name-collision only sees elements
/// with a `name`/`key` field, so without this rule the union merge
/// silently lands the element twice. Separators are excluded (every
/// comma equals every other comma); named elements are
/// name-collision's domain.
fn duplicate_insert(ctx: &Ctx, edits_a: &[Edit], edits_b: &[Edit], found: &mut Vec<Conflict>) {
    let elements = |tree: &Tree, matching: &Matching, script: &[Edit]| {
        let mut map: HashMap<(NodeId, u64), Vec<(Slot, NodeId)>> = HashMap::new();
        for edit in script {
            let Edit::Insert(node) = edit else {
                continue;
            };
            if !tree.is_named(*node) || node_name(tree, *node).is_some() {
                continue;
            }
            let Some(slot) = insert_anchor(tree, matching, *node) else {
                continue;
            };
            if !ctx.o.lang().is_commutative(ctx.o.kind(slot.0)) {
                continue;
            }
            map.entry((slot.0, tree.hash(*node)))
                .or_default()
                .push((slot, *node));
        }
        map
    };
    let elements_a = elements(ctx.a, ctx.f, edits_a);
    let elements_b = elements(ctx.b, ctx.g, edits_b);
    for (key, inserts_a) in &elements_a {
        let Some(inserts_b) = elements_b.get(key) else {
            continue;
        };
        for (slot_a, node_a) in inserts_a {
            for (slot_b, node_b) in inserts_b {
                if slot_a == slot_b {
                    continue;
                }
                let conflict = Conflict {
                    span_o: Some(ctx.o.span(key.0)),
                    span_a: Some(ctx.a.span(*node_a)),
                    span_b: Some(ctx.b.span(*node_b)),
                    rule: "duplicate-insert",
                };
                if !found.contains(&conflict) {
                    found.push(conflict);
                }
            }
        }
    }
}

/// A branch's named top-level insertions, keyed by the O parent they
/// graft under plus the name text, valued by (slot, inserted node).
fn insert_names<'t>(
    tree: &'t Tree,
    matching: &Matching,
    script: &[Edit],
) -> HashMap<(NodeId, &'t str), Vec<(Slot, NodeId)>> {
    let mut names: HashMap<(NodeId, &'t str), Vec<(Slot, NodeId)>> = HashMap::new();
    for edit in script {
        let Edit::Insert(node) = edit else {
            continue;
        };
        let Some(slot) = insert_anchor(tree, matching, *node) else {
            continue;
        };
        let Some(name) = node_name(tree, *node) else {
            continue;
        };
        names.entry((slot.0, name)).or_default().push((slot, *node));
    }
    names
}

/// The text of a node's `name` or `key` field child, if it has one.
fn node_name(tree: &Tree, node: NodeId) -> Option<&str> {
    let language = tree.lang().language();
    tree.children(node).iter().find_map(|&child| {
        let field = tree
            .field_id(child)
            .and_then(|fid| language.field_name_for_id(fid.get()))?;
        if field == "name" || field == "key" {
            tree.source_slice(tree.span(child))
        } else {
            None
        }
    })
}

/// Groups a branch's top-level insertions by slot, in sibling order
/// (the edit script lists inserts in pre-order, so same-slot siblings
/// arrive left to right).
fn insert_slots(tree: &Tree, matching: &Matching, script: &[Edit]) -> HashMap<Slot, Vec<NodeId>> {
    let mut slots: HashMap<Slot, Vec<NodeId>> = HashMap::new();
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

/// Splits an inserted run into element groups: each group is a named
/// node together with the anonymous separators and forward-binding
/// prefixes preceding it (a JSON insertion is `, "k": v` — the comma
/// travels with its pair; a Rust `#[attr]` travels with the item it
/// governs). `None` when trailing prefix nodes remain: a separator
/// with no element cannot participate in a union merge, and a
/// trailing attribute governs something *outside* the run, which a
/// union would displace. Group-wise dedupe in the builder relies on
/// the same decomposition.
pub(crate) fn element_groups(tree: &Tree, run: &[NodeId]) -> Option<Vec<Vec<NodeId>>> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    for &node in run {
        current.push(node);
        if tree.is_named(node) && !tree.lang().binds_forward(tree.kind(node)) {
            groups.push(std::mem::take(&mut current));
        }
    }
    current.is_empty().then_some(groups)
}

/// The byte range covering a sequence of nodes.
fn covering_span(tree: &Tree, seq: &[NodeId]) -> Option<std::ops::Range<usize>> {
    let start = seq.iter().map(|&n| tree.span(n).start).min()?;
    let end = seq.iter().map(|&n| tree.span(n).end).max()?;
    Some(start..end)
}

/// The slot an inserted node lands in. `None` for nested insertions
/// (their parent is inserted too — they ride along with the
/// top-level graft).
fn insert_anchor(tree: &Tree, matching: &Matching, node: NodeId) -> Option<Slot> {
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
