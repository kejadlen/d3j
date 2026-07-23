# Comparison site

A generated static site for reviewing how d3j's merges differ from
[mergiraf](https://mergiraf.org)'s. `generate.sh` runs both tools over
every scenario in `scenarios/` and renders the results side by side. The
design and rationale live in
[`docs/plans/2026-07-23-comparison-site-design.md`](../docs/plans/2026-07-23-comparison-site-design.md).

## Running locally

Prerequisites: `mergiraf` on your `PATH` (or pointed at via `MERGIRAF`),
and a d3j build.

```sh
cargo build
D3J=target/debug/d3j ./compare/generate.sh dist
```

Open `dist/index.html`. Both tools default to `PATH`; override the
`MERGIRAF` and `D3J` environment variables to point at specific binaries.
Set `TRACE=1` to trace the generator.

d3j has no working merge yet, so its column reads "pending" until the CLI
lands — the harness tolerates that and fills in as d3j grows.

## Adding a scenario

Create `scenarios/<name>/` with `base.<ext>`, `left.<ext>`, and
`right.<ext>` in one of the languages d3j supports (`.rs`, `.java`,
`.json`); the extension drives language detection. Add a `notes.md`
describing the case and the expected outcome. The next run picks it up.

## Publishing

`.github/workflows/compare.yml` regenerates the site and deploys it to
GitHub Pages on pushes to `main` that touch the corpus, the generator, or
d3j's sources. Publishing requires enabling Pages for the repository with
the source set to "GitHub Actions" — a one-time setting.
