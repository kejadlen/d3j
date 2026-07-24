//! The diff round-trip property: for generated (O, A) pairs, the
//! matching validates and the derived edit script rebuilds A's shape.

use std::path::Path;
use std::sync::LazyLock;

use hegel::TestCase;
use hegel::generators::integers;

use d3j::diff::{Shape, apply, diff, edits};
use d3j::{Lang, Tree};

/// The seed corpus: (language, source) pairs from tests/corpus/seeds/.
static SEEDS: LazyLock<Vec<(&'static Lang, String)>> = LazyLock::new(|| {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/seeds");
    let mut seeds: Vec<(&'static Lang, String)> = fs_err::read_dir(&dir)
        .expect("seed corpus directory exists")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let lang = Lang::detect(&path)?;
            let source = fs_err::read_to_string(&path).ok()?;
            Some((lang, source))
        })
        .collect();
    seeds.sort_by_key(|(lang, _)| lang.name());
    assert!(!seeds.is_empty(), "seed corpus must not be empty");
    seeds
});

/// Draws an index into 0..len (len must be nonzero).
fn draw_index(tc: &TestCase, len: usize) -> usize {
    let max = u32::try_from(len.saturating_sub(1)).unwrap_or(u32::MAX);
    tc.draw(integers::<u32>().min_value(0).max_value(max)) as usize
}

/// One small textual edit, or None when the draw picks an inapplicable
/// spot. Callers re-parse to filter out unparsable results.
fn mutate(tc: &TestCase, source: &str, lang: &'static Lang) -> Option<String> {
    match tc.draw(integers::<u8>().min_value(0).max_value(2)) {
        0 => {
            // Duplicate a line.
            let lines: Vec<&str> = source.lines().collect();
            let i = draw_index(tc, lines.len());
            let mut out = lines.clone();
            out.insert(i, lines.get(i)?);
            Some(out.join("\n"))
        }
        1 => {
            // Delete a line.
            let mut lines: Vec<&str> = source.lines().collect();
            if lines.len() < 2 {
                return None;
            }
            let i = draw_index(tc, lines.len());
            lines.remove(i);
            Some(lines.join("\n"))
        }
        _ => {
            // Rewrite one labeled leaf in place.
            let tree = Tree::parse(source, lang).ok()?;
            let leaves: Vec<_> = tree.nodes().filter(|&n| tree.label(n).is_some()).collect();
            if leaves.is_empty() {
                return None;
            }
            let node = *leaves.get(draw_index(tc, leaves.len()))?;
            let fresh = tc.draw(integers::<u16>().min_value(0).max_value(999));
            let replacement = match tree.kind(node) {
                "identifier" | "type_identifier" | "field_identifier" => format!("zz{fresh}"),
                "integer_literal" | "number" => format!("{fresh}"),
                "string_content" => format!("s{fresh}"),
                "true" | "false" => "true".to_string(),
                _ => return None,
            };
            let span = tree.span(node);
            let mut out = String::with_capacity(source.len());
            out.push_str(source.get(..span.start)?);
            out.push_str(&replacement);
            out.push_str(source.get(span.end..)?);
            Some(out)
        }
    }
}

/// Applies up to `budget` parsability-preserving edits to `source`.
fn evolve(tc: &TestCase, source: &str, lang: &'static Lang, budget: u8) -> String {
    let mut current = source.to_string();
    let count = tc.draw(integers::<u8>().min_value(0).max_value(budget));
    for _ in 0..count {
        if let Some(candidate) = mutate(tc, &current, lang)
            && Tree::parse(&candidate, lang).is_ok()
        {
            current = candidate;
        }
    }
    current
}

#[hegel::test(test_cases = 500)]
fn diff_round_trips_generated_edits(tc: TestCase) {
    let (lang, seed) = {
        let seeds = &*SEEDS;
        let (lang, seed) = &seeds[draw_index(&tc, seeds.len())];
        (*lang, seed.clone())
    };
    let o_source = evolve(&tc, &seed, lang, 2);
    let a_source = evolve(&tc, &o_source, lang, 4);
    tc.note(&format!("O:\n{o_source}\nA:\n{a_source}"));

    let o = Tree::parse(&o_source, lang).expect("O evolved through parsable edits");
    let a = Tree::parse(&a_source, lang).expect("A evolved through parsable edits");

    let m = diff(&o, &a);
    m.validate(&o, &a)
        .expect("diff produced an invalid matching");
    let script = edits(&o, &a, &m);
    assert_eq!(apply(&o, &a, &m, &script), Shape::of(&a));
}
