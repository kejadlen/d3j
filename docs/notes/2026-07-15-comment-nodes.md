# Comments as opaque leaf nodes

*2026-07-15.*

Comments were tree-sitter `extra` nodes, skipped at lift and carried
only as inter-leaf trivia by span synthesis. That made comment *edits*
invisible to the diff — the top remaining gap from the mergiraf
comparison. The motivating failure: `use crate::{foo, /* bar */}` vs
`/* baz */` looked like two identical comma-inserts, deduped, and the
output dropped both comments where mergiraf conflicts.

## Decision

Lift extras as **opaque labeled leaves**: keep the extra node, do not
lift its children, and let `label()` return its full source text.
Everything downstream then treats a comment like any other labeled
leaf — a comment edit is a relabel, concurrent distinct edits are a
relabel-relabel conflict, and comment inserts ride the existing graft
machinery.

Opacity is what makes the representation uniform. Java and JSON
comments are already leaves carrying their text, but rust comments
have interior structure whose *body text lives in no leaf*: `// plain`
parses as `line_comment` with a single anonymous `//` child, and
`/* bar */` as `block_comment` holding only the `/*` and `*/` tokens.
Lifted naively, two different comments would be structurally
identical — same kinds, no labels — and the edit would stay invisible.
Doc comments (`/// doc`) do carry an inner `doc_comment` leaf, but
plain comments are the common case, and one representation for all
comments beats two.

## Consequences through the pipeline

- Hashing and anchoring see comment text via the label; a unique
  comment can anchor, duplicated ones (`// TODO`) are excluded by the
  uniqueness rule as usual.
- Phase 2's (kind, label) LCS never pairs comments whose text differs;
  phase 3's Zhang–Shasha folds them as relabels within the comment
  kind. The relabel-relabel and relabel-delete rules then cover
  concurrent comment edits and edit-under-delete for free.
- The arity rule must skip extra children: extras fill no grammar
  slot and `node-types.json` does not admit them anywhere, so a
  changed node holding a comment would otherwise false-conflict.
- Element groups treat extras as forward-binding prefixes, like rust
  attributes: a standalone comment describes the item below it, and a
  union merge must not splice the other branch's code between a doc
  comment and its function. A run *ending* in a comment refuses to
  group — which is exactly what turns the `use_list` probe into the
  insert-insert conflict mergiraf expects, instead of a broken union.
- duplicate-insert exempts extras: both branches adding an identical
  `// section` comment at different slots is legitimate, not a
  duplicate definition.
- A branch that only edits comment text is no longer "edit-free", so
  the reformat preference does not fire for it; the edit lands through
  the normal relabel path instead, which is strictly better.
- The corpus's structural comparison (`Shape`) now covers comments, so
  scenarios assert comment content, not just code shape.

## Rejected alternative

mergiraf-style comment *attachment* (folding a comment's text into the
hash/identity of the node it documents). It makes comment edits look
like edits of the attached code — coarser conflicts — and needs
per-language attachment heuristics up front. Attachment may still be
worth adding later for *matching* quality (a moved function should
carry its doc comment), which is the move-detection milestone's
territory.
