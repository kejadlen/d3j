pub mod check;
pub mod merge;

use std::path::Path;

use d3j::{Error, Lang, Tree};

/// Resolves the language: an explicit `--lang` wins, otherwise the
/// file extension decides.
pub fn resolve_lang(path: &Path, lang: Option<&str>) -> Result<&'static Lang, Error> {
    match lang {
        Some(name) => Lang::by_name(name).ok_or(Error::UnknownLanguage { path: name.into() }),
        None => Lang::detect(path).ok_or(Error::UnknownLanguage { path: path.into() }),
    }
}

/// Reads and parses one input file, wiring the path into any error.
pub fn load(path: &Path, lang: &'static Lang) -> Result<(String, Tree), Error> {
    let source = fs_err::read_to_string(path).map_err(|source| Error::Io {
        path: path.into(),
        source,
    })?;
    let tree = Tree::parse(&source, lang).map_err(|error| match error {
        Error::Parse { lang, .. } => Error::Parse {
            path: path.into(),
            lang,
        },
        other => other,
    })?;
    Ok((source, tree))
}
