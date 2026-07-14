//! End-to-end tests against the compiled d3j binary.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn cmd() -> Command {
    Command::cargo_bin("d3j").expect("d3j binary builds")
}

/// Writes the given files into a fresh temp dir; returns their paths.
fn fixtures(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = files
        .iter()
        .map(|(name, content)| {
            let path = dir.path().join(name);
            fs_err::write(&path, content).expect("write fixture");
            path
        })
        .collect();
    (dir, paths)
}

#[test]
fn clean_merge_exits_zero_with_expected_output() {
    let (_dir, paths) = fixtures(&[
        ("O.json", "[1, 2, 3]\n"),
        ("A.json", "[1, 2, 4, 5, 3]\n"),
        ("B.json", "[1, 6, 3]\n"),
    ]);
    cmd()
        .arg("merge")
        .args(&paths)
        .assert()
        .success()
        .stdout(predicate::str::contains("6").and(predicate::str::contains("4")));
}

#[test]
fn merge_writes_to_the_output_file() {
    let (dir, paths) = fixtures(&[
        ("O.json", "[1, 2]\n"),
        ("A.json", "[1, 2, 3]\n"),
        ("B.json", "[1, 2]\n"),
    ]);
    let out = dir.path().join("M.json");
    cmd()
        .arg("merge")
        .args(&paths)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    let written = fs_err::read_to_string(&out).expect("output file written");
    assert!(written.contains('3'), "{written:?}");
}

#[test]
fn conflicting_merge_exits_one_with_markers() {
    let (_dir, paths) = fixtures(&[
        ("O.rs", "fn a() {}\n"),
        ("A.rs", "fn b() {}\n"),
        ("B.rs", "fn c() {}\n"),
    ]);
    cmd()
        .arg("merge")
        .args(&paths)
        .assert()
        .code(1)
        .stdout(predicate::eq(
            "<<<<<<< A\nfn b() {}\n||||||| O\nfn a() {}\n=======\nfn c() {}\n>>>>>>> B\n",
        ))
        .stderr(predicate::str::contains("relabel-relabel"));
}

#[test]
fn unparsable_input_exits_two_with_the_parse_diagnostic() {
    let (_dir, paths) = fixtures(&[
        ("O.rs", "fn main( {\n"),
        ("A.rs", "fn main() {}\n"),
        ("B.rs", "fn main() {}\n"),
    ]);
    cmd()
        .arg("merge")
        .args(&paths)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("d3j::parse"));
}

#[test]
fn unknown_extension_exits_two() {
    let (_dir, paths) = fixtures(&[("O.zig", "x\n"), ("A.zig", "x\n"), ("B.zig", "x\n")]);
    cmd()
        .arg("merge")
        .args(&paths)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("d3j::unknown_language"));
}

#[test]
fn lang_flag_overrides_detection() {
    let (_dir, paths) = fixtures(&[
        ("O.zig", "[1]\n"),
        ("A.zig", "[1, 2]\n"),
        ("B.zig", "[1]\n"),
    ]);
    cmd()
        .arg("merge")
        .args(&paths)
        .args(["--lang", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2"));
}

#[test]
fn check_flags_a_bad_merge() {
    // Figure 10: A inserts 3, B deletes 1; M dropped the insertion.
    let (_dir, paths) = fixtures(&[
        ("O.json", "[1, 2]\n"),
        ("A.json", "[1, 2, 3]\n"),
        ("B.json", "[2]\n"),
        ("M.json", "[2]\n"),
    ]);
    cmd()
        .arg("check")
        .args(&paths)
        .assert()
        .code(1)
        .stdout(predicate::str::contains("missed insertion"));
}

#[test]
fn check_passes_a_correct_merge() {
    let (_dir, paths) = fixtures(&[
        ("O.json", "[1, 2]\n"),
        ("A.json", "[1, 2, 3]\n"),
        ("B.json", "[2]\n"),
        ("M.json", "[2, 3]\n"),
    ]);
    cmd().arg("check").args(&paths).assert().success();
}

#[test]
fn no_arguments_prints_help() {
    cmd()
        .assert()
        .success()
        .stdout(predicate::str::contains("merge"));
}

#[test]
fn completions_generate() {
    cmd().args(["--completions", "zsh"]).assert().success();
}
