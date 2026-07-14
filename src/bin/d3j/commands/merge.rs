use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use miette::IntoDiagnostic as _;

use d3j::merge::{Conflict, MergeResult, merge_to_text};

use super::{load, resolve_lang};

#[derive(Args)]
pub struct MergeArgs {
    /// The common origin file.
    origin: PathBuf,
    /// One branch.
    a: PathBuf,
    /// The other branch.
    b: PathBuf,
    /// Write the result here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Override language detection (rust, java, json).
    #[arg(long)]
    lang: Option<String>,
}

pub fn run(args: MergeArgs) -> miette::Result<ExitCode> {
    let lang = resolve_lang(&args.origin, args.lang.as_deref())?;
    let (o_source, o) = load(&args.origin, lang)?;
    let (a_source, a) = load(&args.a, lang)?;
    let (b_source, b) = load(&args.b, lang)?;

    match merge_to_text(&o, &a, &b)? {
        MergeResult::Merged(text) => {
            emit(&args.output, &text)?;
            Ok(ExitCode::SUCCESS)
        }
        MergeResult::Conflicts(conflicts) => {
            report_conflicts(&conflicts);
            // v1 renders conflicts as whole-file diff3 markers; the
            // rules' byte spans go to stderr above.
            let rendered = diff3(&o_source, &a_source, &b_source);
            emit(&args.output, &rendered)?;
            Ok(ExitCode::from(1))
        }
    }
}

fn emit(output: &Option<PathBuf>, text: &str) -> miette::Result<()> {
    match output {
        Some(path) => fs_err::write(path, text).into_diagnostic(),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

fn report_conflicts(conflicts: &[Conflict]) {
    for conflict in conflicts {
        let spans: Vec<String> = [
            ("O", &conflict.span_o),
            ("A", &conflict.span_a),
            ("B", &conflict.span_b),
        ]
        .iter()
        .filter_map(|(name, span)| {
            span.as_ref()
                .map(|s| format!("{name} bytes {}..{}", s.start, s.end))
        })
        .collect();
        eprintln!("conflict: {} ({})", conflict.rule, spans.join(", "));
    }
}

/// Whole-file diff3-style markers.
fn diff3(o: &str, a: &str, b: &str) -> String {
    let terminated = |s: &str| {
        if s.is_empty() || s.ends_with('\n') {
            s.to_string()
        } else {
            format!("{s}\n")
        }
    };
    format!(
        "<<<<<<< A\n{}||||||| O\n{}=======\n{}>>>>>>> B\n",
        terminated(a),
        terminated(o),
        terminated(b),
    )
}
