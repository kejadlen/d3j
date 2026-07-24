# d3j

Structural three-way merge for source code, built on tree-sitter.

Textual merge tools like `diff3` work on lines. They report a conflict
whenever two branches edit nearby lines, even when the edits are
independent — reordering two methods, or adding an import next to one
someone else added. d3j merges the *syntax tree* instead of the text, so
edits that don't structurally overlap merge cleanly, and the conflicts
that remain are real.

d3j also refuses to emit a merge it cannot prove correct. Every merge it
produces is checked against a formal correctness criterion before it
reaches your working tree; if the check fails, d3j reports a conflict
rather than hand you a wrong answer.

This is a language-generic Rust implementation of the tool and
correctness criteria from Mori & Hashimoto, ["On the Correctness of
Software Merge"](https://arxiv.org/abs/2607.07987) (ASE 2025). The
paper's d3j targets Java through a custom OCaml parser; this port drives
any tree-sitter grammar, and ships with Rust, Java, and JSON.

## Status

Early development, but the pipeline works end to end: parsing, the
arena AST, base-to-branch diffing, the pushout merge, the correctness
checker, output synthesis, and the `d3j` CLI are all in place for the
bundled grammars (Rust, Java, JSON). Move detection, deeper Java
conflict rules, and evaluation against the paper's replication
datasets are still to come. See
[`docs/plans/2026-07-10-d3j-design.md`](docs/plans/2026-07-10-d3j-design.md)
for the full design and milestones.

## How it works

d3j parses the base and both branches, lifts each tree-sitter syntax
tree into an arena AST, and diffs the base against each branch to build a
partial inclusion map — which nodes survived, which were inserted,
deleted, or relabeled. It merges the two maps as a pushout: a node
survives the merge when both branches keep it, independent edits apply
once, and edits that touch the same syntactic slot in incompatible ways
raise a conflict.

Output is synthesized from the original source spans, so untouched
regions come out byte-identical to their input — formatting and comments
survive. Before emitting, d3j re-parses its own output and runs the
correctness checker on it. That checker is also the test oracle: every
merge d3j emits must pass the same universality conditions the paper
defines.

## Building

d3j is a standard Cargo project; it needs a Rust toolchain on the 2024
edition.

```sh
cargo build
cargo test
```

To install the `d3j` binary:

```sh
just install    # cargo install --locked --path .
```

## Usage

The binary takes diff3-style argument order, so it drops into a
merge-driver configuration:

```sh
d3j merge <base> <ours> <theirs> [-o out] [--lang rust]
d3j check <base> <ours> <theirs> <merged>
```

`merge` exits 0 on a clean merge and 1 when it emits conflict markers.
`check` reports which correctness conditions a merge result violates.
Both exit 2 on unparsable input or an unknown language — d3j never
silently falls back to a textual merge. Language is detected from the
file extension, with `--lang` as an override.

## Using with jj

d3j plugs into [Jujutsu](https://jj-vcs.dev) as a merge tool. With the
binary installed, add this to your jj config (`jj config edit --user`):

```toml
[merge-tools.d3j]
program = "d3j"
merge-args = ["merge", "$base", "$left", "$right", "-o", "$output"]
merge-conflict-exit-codes = [1]
conflict-marker-style = "git"
```

Then resolve conflicts with it:

```sh
jj resolve --tool d3j                   # all conflicted files
jj resolve --tool d3j 'glob:**/*.rs'    # scope to one language
```

jj hands d3j temporary copies of the three sides (the files keep their
extension, so language detection works) and reads the merged result
back from `$output`. `merge-conflict-exit-codes` (jj ≥ 0.24) tells jj
that exit 1 means the output still contains conflict markers, which jj
parses back into its own conflict state; `conflict-marker-style =
"git"` matches the diff3-style markers d3j emits. Exit 2 is
deliberately not listed: an unparsable file or unknown language is a
tool failure, and jj leaves the conflict untouched.

Because only Rust, Java, and JSON grammars are bundled, scoping
`jj resolve` with a fileset is the smoothest workflow — files in other
languages fail with exit 2. Keep `ui.merge-editor` pointed at an
interactive editor and reach for `--tool d3j` explicitly.

## Development

The `justfile` wraps the common tasks:

```sh
just            # fmt, clippy, and coverage
just clippy     # cargo clippy --workspace -- -D warnings
just coverage   # test coverage report
just mutants    # mutation testing
```

CI runs formatting, clippy, coverage, and mutation testing on every push
and pull request.

## Releases

Pushing a `v*` tag triggers the release workflow, which builds macOS
(aarch64) and Linux (aarch64) binaries, creates a GitHub release with
generated notes, and publishes a [DotSlash](https://dotslash-cli.com)
file for the release.

## Reference

- Paper: Mori & Hashimoto, "On the Correctness of Software Merge,"
  arXiv:2607.07987 — <https://arxiv.org/abs/2607.07987>
- Replication package —
  <https://doi.org/10.5281/zenodo.13335352>
