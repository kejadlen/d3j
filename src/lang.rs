use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use serde::Deserialize;

/// A supported grammar: its tree-sitter parser plus `node-types.json`
/// metadata.
///
/// Instances are `'static` — one per supported language, built lazily on
/// first use. `detect` and `by_name` are the only ways to obtain one.
#[derive(Debug)]
pub struct Lang {
    name: &'static str,
    language: tree_sitter::Language,
    node_types: NodeTypes,
}

impl Lang {
    /// Detects the language for `path` from its extension.
    ///
    /// Matching is deliberately case-sensitive: `foo.RS` is not detected
    /// as rust. Returns `None` for extensions with no registered grammar
    /// (or no extension at all); callers fall back to `--lang` in that
    /// case.
    pub fn detect(path: &Path) -> Option<&'static Lang> {
        let name = match path.extension()?.to_str()? {
            "rs" => "rust",
            "java" => "java",
            "json" => "json",
            _ => return None,
        };
        Self::by_name(name)
    }

    /// Looks up a language by its canonical name (`"rust"`, `"java"`,
    /// `"json"`).
    pub fn by_name(name: &str) -> Option<&'static Lang> {
        match name {
            "rust" => Some(&*RUST),
            "java" => Some(&*JAVA),
            "json" => Some(&*JSON),
            _ => None,
        }
    }

    /// The language's canonical name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The tree-sitter grammar, for parsing source into a `Tree`.
    pub fn language(&self) -> &tree_sitter::Language {
        &self.language
    }

    /// The grammar's `node-types.json` metadata.
    pub fn node_types(&self) -> &NodeTypes {
        &self.node_types
    }

    fn new(name: &'static str, language: tree_sitter::Language, node_types_json: &str) -> Self {
        // `node_types_json` is the grammar crate's own bundled NODE_TYPES
        // constant, not external input — a parse failure here means the
        // grammar crate shipped malformed JSON, a build-time invariant
        // violation rather than a runtime error to recover from.
        #[allow(clippy::expect_used)]
        let node_types = NodeTypes::parse(node_types_json)
            .expect("grammar crate NODE_TYPES is well-formed JSON");
        Self {
            name,
            language,
            node_types,
        }
    }
}

static RUST: LazyLock<Lang> = LazyLock::new(|| {
    Lang::new(
        "rust",
        tree_sitter_rust::LANGUAGE.into(),
        tree_sitter_rust::NODE_TYPES,
    )
});

static JAVA: LazyLock<Lang> = LazyLock::new(|| {
    Lang::new(
        "java",
        tree_sitter_java::LANGUAGE.into(),
        tree_sitter_java::NODE_TYPES,
    )
});

static JSON: LazyLock<Lang> = LazyLock::new(|| {
    Lang::new(
        "json",
        tree_sitter_json::LANGUAGE.into(),
        tree_sitter_json::NODE_TYPES,
    )
});

/// Parsed `node-types.json`: grammar metadata keyed by node kind name.
///
/// Models only what the arity/category conflict rule (merged nodes'
/// children must satisfy the grammar) needs: each kind's fixed field
/// slots, its catch-all children slot, and — for supertypes — its
/// subtypes. Everything else in the JSON is ignored.
#[derive(Debug)]
pub struct NodeTypes {
    by_kind: HashMap<String, NodeType>,
}

impl NodeTypes {
    /// Whether `kind` has fixed field slots, as opposed to (or alongside)
    /// an unordered children list.
    pub fn has_fields(&self, kind: &str) -> bool {
        self.by_kind
            .get(kind)
            .is_some_and(|node_type| !node_type.fields.is_empty())
    }

    /// The field, children, and subtype constraints for `kind`, if the
    /// grammar defines it.
    pub fn get(&self, kind: &str) -> Option<&NodeType> {
        self.by_kind.get(kind)
    }

    fn parse(json: &str) -> Result<Self, serde_json::Error> {
        let entries: Vec<Entry> = serde_json::from_str(json)?;
        // Keep only named entries: a kind name can appear twice — once
        // named (carrying fields/children/subtypes) and once as an
        // anonymous token with no metadata (e.g. rust's `block`, java's
        // `throws`). Collecting both would let the empty anonymous entry
        // clobber the real one, and anonymous tokens have no arity to
        // validate anyway.
        let by_kind = entries
            .into_iter()
            .filter(|entry| entry.named)
            .map(|entry| {
                let node_type = NodeType {
                    fields: entry.fields,
                    children: entry.children,
                    subtypes: entry.subtypes,
                };
                (entry.kind, node_type)
            })
            .collect();
        Ok(Self { by_kind })
    }
}

/// One node kind's field, children, and subtype constraints.
#[derive(Debug, Clone)]
pub struct NodeType {
    /// Named field slots, keyed by field name.
    pub fields: HashMap<String, Arity>,
    /// The unordered, catch-all children slot, if the grammar defines one
    /// for this kind.
    pub children: Option<Arity>,
    /// For supertypes, the concrete kinds it stands in for.
    pub subtypes: Vec<TypeRef>,
}

/// How many of which types a field or the children slot accepts.
#[derive(Debug, Clone, Deserialize)]
pub struct Arity {
    pub required: bool,
    pub multiple: bool,
    pub types: Vec<TypeRef>,
}

/// A child type reference: a node kind name plus whether it's a named
/// node, as opposed to an anonymous token.
#[derive(Debug, Clone, Deserialize)]
pub struct TypeRef {
    #[serde(rename = "type")]
    pub kind: String,
    pub named: bool,
}

/// One `node-types.json` array entry, before its kind name is split out
/// as a map key.
#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    named: bool,
    #[serde(default)]
    fields: HashMap<String, Arity>,
    #[serde(default)]
    children: Option<Arity>,
    #[serde(default)]
    subtypes: Vec<TypeRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_language_from_extension() {
        assert_eq!(
            Lang::detect(Path::new("foo.rs")).map(Lang::name),
            Some("rust")
        );
        assert_eq!(
            Lang::detect(Path::new("Foo.java")).map(Lang::name),
            Some("java")
        );
        assert_eq!(
            Lang::detect(Path::new("foo.json")).map(Lang::name),
            Some("json")
        );
        assert!(Lang::detect(Path::new("foo.zig")).is_none());
    }

    #[test]
    fn by_name_returns_none_for_unregistered_language() {
        assert!(Lang::by_name("zig").is_none());
    }

    #[test]
    fn node_types_metadata_is_loaded() {
        // binary_expression has fields => fixed slots exist for it.
        let has_fields =
            Lang::by_name("rust").map(|lang| lang.node_types().has_fields("binary_expression"));
        assert_eq!(has_fields, Some(true));
    }

    #[test]
    fn fields_carry_required_multiple_and_types() {
        let left_arity = Lang::by_name("rust")
            .and_then(|lang| lang.node_types().get("binary_expression"))
            .and_then(|node_type| node_type.fields.get("left"));
        assert!(matches!(
            left_arity,
            Some(Arity {
                required: true,
                multiple: false,
                ..
            })
        ));
    }

    #[test]
    fn children_slot_is_loaded_for_kinds_without_fields() {
        // JSON's `array` node has an unordered children list, not fields.
        let children = Lang::by_name("json")
            .and_then(|lang| lang.node_types().get("array"))
            .and_then(|node_type| node_type.children.as_ref());
        assert!(matches!(children, Some(Arity { multiple: true, .. })));
    }

    #[test]
    fn named_entry_wins_over_anonymous_duplicate() {
        // Rust's `block` appears twice in node-types.json: the named
        // entry has a children arity, the anonymous token entry has
        // nothing. The named entry's metadata must survive the merge
        // into the by-kind map.
        let children = Lang::by_name("rust")
            .and_then(|lang| lang.node_types().get("block"))
            .and_then(|node_type| node_type.children.as_ref());
        assert!(matches!(
            children,
            Some(Arity {
                required: false,
                multiple: true,
                types,
            }) if types.iter().any(|t| t.kind == "_expression")
        ));
    }

    #[test]
    fn java_node_types_content_is_loaded() {
        // `throws` is another named/anonymous duplicate; assert the
        // named entry's real arity so java's metadata is genuinely
        // checked, not just parsed.
        let throws_children = Lang::by_name("java")
            .and_then(|lang| lang.node_types().get("throws"))
            .and_then(|node_type| node_type.children.as_ref());
        assert!(matches!(
            throws_children,
            Some(Arity {
                required: true,
                multiple: true,
                types,
            }) if types.iter().any(|t| t.kind == "_type")
        ));

        let has_operator = Lang::by_name("java").is_some_and(|lang| {
            lang.node_types()
                .get("binary_expression")
                .is_some_and(|node_type| node_type.fields.contains_key("operator"))
        });
        assert!(has_operator);
    }

    #[test]
    fn supertype_subtypes_are_loaded() {
        let has_const_item = Lang::by_name("rust").map(|lang| {
            lang.node_types()
                .get("_declaration_statement")
                .is_some_and(|node_type| node_type.subtypes.iter().any(|t| t.kind == "const_item"))
        });
        assert_eq!(has_const_item, Some(true));
    }

    #[test]
    fn language_accessor_exposes_grammar() {
        let node_kind_count = Lang::by_name("json").map(|lang| lang.language().node_kind_count());
        assert!(node_kind_count.is_some_and(|count| count > 0));
    }

    #[test]
    fn unknown_kind_has_no_fields_or_metadata() {
        let lang = Lang::by_name("rust");
        assert_eq!(
            lang.map(|lang| lang.node_types().has_fields("not_a_real_kind")),
            Some(false)
        );
        assert!(lang.is_some_and(|lang| lang.node_types().get("not_a_real_kind").is_none()));
    }
}
