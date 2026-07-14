use std::hash::{DefaultHasher, Hash, Hasher};

use crate::tree::Tree;

/// Computes a Merkle hash for every node: (kind_id, label, child hashes
/// in order), indexed like the tree's arena.
///
/// Spans are deliberately excluded so the hash is position-independent —
/// identical code at different places (or in different trees) collides,
/// which is exactly what anchoring the diff on unique equal subtrees
/// needs.
pub(crate) fn compute(tree: &Tree) -> Vec<u64> {
    let mut hashes = vec![0u64; tree.nodes().count()];
    // Arena order is pre-order, so every child's index is greater than
    // its parent's; walking indices in reverse hashes children first.
    let ids: Vec<_> = tree.nodes().collect();
    for &id in ids.iter().rev() {
        let mut hasher = DefaultHasher::new();
        tree.kind_id(id).hash(&mut hasher);
        tree.label(id).hash(&mut hasher);
        for &child in tree.children(id) {
            // In-bounds by construction: children share the arena the
            // hashes vec was sized from, and the reverse walk computed
            // their hashes already.
            #[allow(clippy::indexing_slicing)]
            hashes[child.index()].hash(&mut hasher);
        }
        #[allow(clippy::indexing_slicing)]
        {
            hashes[id.index()] = hasher.finish();
        }
    }
    hashes
}

#[cfg(test)]
mod tests {
    use crate::error::Error;
    use crate::lang::Lang;
    use crate::tree::Tree;

    fn parse_rust(src: &str) -> Result<Tree, Error> {
        let lang = Lang::by_name("rust").ok_or(Error::UnknownLanguage {
            path: "rust".into(),
        })?;
        Tree::parse(src, lang)
    }

    #[test]
    fn identical_subtrees_hash_equal_across_positions() -> Result<(), Error> {
        let t = parse_rust("fn a() { x(); }\nfn b() { x(); }")?;
        let calls: Vec<_> = t
            .nodes()
            .filter(|&n| t.kind(n) == "call_expression")
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls.first().map(|&n| t.hash(n)),
            calls.last().map(|&n| t.hash(n))
        );
        Ok(())
    }

    #[test]
    fn equal_sources_hash_equal_across_trees() -> Result<(), Error> {
        let a = parse_rust("fn main() { x(); }")?;
        let b = parse_rust("fn main() { x(); }")?;
        assert_eq!(a.hash(a.root()), b.hash(b.root()));
        Ok(())
    }

    #[test]
    fn different_labels_hash_differently() -> Result<(), Error> {
        let a = parse_rust("fn main() { x(); }")?;
        let b = parse_rust("fn main() { y(); }")?;
        assert_ne!(a.hash(a.root()), b.hash(b.root()));
        Ok(())
    }

    #[test]
    fn different_structure_hashes_differently() -> Result<(), Error> {
        let a = parse_rust("fn main() { x(); }")?;
        let b = parse_rust("fn main() { x(); x(); }")?;
        assert_ne!(a.hash(a.root()), b.hash(b.root()));
        Ok(())
    }
}
