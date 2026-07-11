# d3j implementation plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to
> implement this plan task-by-task.

**Goal:** A working `d3j merge O A B` / `d3j check O A B M` binary
implementing the design in `2026-07-10-d3j-design.md`.

**Architecture:** Library crate with a thin CLI. Parse with
tree-sitter, lift to an owned arena tree, diff via anchored matching
(hash anchors → recursive alignment → bounded Zhang–Shasha),
construct the merge as a pushout under conflict rules, synthesize
output from origin byte spans, and self-check every merge with the
universality checker before emitting.

**Tech stack:** Rust edition 2024, tree-sitter ~0.25 plus grammar
crates (rust, java, json), clap, thiserror + miette, hegeltest for
property tests. Toolchain per the `rust` skill (just, grcov coverage
gate, cargo-mutants).

**Deviation from the rust skill:** no tokio. d3j is a synchronous,
CPU-bound merge driver where startup latency matters; `main` is a
plain `fn main() -> miette::Result<()>`. If this feels wrong, veto
before Task 0.

**Read first:** `docs/plans/2026-07-10-d3j-design.md` (the design),
especially the correctness criteria section. The paper is at
https://arxiv.org/abs/2607.07987 if a rule's rationale is unclear.

**Conventions for every task:**
- TDD: failing test → verify failure → minimal implementation →
  verify pass → commit. No implementation before its test exists.
- Commit with `jj commit` per the `commit` and `describing-changes`
  skills (plain-sentence subject, `Assisted-by` trailer). The
  `git commit` examples below are shorthand for message content only.
- Run `just clippy` before every commit; the panic-discipline lints
  (`unwrap_used`, `indexing_slicing`, ...) are warnings that CI turns
  into errors.
- tree-sitter API details drift between minor versions. When a
  snippet below doesn't compile, trust `docs.rs/tree-sitter` over the
  plan and note the correction in the final report.

---

### Task 0: Scaffold the project

**Files:**
- Create: `Cargo.toml`, `justfile`, `bin/coverage`, `.gitignore`,
  `src/lib.rs`, `src/bin/d3j/main.rs`, `.github/workflows/ci.yml`

**Step 1: Scaffold per the `rust` skill**

Follow `rust` skill `scaffolding.md` + `binary.md` exactly, except:
no tokio, no `build.rs`/versioning yet (later milestone), no
release workflow yet.

```toml
[package]
name = "d3j"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "d3j"
path = "src/bin/d3j/main.rs"

[dependencies]
clap = { version = "*", features = ["derive", "env"] }
fs-err = "*"
miette = { version = "*", features = ["fancy"] }
serde = { version = "*", features = ["derive"] }
serde_json = "*"
thiserror = "*"
tracing = "*"
tracing-subscriber = { version = "*", features = ["env-filter"] }
tree-sitter = "*"
tree-sitter-java = "*"
tree-sitter-json = "*"
tree-sitter-rust = "*"

[dev-dependencies]
assert_cmd = "*"
hegeltest = "*"
predicates = "*"
tempfile = "*"

[lints.clippy]
self_named_module_files = "warn"
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
indexing_slicing = "warn"
arithmetic_side_effects = "warn"
```

Copy the justfile and `bin/coverage` from the skill (`coverage.sh`
reference file, `chmod +x`). `.gitignore` gains `/target` (keep the
existing PDF line). `src/lib.rs` is empty module decls; `main.rs` is
a stub `fn main() -> miette::Result<()> { Ok(()) }` with
`miette::set_panic_hook()` and tracing init per `binary.md` (minus
tokio and subcommands for now).

**Step 2: Verify** — `cargo build && just clippy` both green.

**Step 3: Commit** — "Scaffold the crate".

---

## Milestone 1: parse and lift

### Task 1: Error enum

**Files:** Create `src/error.rs`, modify `src/lib.rs`.

**Step 1: Failing test** (in `src/error.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_with_diagnostic_codes() {
        let err = Error::UnknownLanguage { path: "x.zig".into() };
        assert!(err.to_string().contains("x.zig"));
    }
}
```

**Step 2:** `cargo test` — fails: `Error` not defined.

**Step 3: Implement** the full v1 error space (design doc "Error
space" section):

```rust
use std::path::PathBuf;
use miette::Diagnostic;

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum Error {
    #[error("cannot detect language for {path}")]
    #[diagnostic(
        code(d3j::unknown_language),
        help("pass --lang; supported: rust, java, json")
    )]
    UnknownLanguage { path: PathBuf },

    #[error("{path} does not parse as {lang}")]
    #[diagnostic(
        code(d3j::parse),
        help("structural merge requires syntactically valid inputs")
    )]
    Parse { path: PathBuf, lang: String },

    #[error("io error on {path}: {source}")]
    #[diagnostic(code(d3j::io))]
    Io { path: PathBuf, #[source] source: std::io::Error },
}
```

`lib.rs`: `pub mod error;` and `pub use error::Error;`.

**Step 4:** test passes. **Step 5:** Commit.

### Task 2: Language registry

**Files:** Create `src/lang.rs`, test in-module.

Registry maps extension → tree-sitter `Language` + parsed
`node-types.json` metadata (every grammar crate exports `NODE_TYPES:
&str`).

**Step 1: Failing tests:**

```rust
#[test]
fn detects_language_from_extension() {
    assert_eq!(Lang::detect(Path::new("foo.rs")).unwrap().name(), "rust");
    assert_eq!(Lang::detect(Path::new("Foo.java")).unwrap().name(), "java");
    assert!(Lang::detect(Path::new("foo.zig")).is_none());
}

#[test]
fn node_types_metadata_is_loaded() {
    let lang = Lang::by_name("rust").unwrap();
    // binary_expression has fields => fixed slots exist for it.
    assert!(lang.node_types().has_fields("binary_expression"));
}
```

**Step 2:** fail. **Step 3: Implement:**

- `pub struct Lang { name, language: tree_sitter::Language, node_types: NodeTypes }`
- `Lang::detect(&Path) -> Option<&'static Lang>` by extension
  (`rs`/`java`/`json`), `Lang::by_name`. Use `std::sync::LazyLock`
  for the three static instances
  (`tree_sitter_rust::LANGUAGE.into()` etc.).
- `NodeTypes`: serde-deserialize the crate's `NODE_TYPES` JSON.
  Model only what the arity rule needs later: per node type, its
  `fields` (name → {required, multiple, types}) and `children`
  ({required, multiple, types}). Ignore the rest with
  `#[serde(default)]`.

**Step 4:** pass. **Step 5:** Commit.

### Task 3: Lift the CST to an arena tree

**Files:** Create `src/tree.rs`.

The core type from the design doc. Lift rules:
- include named **and** anonymous nodes (anonymous tokens like `+`
  carry meaning via their kind); skip `extra` nodes (comments) and
  reject trees containing error/missing nodes,
- a node is a *labeled leaf* iff it is named and has zero children
  in the lifted tree; its label is its source text,
- record `parent`, `children`, byte `span`, tree-sitter `kind_id`,
  and `field_id` (slot within parent).

**Step 1: Failing tests:**

```rust
fn parse_rust(src: &str) -> Tree {
    Tree::parse(src, Lang::by_name("rust").unwrap()).unwrap()
}

#[test]
fn lifts_a_simple_function() {
    let t = parse_rust("fn main() {}");
    let root = t.root();
    assert_eq!(t.kind(root), "source_file");
    // an identifier leaf carries its text as label
    let ids: Vec<_> = t.nodes()
        .filter(|&n| t.kind(n) == "identifier")
        .collect();
    assert_eq!(t.label(ids[0]), Some("main"));
}

#[test]
fn comments_are_excluded() {
    let t = parse_rust("// hello\nfn main() {}");
    assert!(t.nodes().all(|n| t.kind(n) != "line_comment"));
}

#[test]
fn parse_errors_are_rejected() {
    assert!(Tree::parse("fn main( {", Lang::by_name("rust").unwrap()).is_err());
}

#[test]
fn spans_reconstruct_source() {
    let src = "fn main() {}";
    let t = parse_rust(src);
    assert_eq!(&src[t.span(t.root())], src);
}
```

**Step 2:** fail. **Step 3: Implement** with a `TreeCursor` walk
(recursion on deep files blows the stack; use an explicit stack).
`Tree::parse` returns `Result<Tree, Error>`; walk once to detect
`is_error() || is_missing()` and reject. Store owned `source: String`.
Accessor methods (`kind`, `label`, `span`, `children`, `parent`,
`root`, `nodes`) rather than public fields — the diff code reads via
these constantly.

**Step 4:** pass. **Step 5:** Commit.

### Task 4: Subtree hashing

**Files:** Modify `src/tree.rs` (or new `src/hash.rs` if tree.rs
passes 300 lines).

Merkle hash per node over (kind_id, label, child hashes in order).
**Not** the span — position-independent so identical code in
different places collides deliberately.

**Step 1: Failing tests:**

```rust
#[test]
fn identical_subtrees_hash_equal_across_positions() {
    let t = parse_rust("fn a() { x(); }\nfn b() { x(); }");
    let calls: Vec<_> = t.nodes()
        .filter(|&n| t.kind(n) == "call_expression").collect();
    assert_eq!(t.hash(calls[0]), t.hash(calls[1]));
}

#[test]
fn different_labels_hash_differently() {
    let a = parse_rust("fn main() { x(); }");
    let b = parse_rust("fn main() { y(); }");
    assert_ne!(a.hash(a.root()), b.hash(b.root()));
}
```

**Step 2–4:** fail / implement (post-order pass at `parse` time,
`FxHasher`-style or `std` `DefaultHasher` is fine for v1) / pass.
**Step 5:** Commit.

---

## Milestone 2: diff

### Task 5: Matching type and invariants

**Files:** Create `src/diff.rs`.

`Matching` = bidirectional map src↔dst with invariants (these ARE
the "order-preserving partial inclusion map"):

1. bijective on its domain,
2. matched pairs have equal `kind_id`,
3. ancestry preserved: if x is an ancestor of y (both matched), then
   m(x) is an ancestor of m(y),
4. sibling order preserved: for matched x, y with the same parent,
   source order equals destination order.

**Step 1: Failing tests** — construct tiny trees by hand (add a
`Tree::from_toy(...)` test helper or parse minimal JSON files —
JSON grammar makes deterministic small trees: `[1, 2, 3]`).
Test `Matching::validate` accepts an order-preserving map and
rejects a crossing one.

**Step 2–4:** implement `Matching { src_to_dst: Vec<Option<NodeId>>,
dst_to_src: Vec<Option<NodeId>> }` (dense — NodeIds are arena
indices), `insert`, `get`, `validate(&TreeO, &TreeA) ->
Result<(), InvariantViolation>`. `validate` is test/debug-assert
machinery — merge-path code relies on construction being correct.
**Step 5:** Commit.

### Task 6: Diff phase 1 — hash anchors

**Files:** Modify `src/diff.rs`.

Match subtrees whose hash occurs exactly once in each tree
(unique-unique), largest first; skip candidates that would violate
invariant 3 or 4 against already-placed anchors (demote, don't
fail).

**Step 1: Failing tests:**

```rust
#[test]
fn unique_subtrees_anchor() {
    let o = parse_rust("fn a() {}\nfn b() {}");
    let a = parse_rust("fn a() {}\nfn c() {}\nfn b() {}");
    let m = anchor(&o, &a);
    // both fn a and fn b anchored wholesale
    assert!(matched_kinds(&m, &o).contains("function_item"));
}

#[test]
fn crossing_anchors_are_demoted() { /* construct a swap: O = [x, y], A = [y, x];
    the second anchor must be dropped, not create a crossing map */ }
```

**Step 2–4:** fail / implement / pass. **Step 5:** Commit.

### Task 7: Diff phase 2 — recursive alignment

**Files:** Modify `src/diff.rs`.

Top-down from the root pair: if both roots share `kind_id`, match
them, then align their child sequences by LCS keyed on subtree hash
(exact-equal subtrees) and second-pass LCS keyed on `kind_id`
(same-kind, recurse into the pair). Children left unaligned remain
unmatched for phase 3. Respect anchors from phase 1 (they are
fixed points the LCS must keep in order).

**Step 1: Failing tests** — the paper's Figure 2/3 scenario in JSON
(`[1,2,3]` → `[1,2,4,5,3]` and `[1,2,3]` → `[1,6,3]`): after
alignment, `1`/`3` (and the arrays and document nodes) are matched,
`4`,`5`,`6` unmatched-in-dst, `2` matched in the first pair,
unmatched in the second. Plus a Rust case: rename a function
(same-kind alignment matches the `function_item` so the identifier
pair becomes a relabel candidate).

**Step 2–4:** fail / implement / pass. **Step 5:** Commit.

### Task 8: Diff phase 3 — bounded Zhang–Shasha on residues

**Files:** Create `src/zs.rs`, modify `src/diff.rs`.

For each matched parent pair with unmatched child spans on both
sides, if the total unmatched node count is ≤ 400 (constant, tune
later), run textbook Zhang–Shasha (keyroots + treedist DP) over the
two forests with costs: 0 same-kind-same-label, 1 relabel
(same-kind different-label), ∞ cross-kind (never matched), 1
insert/delete. Fold the resulting mapping into the matching (again
demoting invariant violations). Over budget: leave unmatched
(delete+insert — conservative, never wrong).

**Step 1: Failing test** — a case phases 1–2 cannot catch: nested
restructure where a subtree moved one level deeper with an edit
inside it, e.g. JSON `{"a": [1, 2]}` → `{"a": [[1, 2, 3]]}` should
still match the `1` and `2` leaves.

**Step 2–4:** fail / implement / pass. ZS is the one genuinely
fiddly algorithm here; implement it standalone in `src/zs.rs`
against plain "label + children" test trees first, then wire in.
Cite: Zhang & Shasha, SIAM J. Comput. 18(6), 1989 — the paper's
reference [16].
**Step 5:** Commit (two commits fine: zs.rs standalone, then wiring).

### Task 9: Edit derivation and the round-trip property

**Files:** Modify `src/diff.rs`, create `tests/property.rs`.

```rust
pub enum Edit {
    Delete(NodeId),           // src node without image
    Insert(NodeId),           // dst node without preimage
    Relabel(NodeId, NodeId),  // matched, labels differ
}
pub fn diff(o: &Tree, a: &Tree) -> Matching;       // phases 1+2+3
pub fn edits(o: &Tree, a: &Tree, m: &Matching) -> Vec<Edit>;
```

**Step 1: Property test** (hegeltest — see rust skill
`property-testing.md`): generate a random source by picking a seed
file from `tests/corpus/seeds/` and applying random small textual
edits **that keep it parsable** (duplicate a top-level item, delete
a statement line, rename an identifier occurrence; re-parse to
filter). Property: for every generated (O, A):

1. `diff(o, a).validate(&o, &a)` holds, and
2. `apply(&o, &edits(...), &a)` reconstructs a tree
   structurally equal to A (same kinds, labels, shape — spans
   excluded). `apply` builds M from O by dropping deletes,
   rewriting relabels, and grafting inserts at
   (parent-image, sibling-index) positions read from A.

`apply` here is a test oracle but lives in `src/diff.rs` — the
merge pushout in Task 11 reuses exactly this grafting logic.

**Step 2–4:** fail / implement `edits` + `apply` / pass, plus fixed
unit cases: identical trees → zero edits; rename-only → exactly one
Relabel.
**Step 5:** Commit.

---

## Milestone 3: the checker

### Task 10: Universality checker

**Files:** Create `src/check.rs`.

Given trees O, A, B, M: compute `f = diff(O,A)`, `g = diff(O,B)`,
`i1 = diff(A,M)`, `i2 = diff(B,M)` and evaluate the four conditions
from the design doc:

1. **No extra insertion:** every M node has a preimage under i1 or i2.
2. **No missed insertion:** every A node unmatched in f has an image
   under i1; same for B/g/i2.
3. **No extra deletion:** every O node with images under both f and
   g — those images must map into M (via i1 and i2) and agree.
4. **No missed deletion:** every O node lacking an image in f or in
   g must not reach M through the other route.

(Condition 3's "agree" is the commutativity check: i1(f(x)) =
i2(g(x)) as M nodes.)

```rust
pub struct Report {
    pub parsable: bool,
    pub violations: Vec<Violation>, // enum + the witness NodeId + span
}
pub fn check(o: &Tree, a: &Tree, b: &Tree, m: &Tree) -> Report;
```

**Step 1: Failing tests** — hand-built JSON scenarios, one per
condition, each asserting exactly that violation is reported:
- extra insertion: M contains a node in neither A nor B
  (O=A=B=`[1]`, M=`[1, 9]`),
- missed insertion: paper Figure 10 (O=`[1,2]`… A inserts `3`, B
  deletes `1`, M=`[2]` — must flag; M=`[2,3]` — must pass),
- extra deletion: O=A=B=`[1,2]`, M=`[1]`,
- missed deletion: A deletes `1`, M keeps it.
Plus the happy path: Figure 2/3 (O=`[1,2,3]`, A=`[1,2,4,5,3]`,
B=`[1,6,3]`, M=`[1,6,4,5,3]`) → no violations.

**Step 2–4:** fail / implement / pass. **Step 5:** Commit.

**Note:** checker fidelity is bounded by diff quality (the paper
has the same caveat). The Figure-10 tests pin the diff behavior the
checker depends on; if they fail, fix `diff`, not `check`.

---

## Milestone 4: merge

### Task 11: Pushout construction, conflict-free path

**Files:** Create `src/merge.rs`.

```rust
pub enum MergeOutcome {
    Merged(Tree),               // origin-tagged, see Task 14
    Conflicts(Vec<Conflict>),
}
pub fn merge(o: &Tree, a: &Tree, b: &Tree) -> Result<MergeOutcome, Error>;
```

Algorithm (design doc "Pushout construction"):
1. `f = diff(o,a)`, `g = diff(o,b)`.
2. Survivors: O nodes with images under both f and g.
3. Relabels: apply each branch's relabel to the surviving node;
   identical relabels dedupe (conflict rule comes in Task 12).
4. Inserts: graft A-inserted and B-inserted subtrees at their
   parent's image, reusing Task 9's `apply` grafting. Dedupe equal
   insertions (same subtree hash, same anchor). Same-slot order:
   A's nodes before B's.
5. Every M node records origin: `O(NodeId) | A(NodeId) | B(NodeId)`.

**Step 1: Failing tests:**
- Figure 2/3 JSON scenario merges to `[1,6,4,5,3]` (assert via
  structural equality against a parse of the expected text).
- Both branches make the same insertion → appears once.
- Both branches delete the same node → gone, no conflict.
- A-only rename + B-only statement insert in different functions →
  both land.
- Result of every merged case passes `check(o, a, b, &m)`.

**Step 2–4:** fail / implement / pass. **Step 5:** Commit.

### Task 12: Conflict rules — relabel-relabel and insert-insert

**Files:** Modify `src/merge.rs`, create `src/rules.rs`.

Rule trait per design doc: each rule inspects an (edit-from-A,
edit-from-B) pair with context and may yield a `Conflict { span_o,
span_a, span_b, rule: &'static str }`.

- **relabel-relabel:** same O node, different target labels.
- **insert-insert:** insertions anchored at the same
  (parent-image, slot) whose subtree hashes differ.

**Step 1: Failing tests:** both branches rename one function
differently → conflict named `relabel-relabel`; both insert
different statements at the same position → `insert-insert`;
same-position *identical* inserts still merge (regression on
Task 11 dedupe).

**Step 2–4 / 5:** fail / implement / pass / commit.

### Task 13: Conflict rules — delete-delete and insert-delete

**Files:** Modify `src/rules.rs`.

- **delete-delete (split deletion):** connected deletion regions of
  A and B that overlap but do not coincide → conflict. Compute
  connected components of deleted O nodes per branch (parent-child
  edges within the deleted set); flag pairs of components that
  intersect without being equal.
- **insert-delete (broken dependency):** branch X inserts under
  node p (or a descendant of p), branch Y deletes p, and no
  surviving ancestor of p shares p's syntactic category (per
  `node-types.json` supertypes — approximate the paper's "same
  syntactic category" with: an ancestor whose kind admits the
  inserted node's kind among its children).

**Step 1: Failing tests:** the paper's `f(c)` example — A rewrites
`f(c)` to `x = c` (deletes the call wrapper), B deletes the whole
expression statement → `delete-delete` conflict. A inserts a
statement into a function body, B deletes the whole function →
`insert-delete` conflict. A inserts a method into an inner
class, B deletes only that class-but-not-the-outer → still a
conflict per the rule; A inserts into a function, B deletes a
*different* function → no conflict (negative case).

**Step 2–4 / 5:** fail / implement / pass / commit.

### Task 14: Arity/category rule

**Files:** Modify `src/rules.rs`, `src/lang.rs`.

After constructing candidate M (before declaring success), walk
every M node whose child list changed relative to O and validate
against `NodeTypes`:
- fixed slots (fields): each required field present exactly once,
  each field's kind in the field's allowed types,
- variable-arity children: each child's kind within the parent's
  allowed `children.types`; `required: true` children not emptied.

Violations become conflicts attributed to the contributing edits.
This is the grammar-driven stand-in for the paper's per-language
syntactic-consistency rules, and it also backstops rule gaps: any
structurally invalid combination gets caught here even if no
pairwise rule fired.

**Step 1: Failing test:** craft a merge where each branch's edit is
individually fine but the combination empties a required field
(e.g. A deletes an if-condition's only child while B relies on it —
if constructing this is hard in real grammars, drive the rule
directly with a hand-built candidate M). Negative case: legal
variable-arity growth (two statements inserted into a block) does
not fire.

**Step 2–4 / 5:** fail / implement / pass / commit.

---

## Milestone 5: synthesis and CLI

### Task 15: Span-based synthesis

**Files:** Create `src/synth.rs`.

Emit merged source text: walk M depth-first; maximal runs of
consecutive leaves sharing an origin file emit as one contiguous
source slice (byte-identical, preserving interior comments and
formatting); at origin switches, emit the inter-token whitespace
from the incoming node's origin file (the text between the previous
sibling's end and the node's start in that file), defaulting to a
single space/newline heuristic when the origin context is missing
(first child, etc.).

**Step 1: Failing tests:**
- Merge where only A edits: output preserves O/B-side comments and
  exact formatting of untouched regions (assert byte equality of
  the unedited region).
- Figure 2/3 JSON merge output parses and is structurally equal to
  `[1,6,4,5,3]`.
- A Rust merge: A adds a doc-commented function at the top, B adds
  one at the bottom; both survive with their comments (comments
  travel because whole-function spans are copied).

**Step 2–4 / 5:** fail / implement / pass / commit.

### Task 16: Self-check wiring

**Files:** Modify `src/merge.rs` (or a `src/lib.rs`-level
`merge_files` orchestrator).

`merge_to_text(o, a, b) -> MergeResult`: run merge → synthesize →
**re-parse the output** (must produce zero error nodes) → run
`check()` on the re-parsed M. Any failure converts the outcome to
`Conflicts` with a `self-check` pseudo-rule (and a
`tracing::error!` — it means a d3j bug, not a user problem).

**Step 1: Failing test:** unit-test the guard by feeding a mock
synthesis output that drops a node (make the synthesis step
injectable for tests, or `#[cfg(test)]` hook). All existing merge
tests re-run through `merge_to_text` and still pass.

**Step 2–4 / 5:** fail / implement / pass / commit.

### Task 17: CLI

**Files:** Modify `src/bin/d3j/main.rs`, create
`src/bin/d3j/commands/{merge,check}.rs`, `tests/cli.rs`.

Per design doc CLI section and rust skill `binary.md` (clap derive,
`--completions`, miette main — no tokio):

```
d3j merge <O> <A> <B> [-o out] [--lang X]    # 0 merged, 1 conflicts, 2 error
d3j check <O> <A> <B> <M> [--lang X]         # 0 correct, 1 violations, 2 error
```

Conflict rendering: diff3-style markers (`<<<<<<<`/`|||||||`/
`=======`/`>>>>>>>`) around the three origin spans of each
conflict, embedded in otherwise-merged output where feasible; v1
may fall back to whole-file markers when conflicts overlap.
`check` prints one line per violation (condition, span, source
excerpt) — human-readable, no JSON output in v1.

**Step 1: Failing integration tests** (`tests/cli.rs`, assert_cmd):
- clean merge exits 0 and writes expected output (Figure 2/3 JSON
  scenario as fixture files in `tests/corpus/`),
- conflicting merge exits 1, output contains `<<<<<<<`,
- unparsable input exits 2 with the `d3j::parse` diagnostic,
- unknown extension exits 2,
- `check` on a known-bad M exits 1 naming the violated condition.

**Step 2–4 / 5:** fail / implement / pass / commit.

### Task 18: Scenario corpus harness

**Files:** Create `tests/corpus.rs`, `tests/corpus/scenarios/*/`.

Directory-per-scenario: `O.<ext>`, `A.<ext>`, `B.<ext>`, plus
either `expected.<ext>` (must merge cleanly and match structurally)
or `expected.CONFLICT` (must conflict, file names the rule). The
test enumerates directories at runtime — adding a scenario is just
adding files. Seed with: Figures 2/3 and 10, one scenario per
conflict rule, and the Task 15 comment-preservation cases, in both
JSON and Rust where expressible.

**Steps:** enumerate-fail / seed / pass / commit.

---

## Done criteria

`just all` green (fmt, clippy, 100% library coverage per the
skill's gate — adjust with documented reasoning if unreachable),
`just mutants` clean, and:

```sh
d3j merge tests/corpus/scenarios/recipe/O.json \
          tests/corpus/scenarios/recipe/A.json \
          tests/corpus/scenarios/recipe/B.json
```

prints `[1, 6, 4, 5, 3]`-shaped JSON with exit 0. Then wire it into
jj (`merge-tools.d3j`) and dogfood — that, move detection, Java
conflict-rule depth, and the paper's replication datasets are the
next plan.

## Deferred (do not build in this plan)

Move detection, comment *merging* (beyond span survival), textual
fallback, JSON output modes, `build.rs` versioning + release
workflow, DotSlash. YAGNI until the core proves out.
