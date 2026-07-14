//! The universality checker: the oracle that decides whether a merge M
//! of (O, A, B) is the pushout the design demands — every branch edit
//! lands, nothing else does.
//!
//! Fidelity is bounded by diff quality (the paper carries the same
//! caveat): the four conditions are set-membership statements over the
//! diffs f: O→A, g: O→B, i1: A→M, i2: B→M.

use std::ops::Range;

use crate::diff::{Matching, diff};
use crate::tree::{NodeId, Tree};

/// Which branch a violation's witness lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    A,
    B,
}

/// One universality violation, with the witness node and its span in
/// the tree named by the variant (M, a branch, or O).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Violation {
    #[error("extra insertion: merge node at {span:?} comes from neither branch")]
    ExtraInsertion { node: NodeId, span: Range<usize> },

    #[error("missed insertion: branch {branch:?} inserted at {span:?} but the merge dropped it")]
    MissedInsertion {
        branch: Branch,
        node: NodeId,
        span: Range<usize>,
    },

    #[error("extra deletion: both branches kept the node at {span:?} but the merge lost it")]
    ExtraDeletion { node: NodeId, span: Range<usize> },

    #[error("missed deletion: a branch deleted the node at {span:?} but the merge kept it")]
    MissedDeletion { node: NodeId, span: Range<usize> },
}

/// The checker's verdict on one (O, A, B, M) quadruple.
#[derive(Debug)]
pub struct Report {
    /// Whether M parsed at all. [`check`] itself always sets this;
    /// the merge pipeline reports an unparsable synthesis by hand.
    pub parsable: bool,
    pub violations: Vec<Violation>,
}

impl Report {
    /// A correct merge: parsable with no violations.
    pub fn is_correct(&self) -> bool {
        self.parsable && self.violations.is_empty()
    }
}

/// Checks M against the four universality conditions.
pub fn check(o: &Tree, a: &Tree, b: &Tree, m: &Tree) -> Report {
    let f = diff(o, a);
    let g = diff(o, b);
    let i1 = diff(a, m);
    let i2 = diff(b, m);
    let mut violations = Vec::new();

    // 1. No extra insertion: every M node has a preimage under i1 or
    //    i2 — it came from somewhere.
    for node in m.nodes() {
        if fungible_separator(m, node) {
            continue;
        }
        if i1.preimage(node).is_none() && i2.preimage(node).is_none() {
            violations.push(Violation::ExtraInsertion {
                node,
                span: m.span(node),
            });
        }
    }

    // 2. No missed insertion: every node a branch inserted reaches M.
    missed_insertions(a, &f, &i1, Branch::A, &mut violations);
    missed_insertions(b, &g, &i2, Branch::B, &mut violations);

    // 3. No extra deletion: an O node both branches kept must reach M
    //    through both routes, and the routes must agree (i1∘f = i2∘g).
    for node in o.nodes() {
        if fungible_separator(o, node) {
            continue;
        }
        if let (Some(in_a), Some(in_b)) = (f.image(node), g.image(node)) {
            let via_a = i1.image(in_a);
            let via_b = i2.image(in_b);
            let commutes = matches!((via_a, via_b), (Some(x), Some(y)) if x == y);
            if !commutes {
                violations.push(Violation::ExtraDeletion {
                    node,
                    span: o.span(node),
                });
            }
        }
    }

    // 4. No missed deletion: an O node one branch deleted must not
    //    sneak into M through the other branch.
    for node in o.nodes() {
        if fungible_separator(o, node) {
            continue;
        }
        let reaches = match (f.image(node), g.image(node)) {
            (None, Some(via)) => i2.image(via).is_some(),
            (Some(via), None) => i1.image(via).is_some(),
            _ => false,
        };
        if reaches {
            violations.push(Violation::MissedDeletion {
                node,
                span: o.span(node),
            });
        }
    }

    Report {
        parsable: true,
        violations,
    }
}

/// Anonymous tokens under a commutative parent are fungible
/// separators, exempt from all four conditions. A cross-branch union
/// merge makes their attribution ambiguous — both branches' commas
/// legitimately claim the same merged comma, orphaning its twin — and
/// the parse itself (verified before the conditions run) already pins
/// how many separators the output holds. Named nodes under the same
/// parent stay fully checked.
fn fungible_separator(tree: &Tree, node: NodeId) -> bool {
    !tree.is_named(node)
        && tree
            .parent(node)
            .is_some_and(|parent| tree.lang().is_commutative(tree.kind(parent)))
}

/// Condition 2 for one branch: nodes the branch inserted (no preimage
/// under its diff from O) must have an image in M.
fn missed_insertions(
    branch_tree: &Tree,
    from_o: &Matching,
    to_m: &Matching,
    branch: Branch,
    violations: &mut Vec<Violation>,
) {
    for node in branch_tree.nodes() {
        if fungible_separator(branch_tree, node) {
            continue;
        }
        if from_o.preimage(node).is_none() && to_m.image(node).is_none() {
            violations.push(Violation::MissedInsertion {
                branch,
                node,
                span: branch_tree.span(node),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::lang::Lang;

    fn parse_json(src: &str) -> Result<Tree, Error> {
        let lang = Lang::by_name("json").ok_or(Error::UnknownLanguage {
            path: "json".into(),
        })?;
        Tree::parse(src, lang)
    }

    fn check_json(o: &str, a: &str, b: &str, m: &str) -> Result<Report, Error> {
        let o = parse_json(o)?;
        let a = parse_json(a)?;
        let b = parse_json(b)?;
        let m = parse_json(m)?;
        Ok(check(&o, &a, &b, &m))
    }

    #[test]
    fn extra_insertion_is_flagged() -> Result<(), Error> {
        // Nobody wrote 9; the merge invented it.
        let report = check_json("[1]", "[1]", "[1]", "[1, 9]")?;
        assert!(!report.violations.is_empty());
        assert!(
            report
                .violations
                .iter()
                .all(|v| matches!(v, Violation::ExtraInsertion { .. }))
        );
        // The 9 itself is among the witnesses.
        let m = parse_json("[1, 9]")?;
        let witnesses_nine = report.violations.iter().any(|v| match v {
            Violation::ExtraInsertion { span, .. } => m.source_slice(span.clone()) == Some("9"),
            _ => false,
        });
        assert!(witnesses_nine);
        Ok(())
    }

    #[test]
    fn missed_insertion_is_flagged() -> Result<(), Error> {
        // The paper's Figure 10: A inserts 3, B deletes 1. A merge
        // that drops the insertion must be flagged...
        let report = check_json("[1, 2]", "[1, 2, 3]", "[2]", "[2]")?;
        assert!(!report.violations.is_empty());
        assert!(report.violations.iter().all(|v| matches!(
            v,
            Violation::MissedInsertion {
                branch: Branch::A,
                ..
            }
        )));
        // ...and the one that keeps it must pass.
        let report = check_json("[1, 2]", "[1, 2, 3]", "[2]", "[2, 3]")?;
        assert_eq!(report.violations, Vec::new());
        assert!(report.is_correct());
        Ok(())
    }

    #[test]
    fn missed_insertion_is_flagged_for_branch_b() -> Result<(), Error> {
        let report = check_json("[1, 2]", "[2]", "[1, 2, 3]", "[2]")?;
        assert!(!report.violations.is_empty());
        assert!(report.violations.iter().all(|v| matches!(
            v,
            Violation::MissedInsertion {
                branch: Branch::B,
                ..
            }
        )));
        Ok(())
    }

    #[test]
    fn extra_deletion_is_flagged() -> Result<(), Error> {
        // Both branches kept 2; the merge dropped it.
        let report = check_json("[1, 2]", "[1, 2]", "[1, 2]", "[1]")?;
        assert!(!report.violations.is_empty());
        assert!(
            report
                .violations
                .iter()
                .all(|v| matches!(v, Violation::ExtraDeletion { .. }))
        );
        Ok(())
    }

    #[test]
    fn missed_deletion_is_flagged() -> Result<(), Error> {
        // A deleted 1; the merge kept it anyway.
        let report = check_json("[1, 2]", "[2]", "[1, 2]", "[1, 2]")?;
        assert!(!report.violations.is_empty());
        assert!(
            report
                .violations
                .iter()
                .all(|v| matches!(v, Violation::MissedDeletion { .. }))
        );
        // And symmetrically when B is the deleting branch (the node
        // reaches M through A's route).
        let report = check_json("[1, 2]", "[1, 2]", "[2]", "[1, 2]")?;
        assert!(!report.violations.is_empty());
        assert!(
            report
                .violations
                .iter()
                .all(|v| matches!(v, Violation::MissedDeletion { .. }))
        );
        Ok(())
    }

    #[test]
    fn a_universal_merge_passes() -> Result<(), Error> {
        // The paper's Figures 2/3: disjoint edits, including B's 2→6
        // relabel, all land.
        let report = check_json(
            "[1, 2, 3]",
            "[1, 2, 4, 5, 3]",
            "[1, 6, 3]",
            "[1, 6, 4, 5, 3]",
        )?;
        assert_eq!(report.violations, Vec::new());
        assert!(report.is_correct());
        Ok(())
    }

    #[test]
    fn identical_inputs_pass() -> Result<(), Error> {
        let report = check_json("[1]", "[1]", "[1]", "[1]")?;
        assert!(report.is_correct());
        Ok(())
    }

    #[test]
    fn union_merged_separators_are_not_extra_insertions() -> Result<(), Error> {
        // Each branch adds one entry, each with its comma. The two
        // matchings into M are derived independently, so both commas
        // can claim the same merged comma, orphaning its twin; the
        // fungible-separator exemption keeps that from flagging.
        let report = check_json(
            r#"{"a": 1}"#,
            r#"{"a": 1, "b": 2}"#,
            r#"{"a": 1, "c": 3}"#,
            r#"{"a": 1, "b": 2, "c": 3}"#,
        )?;
        assert_eq!(report.violations, Vec::new());
        assert!(report.is_correct());
        Ok(())
    }

    #[test]
    fn named_nodes_under_commutative_parents_stay_checked() -> Result<(), Error> {
        // The exemption covers anonymous separators only: an invented
        // pair inside the object still flags.
        let report = check_json(
            r#"{"a": 1}"#,
            r#"{"a": 1, "b": 2}"#,
            r#"{"a": 1}"#,
            r#"{"a": 1, "b": 2, "d": 4}"#,
        )?;
        assert!(!report.violations.is_empty());
        assert!(report.violations.iter().any(|v| match v {
            Violation::ExtraInsertion { span, .. } => {
                let m = parse_json(r#"{"a": 1, "b": 2, "d": 4}"#);
                m.is_ok_and(|m| {
                    m.source_slice(span.clone())
                        .is_some_and(|s| s.contains("4"))
                })
            }
            _ => false,
        }));
        Ok(())
    }

    #[test]
    fn separators_under_non_commutative_parents_stay_checked() -> Result<(), Error> {
        // An array is not commutative, so a merge that loses an
        // element (and its comma) both branches kept flags the comma
        // too — the exemption must not reach it.
        let report = check_json("[1, 2]", "[1, 2]", "[1, 2]", "[1]")?;
        let flags_comma = report.violations.iter().any(|v| match v {
            Violation::ExtraDeletion { span, .. } => {
                let o = parse_json("[1, 2]");
                o.is_ok_and(|o| o.source_slice(span.clone()) == Some(","))
            }
            _ => false,
        });
        assert!(flags_comma, "{:?}", report.violations);
        Ok(())
    }
}
