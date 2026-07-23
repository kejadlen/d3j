# Comparison site: d3j vs. mergiraf

A generated static site for reviewing how d3j's merges differ from
[mergiraf](https://mergiraf.org)'s. Mergiraf is a mature tree-sitter
structural merge tool in Rust, so it is the natural baseline: same
problem, same parsing substrate, a different algorithm. The site runs
both tools over a corpus of merge scenarios and renders the results side
by side.

## Why

d3j is built against a paper; mergiraf is a shipping tool that made its
own concrete choices for the same problem. Watching where the two
diverge on real inputs is a development aid — a differential-testing
dashboard that grows as d3j is implemented.

The two tools take different routes:

| Aspect | d3j | mergiraf |
|---|---|---|
| Matching | Anchored Zhang–Shasha, base→each branch | GumTree classic across all three pairs |
| Core object | Partial inclusion map, merged as a pushout | PCS triples, 3DM/Spork-style changeset union |
| On failure | Reports a conflict; never falls back to text | Falls back to line-based diff3 |
| Correctness | Universality checker as oracle + self-check | Pragmatic; no formal guarantee |

The flagship case is the paper's own sequence example (Figures 2 and 3):
base `[1, 2, 3]`, left `[1, 2, 4, 5, 3]`, right `[1, 6, 3]`. As a
multi-line JSON array, mergiraf conflicts; the paper argues the correct
structural merge is `[1, 6, 4, 5, 3]`, which d3j aims to produce.

## Scope

The site is a review aid, not a benchmark. Non-scope: timing, scoring, a
large corpus, or any automated pass/fail gate. It renders outputs and
lets a human judge.

## Corpus

`compare/scenarios/<name>/` holds `base.<ext>`, `left.<ext>`,
`right.<ext>`, and a `notes.md` describing the case and expectation.
Extensions drive language detection for both tools. Seeded across the
three languages d3j supports:

- `json-sequence` — the paper's Figures 2/3 example; mergiraf conflicts.
- `json-independent-keys` — two branches add different keys; clean.
- `java-insert-insert` — based on Figure 12: an insert-insert collision.
- `java-switch-to-if` — based on Figure 13: a switch→if refactor.
- `rust-independent-methods` — two branches add different methods; clean.
- `rust-relabel-relabel` — both rename one identifier differently.

Figures 12 and 13 are images in the paper, so those scenarios reconstruct
the described behavior rather than transcribe code.

## Generator

`compare/generate.sh` (POSIX-friendly bash) takes an output directory and
for each scenario:

1. Runs `mergiraf merge base left right` and captures stdout and exit
   code (0 = clean, 1 = conflict).
2. Runs `d3j merge base left right` the same way. d3j has no `merge`
   subcommand yet, so empty output is treated as "pending" until the CLI
   lands.
3. Renders a per-scenario page — the three inputs, both merged outputs,
   status badges, and a unified diff between the two outputs — plus an
   index with a status matrix and the design summary above.

Output is a self-contained directory (HTML plus one stylesheet); nothing
is committed.

## Publishing

A GitHub Actions workflow (`.github/workflows/compare.yml`) builds d3j,
installs a pinned mergiraf, runs the generator, and deploys to GitHub
Pages. It triggers on push to `main` and on manual dispatch. Enabling
Pages with the "GitHub Actions" source is a one-time repository setting.

## Testing

The generator is exercised by running it locally against the corpus with
the installed mergiraf and a debug d3j build. There is no automated
assertion layer; the site itself is the output under review.
