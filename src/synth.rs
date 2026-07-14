//! Span-based synthesis: turn a merged tree back into source text by
//! emitting original bytes.
//!
//! The merged tree's leaves are visited depth-first. A maximal run of
//! consecutive leaves that are *adjacent* in one origin file emits as a
//! single contiguous slice of that file — byte-identical, so interior
//! comments and formatting survive. Adjacency (consecutive positions in
//! the origin's leaf sequence) is what separates preserved trivia from
//! deleted code: a gap in the sequence means something between the two
//! leaves was deleted, and slicing across it would resurrect it.
//!
//! Where the origin switches, the inter-token trivia comes from the
//! incoming leaf's own file — the text between its preceding leaf there
//! and itself — which is how a grafted function's doc comment travels.
//! An incoming leaf with no predecessor (its file's first leaf) falls
//! back to a single space unless the output is empty, in which case its
//! leading trivia (a file header comment) is emitted instead.

use crate::merge::{MergedTree, Origin};
use crate::tree::{NodeId, Tree};

/// Renders the merged tree as source text.
pub fn synthesize(merged: &MergedTree, o: &Tree, a: &Tree, b: &Tree) -> String {
    let trees = [o, a, b];
    let sequences: [LeafSequence; 3] = [
        LeafSequence::of(o),
        LeafSequence::of(a),
        LeafSequence::of(b),
    ];

    // The merged leaves, as (tree index, origin node), in order.
    let mut leaves: Vec<(usize, NodeId)> = Vec::new();
    let mut stack = vec![merged.root()];
    while let Some(id) = stack.pop() {
        let (tree_index, node) = match merged.origin(id) {
            Origin::O(n) => (0, n),
            Origin::A(n) => (1, n),
            Origin::B(n) => (2, n),
        };
        let children = merged.children(id);
        if children.is_empty() {
            // Only origin-leaves emit: a merged node emptied of the
            // children its origin had must not emit the origin span,
            // which still covers them.
            if trees
                .get(tree_index)
                .is_some_and(|tree| tree.children(node).is_empty())
            {
                leaves.push((tree_index, node));
            }
        } else {
            stack.extend(children.iter().rev().copied());
        }
    }

    let mut out = String::new();
    let mut run: Option<Run> = None;
    for (tree_index, node) in leaves {
        // Tree indices are 0..3 by construction of the leaf list.
        #[allow(clippy::indexing_slicing)]
        let (tree, sequence) = (trees[tree_index], &sequences[tree_index]);
        let Some(position) = sequence.position(node) else {
            continue; // cov-excl-line: merged leaves are origin leaves.
        };
        let span = tree.span(node);

        if let Some(current) = &mut run
            && current.tree_index == tree_index
            && position == current.last_position.saturating_add(1)
        {
            current.end = span.end;
            current.last_position = position;
            continue;
        }

        // Origin switch (or non-adjacent same-origin leaf): flush the
        // run, then bring the incoming leaf's leading trivia along.
        flush(&mut out, run.take(), &trees);
        match preceding_trivia(tree, sequence, position, span.start) {
            Some(trivia) => out.push_str(trivia),
            None if out.is_empty() => {
                out.push_str(tree.source_slice(0..span.start).unwrap_or(""));
            }
            None => {
                if !out.ends_with(char::is_whitespace) {
                    out.push(' ');
                }
            }
        }
        run = Some(Run {
            tree_index,
            start: span.start,
            end: span.end,
            last_position: position,
        });
    }

    // Flush and close the file: trailing trivia only when the last
    // leaf is also its origin's last leaf — anything after an interior
    // leaf might be deleted code.
    let last = run
        .as_ref()
        .map(|current| (current.tree_index, current.last_position));
    flush(&mut out, run.take(), &trees);
    let closed = last.is_some_and(|(tree_index, position)| {
        #[allow(clippy::indexing_slicing)] // Tree indices are 0..3.
        let (tree, sequence) = (trees[tree_index], &sequences[tree_index]);
        if position.saturating_add(1) != sequence.len() {
            return false; // cov-excl-line: in practice the final merged leaf is its origin's closing token, which is that file's last leaf.
        }
        let Some(last_node) = sequence.node_at(position) else {
            return false; // cov-excl-line: the position was just bounds-checked.
        };
        let tail = tree.source_slice(tree.span(last_node).end..tree.source_len());
        out.push_str(tail.unwrap_or(""));
        true
    });
    if !closed && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n'); // cov-excl-line: fallback for the unclosed case above.
    }

    out
}

/// One in-progress contiguous slice of a single origin file.
struct Run {
    tree_index: usize,
    start: usize,
    end: usize,
    last_position: usize,
}

fn flush(out: &mut String, run: Option<Run>, trees: &[&Tree; 3]) {
    if let Some(run) = run
        && let Some(&tree) = trees.get(run.tree_index)
    {
        out.push_str(tree.source_slice(run.start..run.end).unwrap_or(""));
    }
}

/// The trivia between a leaf and its predecessor in its own file, or
/// `None` for a file's first leaf.
fn preceding_trivia<'t>(
    tree: &'t Tree,
    sequence: &LeafSequence,
    position: usize,
    start: usize,
) -> Option<&'t str> {
    let previous = sequence.node_at(position.checked_sub(1)?)?;
    tree.source_slice(tree.span(previous).end..start)
}

/// A tree's leaves in document order, with a node → position lookup.
struct LeafSequence {
    order: Vec<NodeId>,
    position: Vec<Option<usize>>,
}

impl LeafSequence {
    fn of(tree: &Tree) -> Self {
        let mut order = Vec::new();
        let mut position = vec![None; tree.nodes().count()];
        for node in tree.nodes() {
            if tree.children(node).is_empty() {
                if let Some(slot) = position.get_mut(node.index()) {
                    *slot = Some(order.len());
                }
                order.push(node);
            }
        }
        Self { order, position }
    }

    fn position(&self, node: NodeId) -> Option<usize> {
        self.position.get(node.index()).copied().flatten()
    }

    fn node_at(&self, position: usize) -> Option<NodeId> {
        self.order.get(position).copied()
    }

    fn len(&self) -> usize {
        self.order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::Shape;
    use crate::error::Error;
    use crate::lang::Lang;
    use crate::merge::{MergeOutcome, merge};

    fn parse(src: &str, lang_name: &str) -> Result<Tree, Error> {
        let lang = Lang::by_name(lang_name).ok_or(Error::UnknownLanguage {
            path: lang_name.into(),
        })?;
        Tree::parse(src, lang)
    }

    fn merge_text(lang: &str, o: &str, a: &str, b: &str) -> Result<String, Error> {
        let o = parse(o, lang)?;
        let a = parse(a, lang)?;
        let b = parse(b, lang)?;
        match merge(&o, &a, &b)? {
            MergeOutcome::Merged(m) => Ok(synthesize(&m, &o, &a, &b)),
            MergeOutcome::Conflicts(conflicts) => {
                panic!("expected a clean merge, got conflicts: {conflicts:?}");
            }
        }
    }

    #[test]
    fn one_sided_edits_reproduce_the_editing_branch_byte_for_byte() -> Result<(), Error> {
        let o = "// header\nfn a() {\n    x(); // call\n}\n\nfn b() {\n    y();\n}\n";
        let a = "// header\nfn a() {\n    x(); // call\n}\n\nfn b() {\n    y();\n    z();\n}\n";
        let text = merge_text("rust", o, a, o)?;
        assert_eq!(text, a);
        Ok(())
    }

    #[test]
    fn figure_2_3_output_parses_to_the_expected_shape() -> Result<(), Error> {
        let text = merge_text("json", "[1, 2, 3]", "[1, 2, 4, 5, 3]", "[1, 6, 3]")?;
        let merged = parse(&text, "json")?;
        let expected = parse("[1, 6, 4, 5, 3]", "json")?;
        assert_eq!(Shape::of(&merged), Shape::of(&expected));
        Ok(())
    }

    #[test]
    fn deleted_regions_do_not_resurface() -> Result<(), Error> {
        let text = merge_text("json", "[1, 2, 3]", "[1, 3]", "[1, 3]")?;
        assert!(!text.contains('2'), "{text:?}");
        let merged = parse(&text, "json")?;
        let expected = parse("[1, 3]", "json")?;
        assert_eq!(Shape::of(&merged), Shape::of(&expected));
        Ok(())
    }

    #[test]
    fn identical_inputs_reproduce_the_file_byte_for_byte() -> Result<(), Error> {
        // Trailing trivia after the last leaf travels...
        let with_tail = "fn a() {} // tail\n";
        assert_eq!(
            merge_text("rust", with_tail, with_tail, with_tail)?,
            with_tail
        );
        // ...and a file without a final newline does not grow one.
        let bare = "[1, 2]";
        assert_eq!(merge_text("json", bare, bare, bare)?, bare);
        Ok(())
    }

    #[test]
    fn grafted_functions_pin_exact_bytes() -> Result<(), Error> {
        // Byte-level regression for the trivia heuristics — including
        // the known single-space wart where the O run's first leaf has
        // no predecessor to borrow trivia from ("{} fn keep").
        let o = "fn keep() {\n    x();\n}\n";
        let a = "/// From A.\nfn top() {}\n\nfn keep() {\n    x();\n}\n";
        let b = "fn keep() {\n    x();\n}\n\n/// From B.\nfn bottom() {}\n";
        let text = merge_text("rust", o, a, b)?;
        assert_eq!(
            text,
            "/// From A.\nfn top() {} fn keep() {\n    x();\n}\n\n/// From B.\nfn bottom() {}\n"
        );
        Ok(())
    }

    #[test]
    fn a_reformat_only_branch_keeps_its_layout_around_the_other_branchs_edit() -> Result<(), Error>
    {
        // A pretty-prints without touching the tree; B bumps a value.
        // The reformat preference emits A's layout with B's edit
        // spliced in, instead of B's single-line bytes.
        let o = r#"{"name": "d3j", "version": 1, "debug": false}"#;
        let a = "{\n  \"name\": \"d3j\",\n  \"version\": 1,\n  \"debug\": false\n}\n";
        let b = r#"{"name": "d3j", "version": 2, "debug": false}"#;
        let text = merge_text("json", o, a, b)?;
        assert_eq!(
            text,
            "{\n  \"name\": \"d3j\",\n  \"version\": 2,\n  \"debug\": false\n}\n"
        );
        Ok(())
    }

    #[test]
    fn doc_comments_travel_with_grafted_functions() -> Result<(), Error> {
        let o = "fn keep() {\n    x();\n}\n";
        let a = "/// From A.\nfn top() {}\n\nfn keep() {\n    x();\n}\n";
        let b = "fn keep() {\n    x();\n}\n\n/// From B.\nfn bottom() {}\n";
        let text = merge_text("rust", o, a, b)?;
        assert!(text.contains("/// From A."), "{text:?}");
        assert!(text.contains("/// From B."), "{text:?}");
        let merged = parse(&text, "rust")?;
        let names: Vec<_> = merged
            .nodes()
            .filter(|&n| merged.kind(n) == "identifier")
            .filter_map(|n| merged.label(n))
            .collect();
        assert_eq!(names, vec!["top", "keep", "x", "bottom"]);
        Ok(())
    }
}
