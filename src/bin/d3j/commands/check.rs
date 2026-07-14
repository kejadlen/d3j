use std::ops::Range;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use d3j::Tree;
use d3j::check::{Branch, Violation, check};

use super::{load, resolve_lang};

#[derive(Args)]
pub struct CheckArgs {
    /// The common origin file.
    origin: PathBuf,
    /// One branch.
    a: PathBuf,
    /// The other branch.
    b: PathBuf,
    /// The merged file to judge.
    merged: PathBuf,
    /// Override language detection (rust, java, json).
    #[arg(long)]
    lang: Option<String>,
}

pub fn run(args: CheckArgs) -> miette::Result<ExitCode> {
    let lang = resolve_lang(&args.origin, args.lang.as_deref())?;
    let (_, o) = load(&args.origin, lang)?;
    let (_, a) = load(&args.a, lang)?;
    let (_, b) = load(&args.b, lang)?;
    let (_, m) = load(&args.merged, lang)?;

    let report = check(&o, &a, &b, &m);
    for violation in &report.violations {
        // One line per violation: condition, span, source excerpt.
        let (tree, span) = witness(&o, &a, &b, &m, violation);
        let excerpt = tree.source_slice(span.clone()).unwrap_or("");
        println!("{violation}: {excerpt:?}");
    }

    if report.is_correct() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

/// The tree a violation's witness span points into.
fn witness<'t>(
    o: &'t Tree,
    a: &'t Tree,
    b: &'t Tree,
    m: &'t Tree,
    violation: &Violation,
) -> (&'t Tree, Range<usize>) {
    match violation {
        Violation::ExtraInsertion { span, .. } => (m, span.clone()),
        Violation::MissedInsertion {
            branch: Branch::A,
            span,
            ..
        } => (a, span.clone()),
        Violation::MissedInsertion {
            branch: Branch::B,
            span,
            ..
        } => (b, span.clone()),
        Violation::ExtraDeletion { span, .. } | Violation::MissedDeletion { span, .. } => {
            (o, span.clone())
        }
    }
}
