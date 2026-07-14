# Comparison with mergiraf

*2026-07-14, d3j at the v1 plan-complete state, mergiraf 0.18.0.*

This note records a first head-to-head between d3j and
[mergiraf][mergiraf], the structural merge tool d3j most resembles.
It covers merge outcomes on the scenario corpus and on hand-built
probe scenarios, output fidelity, and speed. It does not attempt a
large-scale evaluation on real merge histories; that remains future
work alongside the paper-replication datasets.

[mergiraf]: https://mergiraf.org/

Summary: d3j merges the insert-adjacent-to-relabel cases mergiraf
punts on, catches a bad merge mergiraf produces silently, and runs
about an order of magnitude faster — but the comparison exposed that
d3j silently loses an edit when the other branch deleted (or moved)
the enclosing node, a case both mergiraf and git flag as a conflict.

## Method

Both tools ran over `tests/corpus/scenarios/` and the probe scenarios
listed inline below. Invocations:

```
d3j merge O A B
mergiraf merge O A B -t 0
```

`-t 0` disables mergiraf's timeout fallback to git's textual merge,
so every mergiraf result below is its structural algorithm. (With the
default timeout the results were identical.) mergiraf ships prebuilt
binaries on [Codeberg releases][releases].

[releases]: https://codeberg.org/mergiraf/mergiraf/releases

Merged outputs were compared byte-for-byte against each scenario's
`expected.*`, and mergiraf's merged outputs were additionally run
through `d3j check` to judge them against the universality
conditions.

## Corpus results

The following table shows each scenario's outcome. "OK" means the
tool did what `expected.*` calls for: a clean merge matching the
expected output, or a conflict where `expected.CONFLICT` says so.

| Scenario | d3j | mergiraf |
| --- | --- | --- |
| delete-delete (rs) | OK (conflict) | OK (conflict) |
| doc-comments (rs) | OK, whitespace wart | OK, byte-exact |
| figure-10 (json) | OK, whitespace wart | OK, byte-exact |
| figure-2-3 (json) | OK (merged) | conflict — see below |
| insert-dedupe (json) | OK (merged) | OK (merged) |
| insert-delete-java | OK (conflict) | bad merge — see below |
| insert-delete (rs) | OK (conflict) | OK (conflict) |
| insert-insert (rs) | OK (conflict) | OK (conflict) |
| relabel-relabel (rs) | OK (conflict) | OK (conflict) |
| rename-dedupe (rs) | OK (merged) | OK (merged) |

The "whitespace wart" rows are the known synthesis trivia issues
(`{} fn keep` single-space graft boundary, `[ 2, 3]` leading space);
the corpus test compares structurally, and both outputs parse to the
expected shape.

### figure-2-3: pushout composition vs punting

The corpus scenario reproduces the motivating example where A inserts
next to a node B relabels:

```
O: [1, 2, 3]
A: [1, 2, 4, 5, 3]     (insert 4, 5 after 2)
B: [1, 6, 3]           (relabel 2 → 6)
```

d3j merges to `[1, 6, 4, 5, 3]`. mergiraf emits a whole-file
conflict. Composing an insertion with a relabel of its neighbor is
exactly what the pushout construction buys.

### insert-delete-java: a silent bad merge, caught by the checker

A inserts a method into the inner class B deletes:

```
O: class O { class I { } }
A: class O { class I { void m() { } } }
B: class O { }
```

d3j raises its insert-delete conflict. mergiraf merges cleanly to
`class O { void m() { } }` — the inserted method survives, but hoisted
into the parent class, a scope it was never written in. Running
`d3j check` on mergiraf's output rejects it from both directions:

```
extra deletion: both branches kept the node at 8..23 but the merge lost it: "{ class I { } }"
missed deletion: a branch deleted the node at 18..21 but the merge kept it: "{ }"
```

Besides the specific result, this demonstrates that `d3j check`
functions as an external judge of other tools' merges.

## Probe: relabel-under-delete loses edits silently

The comparison's most important finding is a d3j gap. d3j has no
conflict rule for a relabel inside a subtree the other branch
deleted, so the deletion wins and the edit disappears from a
conflict-free merge. Two probes hit the path.

Plain delete-vs-edit, which git and mergiraf both flag as a
delete/modify conflict:

```
O: fn alpha() {          A: fn alpha() {          B: fn alpha() {
       println!("alpha");        println!("alpha");        println!("alpha");
   }                         }                         }

   fn beta() {                                         fn beta() {
       println!("beta");                                   println!("beta v2");
   }                                                   }
```

d3j merges clean to A's version — `fn beta` gone, B's edit gone with
it. mergiraf conflicts on the beta region.

Move-plus-edit, where A only *reorders*:

```
O: fn alpha() ...        A: fn beta() {            B: fn alpha() ...
   fn beta() {                  println!("beta");      fn beta() {
       println!("beta");    }                              println!("beta v2");
   }                        fn alpha() ...            }
```

The two anchor matches cross, the admissibility guard demotes one,
A's move degrades to delete+insert, and B's edit vanishes into the
fake deletion: d3j merges clean with beta's *old* body. mergiraf
produces the ideal merge — beta moved up, with `"beta v2"`.

`d3j check` accepts both lossy outputs. Deletion-wins is a valid
pushout, so the universality conditions cannot see the loss; the
headline property "no incorrect conflict-free merges" holds only
relative to a correctness notion that blesses deletion-wins. Two
complementary fixes:

- A relabel-under-delete conflict rule catches both probes cheaply,
  at the cost of conflicting on the move case rather than merging it.
- Move detection (already the planned next milestone) makes the move
  probe merge correctly instead of conflicting.

## Formatting fidelity

mergiraf reproduced the expected bytes exactly on every clean corpus
merge. Beyond d3j's pinned trivia warts, a probe shows d3j discarding
a reformat-only branch's layout when the other branch made the
structural edit:

```
O: {"name": "d3j", "version": 1, "debug": false}
A: same content, pretty-printed across five lines
B: {"name": "d3j", "version": 2, "debug": false}
```

d3j outputs B's bytes with the edit (`"version": 2`, single line),
dropping A's reformatting. mergiraf keeps both: pretty-printed with
`"version": 2`. A reformat is trivia-only, so the tree-level pushout
cannot represent it; improving this means teaching synthesis to
prefer the reformatted branch's trivia for otherwise-unchanged nodes.

## Speed and coverage

On a realistic input — the 1,297-line `src/diff.rs` with renames
applied to two adjacent functions as A and B — both tools produce
byte-identical, correct merges. d3j finishes in ~46ms, mergiraf in
~570ms (aarch64 Linux container; single run, but the gap is far
outside run-to-run noise).

mergiraf supports roughly 30 languages. d3j supports three (Rust,
Java, JSON).

## Follow-ups

- ~~Add the relabel-under-delete conflict rule; turn the two probes
  above into corpus scenarios~~ — done, see the update below.
- Revisit the move probe when move detection lands; it should then
  merge cleanly with the edit preserved.
- ~~Consider trivia-preference synthesis for reformat-only
  branches~~ — done for pure reformat-only branches, see below.
- A corpus-scale rerun of this comparison is cheap; rerun it after
  each of the above.

## Update, later the same day

Three of the follow-ups landed, and a sweep of mergiraf's own example
suite (52 Rust/Java/JSON cases under its `examples/` directory, run
in place rather than vendored — mergiraf is GPL-3.0 and d3j declares
no license) found one more gap class.

What landed:

- The `relabel-delete` pair rule: a relabel inside a subtree the
  other branch deleted now conflicts instead of silently vanishing.
  Both probes above are corpus scenarios (`relabel-delete`,
  `move-loses-edit`).
- The `name-collision` aggregate rule, prompted by the suite sweep:
  five of six silent bad merges d3j produced on mergiraf's examples
  were both branches inserting a same-named definition at *different*
  slots — two `class Hello`, a JSON object holding one key twice.
  Slot-based insert-insert never fired. The rule keys inserted nodes
  by their `name`/`key` field child; deliberately coarser than real
  signatures (concurrent same-name Java overload insertions conflict
  too, and identical insertions at different slots conflict rather
  than dedupe, where mergiraf merges them).
- Reformat preference: when exactly one branch made no tree edits but
  its bytes differ from O, merge origins remap to that branch, so its
  layout survives the other branch's edits. The reformat-edit probe
  above now produces mergiraf's ideal output. Concurrent distinct
  reformats still resolve to A's layout — span synthesis cannot honor
  two reformats of the same code at once.

Remaining gaps the suite sweep exposed, in rough order of value:

- ~~Commutative-list merging~~ — done, see the second update below.
- Comment edits are invisible. `use crate::{foo, /* bar */}` vs
  `/* baz */`: comments are trivia, so d3j sees two identical
  comma-inserts, dedupes, and outputs `use crate::{foo,};` — both
  branches' comments dropped, and mergiraf's expected conflict never
  fires. Needs comment-aware matching or synthesis.
- Signature collisions via composition. mergiraf's
  `conflicting_method_signatures`: A adds a parameter to `run`, B
  renames `run` to `runNow`; the composed `runNow(Environment env)`
  duplicates a separately-inserted method of the same name.
  name-collision only inspects insert-vs-insert pairs, so catching
  this needs post-merge name-uniqueness validation over the merged
  tree (arity-style), which is the "Java conflict-rule depth"
  milestone.

After the fixes, d3j handles all 14 corpus scenarios correctly
(mergiraf's clean merge on `move-loses-edit` is its move detection
producing the ideal result — d3j stays conflict-safe there until move
detection lands).

## Second update: commutative-list merging

Concurrent insertions at the same slot under a *commutative parent* —
a node kind whose child order carries no meaning — now merge as a
union instead of conflicting. Each language declares its kinds: Rust
`source_file`, `declaration_list`, `use_list`, `trait_bounds`; Java
`program`, `class_body`, `interface_body`, `type_list`, `throws`;
JSON `object`. Both branches adding imports, use declarations, type
members, trait bounds, implemented interfaces, or object keys next to
each other now merges, A's insertions before B's.

The construction leans on machinery that was already there: the
builder always grafted A's then B's same-slot insertions with
cross-branch dedupe — the insert-insert rule was the only blocker.
What the union actually required:

- Element groups. A branch's inserted run decomposes into elements: a
  named node plus the anonymous separators before it (a JSON insert is
  `, "k": v` — the comma travels with its pair) and any forward-binding
  prefix (a Rust `#[attr]` travels with the item it governs). Dedupe
  is now group-wise, so two branches both adding `use c;` alongside
  their own additions share one copy without a stray comma absorbing
  an unrelated twin. A run that does not decompose — a trailing
  separator (prepending into an object), or a trailing attribute whose
  target sits outside the run — refuses to union and conflicts as
  before. The attribute case is real: mergiraf's `attributes` example
  had A attributing the surviving function while B inserted above it,
  and a naive union silently moved `#[cfg(...)]` onto B's code.
- name-collision now covers the shared slot. Same-slot insertions of
  one name used to be insert-insert's problem; in a union they merge,
  so two different `fn foo` at one slot must conflict here. The rule
  compares whole element groups — exactly what the builder dedupes —
  so an attributed insertion stays distinct from a bare one.
- A duplicate-insert rule. Identical *nameless* elements (use
  declarations, imports) inserted by both branches at different slots
  under a commutative parent would land twice — name-collision only
  sees `name`/`key` fields. mergiraf dedupes these; d3j conflicts, to
  stay consistent with its named-element behavior.
- Fungible separators in the checker. With both branches contributing
  a comma each, the self-check's independently derived matchings let
  both claim the same merged comma, orphaning its twin as a bogus
  "extra insertion". Anonymous tokens under commutative parents are
  now exempt from the universality conditions: their attribution is
  ambiguous by construction, and the parse itself pins how many the
  output needs.

On mergiraf's 54-case example suite this moves d3j from 24 correct
outcomes to 33; conflicts-where-mergiraf-merges drop from 24 to 16.
The survivors are mergiraf edges d3j does not model yet: derive
arguments (the argument list is a macro `token_tree`, which cannot be
blanket-commutative), identical elements inserted at different slots
(d3j deliberately conflicts where mergiraf dedupes), move detection,
comment attachment, and normalizing away a branch's insertion that
duplicates something already in base (mergiraf drops it; d3j
faithfully keeps the branch's edit).
