use std::num::NonZeroU16;
use std::ops::Range;
use std::path::PathBuf;

use crate::error::Error;
use crate::lang::Lang;

/// An index into a [`Tree`]'s node arena.
///
/// Ids are only meaningful for the tree that produced them; mixing ids
/// across trees is a logic error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(usize);

impl NodeId {
    /// The arena index, for dense per-node side tables (subtree hashes,
    /// matching maps).
    pub(crate) fn index(self) -> usize {
        self.0
    }

    /// Rebuilds an id from a side-table index. The caller owes the
    /// same care as with indexing: the index must have come from a
    /// table sized for the tree the id will be used with.
    pub(crate) fn from_index(index: usize) -> Self {
        Self(index)
    }
}

/// An owned syntax tree lifted from a tree-sitter CST.
///
/// Nodes live in an arena in pre-order (the root is index 0). The lift
/// keeps named and anonymous nodes — anonymous tokens like `+` carry
/// meaning through their kind — but drops `extra` nodes (comments), and
/// parsing rejects sources whose CST contains error or missing nodes:
/// structural merge requires syntactically valid inputs.
#[derive(Debug)]
pub struct Tree {
    lang: &'static Lang,
    source: String,
    nodes: Vec<NodeData>,
    hashes: Vec<u64>,
}

#[derive(Debug)]
struct NodeData {
    kind: &'static str,
    kind_id: u16,
    named: bool,
    span: Range<usize>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    field_id: Option<NonZeroU16>,
}

impl Tree {
    /// Parses `source` with `lang`'s grammar and lifts the CST.
    ///
    /// Fails with [`Error::Parse`] when the CST contains error or
    /// missing nodes. The error's `path` is empty because the source
    /// arrives here as text; callers that read from a file rewrap the
    /// error with the real path.
    pub fn parse(source: &str, lang: &'static Lang) -> Result<Self, Error> {
        let mut parser = tree_sitter::Parser::new();
        // Failure here means the grammar crate was built against an
        // incompatible tree-sitter ABI — a build-time invariant
        // violation, not a runtime error to recover from.
        #[allow(clippy::expect_used)]
        parser
            .set_language(lang.language())
            .expect("grammar crate is ABI-compatible with tree-sitter");
        // `parse` returns None only without a language or on
        // cancellation; we just set the language and set no
        // cancellation flag.
        #[allow(clippy::expect_used)]
        let cst = parser
            .parse(source, None)
            .expect("parser has a language and no cancellation flag");

        let root = cst.root_node();
        if has_error_or_missing(root) {
            return Err(Error::Parse {
                path: PathBuf::new(),
                lang: lang.name().into(),
            });
        }

        let nodes = lift(root);
        let mut tree = Self {
            lang,
            source: source.into(),
            nodes,
            hashes: Vec::new(),
        };
        tree.hashes = crate::hash::compute(&tree);
        Ok(tree)
    }

    /// The language this tree was parsed with.
    pub fn lang(&self) -> &'static Lang {
        self.lang
    }

    /// The root node (arena index 0).
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Whether the node is named (a grammar rule) as opposed to an
    /// anonymous token like `+`.
    pub fn is_named(&self, id: NodeId) -> bool {
        self.node(id).named
    }

    /// All nodes in pre-order.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.nodes.len()).map(NodeId)
    }

    /// The node's grammar kind name, e.g. `"function_item"`.
    pub fn kind(&self, id: NodeId) -> &'static str {
        self.node(id).kind
    }

    /// The node's grammar kind id. Diffing matches nodes by kind id;
    /// nodes of different kinds never match.
    pub fn kind_id(&self, id: NodeId) -> u16 {
        self.node(id).kind_id
    }

    /// The node's label: its source text, for named leaves only.
    ///
    /// Anonymous tokens and interior nodes have no label — this
    /// restrains relabeling to meaningful cases (identifiers,
    /// literals); structural nodes match by kind alone.
    pub fn label(&self, id: NodeId) -> Option<&str> {
        let node = self.node(id);
        if node.named && node.children.is_empty() {
            self.source.get(node.span.clone())
        } else {
            None
        }
    }

    /// The node's byte range in the source.
    pub fn span(&self, id: NodeId) -> Range<usize> {
        self.node(id).span.clone()
    }

    /// The source text under a byte range, if the range is valid.
    pub fn source_slice(&self, span: Range<usize>) -> Option<&str> {
        self.source.get(span)
    }

    /// The source length in bytes.
    pub fn source_len(&self) -> usize {
        self.source.len()
    }

    /// The node's children, in source order.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    /// The node's parent; `None` for the root.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    /// The grammar field this node fills in its parent, if any —
    /// e.g. a function's identifier sits in `function_item`'s `name`
    /// field.
    pub fn field_id(&self, id: NodeId) -> Option<NonZeroU16> {
        self.node(id).field_id
    }

    /// The node's subtree Merkle hash: (kind_id, label, child hashes),
    /// position-independent. Equal hashes mean structurally identical
    /// subtrees, within or across trees.
    pub fn hash(&self, id: NodeId) -> u64 {
        // Same invariant as node(): ids index this arena.
        #[allow(clippy::indexing_slicing)]
        self.hashes[id.0]
    }

    fn node(&self, id: NodeId) -> &NodeData {
        // NodeIds only come from this arena's own accessors, so an
        // out-of-range index means ids were mixed across trees — a
        // logic bug where panicking beats returning wrong data.
        #[allow(clippy::indexing_slicing)]
        &self.nodes[id.0]
    }
}

/// Walks the whole CST — including `extra` subtrees the lift skips —
/// looking for error or missing nodes.
fn has_error_or_missing(root: tree_sitter::Node) -> bool {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

/// Lifts the CST into arena nodes, pre-order, skipping `extra` nodes.
///
/// Iterative with an explicit stack — recursing per CST level blows the
/// call stack on deeply nested sources.
fn lift(root: tree_sitter::Node) -> Vec<NodeData> {
    let mut nodes: Vec<NodeData> = Vec::new();
    let mut stack: Vec<(tree_sitter::Node, Option<NodeId>, Option<NonZeroU16>)> =
        vec![(root, None, None)];
    while let Some((node, parent, field_id)) = stack.pop() {
        let id = NodeId(nodes.len());
        nodes.push(NodeData {
            kind: node.kind(),
            kind_id: node.kind_id(),
            named: node.is_named(),
            span: node.byte_range(),
            parent,
            children: Vec::new(),
            field_id,
        });
        if let Some(NodeId(parent_index)) = parent {
            // Parent ids are arena indices this loop created earlier.
            #[allow(clippy::expect_used)]
            nodes
                .get_mut(parent_index)
                .expect("parent id is a valid arena index")
                .children
                .push(id);
        }
        // Children go on the stack reversed so the leftmost child pops
        // first, giving pre-order arena indices and in-order `children`.
        let mut cursor = node.walk();
        let mut kids = Vec::new();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if !child.is_extra() {
                    kids.push((child, Some(id), cursor.field_id()));
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        stack.extend(kids.into_iter().rev());
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::Lang;

    fn lang(name: &str) -> Result<&'static Lang, Error> {
        Lang::by_name(name).ok_or(Error::UnknownLanguage { path: name.into() })
    }

    fn parse_rust(src: &str) -> Result<Tree, Error> {
        Tree::parse(src, lang("rust")?)
    }

    #[test]
    fn lifts_a_simple_function() -> Result<(), Error> {
        let t = parse_rust("fn main() {}")?;
        let root = t.root();
        assert_eq!(t.kind(root), "source_file");
        // An identifier leaf carries its source text as its label.
        let ids: Vec<_> = t.nodes().filter(|&n| t.kind(n) == "identifier").collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids.first().and_then(|&n| t.label(n)), Some("main"));
        Ok(())
    }

    #[test]
    fn anonymous_tokens_are_lifted_without_labels() -> Result<(), Error> {
        let t = parse_rust("fn f() -> i32 { 1 + 2 }")?;
        let plus: Vec<_> = t.nodes().filter(|&n| t.kind(n) == "+").collect();
        assert_eq!(plus.len(), 1);
        assert_eq!(plus.first().and_then(|&n| t.label(n)), None);
        Ok(())
    }

    #[test]
    fn comments_are_excluded() -> Result<(), Error> {
        let t = parse_rust("// hello\nfn main() {}")?;
        assert!(t.nodes().all(|n| t.kind(n) != "line_comment"));
        Ok(())
    }

    #[test]
    fn parse_errors_are_rejected() -> Result<(), Error> {
        assert!(Tree::parse("fn main( {", lang("rust")?).is_err());
        Ok(())
    }

    #[test]
    fn missing_nodes_are_rejected() -> Result<(), Error> {
        // An unclosed mod recovers via a zero-width MISSING `}` with no
        // ERROR node anywhere, so this exercises the is_missing check.
        assert!(Tree::parse("mod m { fn f() {} ", lang("rust")?).is_err());
        Ok(())
    }

    #[test]
    fn spans_reconstruct_source() -> Result<(), Error> {
        let src = "fn main() {}";
        let t = parse_rust(src)?;
        assert_eq!(src.get(t.span(t.root())), Some(src));
        Ok(())
    }

    #[test]
    fn parent_and_children_are_consistent() -> Result<(), Error> {
        let t = parse_rust("fn main() {}")?;
        let root = t.root();
        assert_eq!(t.parent(root), None);
        assert!(!t.children(root).is_empty());
        for &child in t.children(root) {
            assert_eq!(t.parent(child), Some(root));
        }
        Ok(())
    }

    #[test]
    fn nodes_iterates_in_preorder() -> Result<(), Error> {
        let t = Tree::parse("[1, 2]", lang("json")?)?;
        assert_eq!(t.nodes().next(), Some(t.root()));
        // Pre-order visits the numbers in source order.
        let labels: Vec<_> = t
            .nodes()
            .filter(|&n| t.kind(n) == "number")
            .map(|n| t.label(n))
            .collect();
        assert_eq!(labels, vec![Some("1"), Some("2")]);
        Ok(())
    }

    #[test]
    fn field_ids_record_the_slot_within_the_parent() -> Result<(), Error> {
        let lang = lang("rust")?;
        let t = Tree::parse("fn main() {}", lang)?;
        let name_field = lang.language().field_id_for_name("name");
        assert!(name_field.is_some());
        let ident = t.nodes().find(|&n| t.kind(n) == "identifier");
        assert_eq!(ident.and_then(|n| t.field_id(n)), name_field);
        // The function item itself sits in no field of source_file.
        let item = t.nodes().find(|&n| t.kind(n) == "function_item");
        assert_eq!(item.and_then(|n| t.field_id(n)), None);
        Ok(())
    }

    #[test]
    fn kind_ids_distinguish_kinds_not_positions() -> Result<(), Error> {
        let t = parse_rust("fn a() {}\nfn b() {}")?;
        let kind_ids: Vec<_> = t
            .nodes()
            .filter(|&n| t.kind(n) == "function_item")
            .map(|n| t.kind_id(n))
            .collect();
        assert_eq!(kind_ids.len(), 2);
        assert_eq!(kind_ids.first(), kind_ids.last());
        assert!(kind_ids.first() != Some(&t.kind_id(t.root())));
        Ok(())
    }
}
