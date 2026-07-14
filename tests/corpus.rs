//! The scenario corpus: every directory under tests/corpus/scenarios/
//! is one merge case. Adding a scenario is adding files, no code.
//!
//! A scenario holds O.<ext>, A.<ext>, B.<ext>, and either
//! expected.<ext> (must merge cleanly and match structurally) or
//! expected.CONFLICT (must conflict; the file names the rule).

use std::path::{Path, PathBuf};

use d3j::diff::Shape;
use d3j::merge::{MergeResult, merge_to_text};
use d3j::{Lang, Tree};

#[test]
fn scenarios() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/scenarios");
    let mut ran = 0usize;
    let entries = fs_err::read_dir(&root).expect("scenario corpus directory exists");
    for entry in entries {
        let dir = entry.expect("readable scenario entry").path();
        if !dir.is_dir() {
            continue;
        }
        run_scenario(&dir);
        ran = ran.saturating_add(1);
    }
    assert!(ran > 0, "the scenario corpus must not be empty");
}

fn run_scenario(dir: &Path) {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let o_path = input(dir, "O").unwrap_or_else(|| panic!("{name}: missing O.<ext>"));
    let lang = Lang::detect(&o_path).unwrap_or_else(|| panic!("{name}: unknown language"));
    let parse = |stem: &str| -> Tree {
        let path = input(dir, stem).unwrap_or_else(|| panic!("{name}: missing {stem}.<ext>"));
        let source =
            fs_err::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {stem}: {e}"));
        Tree::parse(&source, lang).unwrap_or_else(|e| panic!("{name}: {stem} does not parse: {e}"))
    };

    let o = parse("O");
    let a = parse("A");
    let b = parse("B");
    let outcome = merge_to_text(&o, &a, &b).unwrap_or_else(|e| panic!("{name}: merge failed: {e}"));

    let conflict_marker = dir.join("expected.CONFLICT");
    if conflict_marker.exists() {
        let rule = fs_err::read_to_string(&conflict_marker)
            .unwrap_or_else(|e| panic!("{name}: expected.CONFLICT: {e}"));
        let rule = rule.trim();
        match outcome {
            MergeResult::Conflicts(conflicts) => {
                assert!(
                    conflicts.iter().any(|c| c.rule == rule),
                    "{name}: expected a {rule} conflict, got {conflicts:?}"
                );
            }
            MergeResult::Merged(text) => {
                panic!("{name}: expected a {rule} conflict, merged to {text:?}");
            }
        }
    } else {
        let expected = parse("expected");
        match outcome {
            MergeResult::Merged(text) => {
                let merged = Tree::parse(&text, lang)
                    .unwrap_or_else(|e| panic!("{name}: output does not parse: {e}\n{text}"));
                assert_eq!(
                    Shape::of(&merged),
                    Shape::of(&expected),
                    "{name}: merged output {text:?} does not match expected"
                );
            }
            MergeResult::Conflicts(conflicts) => {
                panic!("{name}: expected a clean merge, got {conflicts:?}");
            }
        }
    }
}

/// The scenario file with the given stem (any extension except the
/// CONFLICT marker).
fn input(dir: &Path, stem: &str) -> Option<PathBuf> {
    fs_err::read_dir(dir).ok()?.find_map(|entry| {
        let path = entry.ok()?.path();
        let matches = path.file_stem().is_some_and(|s| s == stem)
            && path.extension().is_some_and(|e| e != "CONFLICT");
        matches.then_some(path)
    })
}
