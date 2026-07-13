use std::path::PathBuf;

use miette::Diagnostic;

/// The library-wide error space for d3j.
///
/// Unparsable input and an unknown language are distinguished from merge
/// conflicts at this level so the CLI can map them to a different exit
/// code than a clean merge with unresolved conflicts.
#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum Error {
    #[error("cannot detect language for {}", path.display())]
    #[diagnostic(
        code(d3j::unknown_language),
        help("pass --lang; supported: rust, java, json")
    )]
    UnknownLanguage { path: PathBuf },

    #[error("{} does not parse as {lang}", path.display())]
    #[diagnostic(
        code(d3j::parse),
        help("structural merge requires syntactically valid inputs")
    )]
    Parse { path: PathBuf, lang: String },

    #[error("io error on {}: {source}", path.display())]
    #[diagnostic(code(d3j::io))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(err: &Error) -> Option<String> {
        err.code().map(|c| c.to_string())
    }

    fn help_of(err: &Error) -> Option<String> {
        err.help().map(|h| h.to_string())
    }

    #[test]
    fn errors_render_with_diagnostic_codes() {
        let err = Error::UnknownLanguage {
            path: "x.zig".into(),
        };
        assert!(err.to_string().contains("x.zig"));
        assert_eq!(code_of(&err).as_deref(), Some("d3j::unknown_language"));
        assert_eq!(
            help_of(&err).as_deref(),
            Some("pass --lang; supported: rust, java, json")
        );
    }

    #[test]
    fn parse_error_renders_path_and_language() {
        let err = Error::Parse {
            path: "x.rs".into(),
            lang: "rust".into(),
        };
        assert!(err.to_string().contains("x.rs"));
        assert!(err.to_string().contains("rust"));
        assert_eq!(code_of(&err).as_deref(), Some("d3j::parse"));
        assert_eq!(
            help_of(&err).as_deref(),
            Some("structural merge requires syntactically valid inputs")
        );
    }

    #[test]
    fn io_error_renders_path_and_source() {
        let source = std::io::Error::other("permission denied");
        let err = Error::Io {
            path: "x.json".into(),
            source,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("x.json"));
        assert!(rendered.contains("permission denied"));
        assert_eq!(code_of(&err).as_deref(), Some("d3j::io"));
        assert!(std::error::Error::source(&err).is_some());
    }
}
