//! The prov → flower schema adapter, for a workspace's *content* documents.
//!
//! prov detects a workspace's controlled vocabularies and relations by resolving
//! its config; flower renders and validates. This module is the seam between them:
//! it turns a resolved [`WorkspaceConfig`] (plus the vocabularies its controlled
//! fields point at) into a generic [`flower_core::Schema`]. flower-core never
//! learns the word "prov"; this crate owns the translation.
//!
//! - Each `fields.<name>` → a rule carrying its declared type. A field that also
//!   names a vocabulary gets a [`Constraint::Enum`]; one that only declares a
//!   type gets a typed rule with no constraint — enough for the editor to render
//!   the right widget (a `date` field gets a date picker) without claiming any
//!   value is illegal. Both a scalar-at-key rule and an each-item rule are
//!   emitted, so the field is governed whether written as a single value
//!   (`audience: public`) or a list (`audience: [public, private]`).
//! - Each relation → a [`Constraint::Reference`]; the spanning relation is
//!   flagged `spanning: true`, a many-relation also governs each item (a list of
//!   links).
//!
//! For the *config* document rather than the content documents, see
//! [`crate::config_schema`].

use std::collections::BTreeMap;

use flower_core::schema::{Constraint, FieldRule, Schema};
use flower_core::{Cardinality, FieldType, Icon, PathPat, Presentation, Term, Tint};
use prov::{Cardinality as ProvCardinality, OpenClosed, Vocabulary, WorkspaceConfig};

/// Build a flower [`Schema`] from a resolved prov workspace config and the
/// vocabularies its controlled fields point at (keyed by field name). Vocabularies
/// the caller could not load are simply absent, yielding an enum with no offered
/// terms — still a rule (so a closed field with no store rejects everything, which
/// is the honest signal that its vocabulary is missing).
pub fn schema_from_config(
    config: &WorkspaceConfig,
    vocabularies: &BTreeMap<String, Vocabulary>,
) -> Schema {
    let mut rules = Vec::new();

    // Field declarations → a typed rule, carrying an Enum constraint when the
    // field also names a vocabulary.
    for (field, spec) in &config.fields {
        // prov's declared type wins. A controlled field that declares none is
        // text, because that is what a vocabulary term is.
        let ty = spec
            .ty
            .or_else(|| spec.vocabulary.as_ref().map(|_| FieldType::Str));
        // Only a field with a vocabulary constrains its values; a type-only
        // field renders as its type and rejects nothing.
        let constraint = spec.vocabulary.as_ref().map(|_| Constraint::Enum {
            values: vocabularies.get(field).map(vocab_terms).unwrap_or_default(),
            closed: matches!(spec.values, OpenClosed::Closed),
        });
        let icon = if constraint.is_some() {
            Icon::Tag
        } else {
            icon_for(ty)
        };
        let rule = |at: PathPat| {
            FieldRule::new(at)
                .ty(ty)
                .constraint_opt(constraint.clone())
                .present(Presentation::default().icon(icon.clone()))
        };
        rules.push(rule(PathPat::key(field.clone())));
        rules.push(rule(PathPat::each_item_of(field.clone())));
    }

    // Relations → Reference constraints. The spanning relation is the containment
    // backbone; the rest are overlay links.
    let relations = config.relation_set();
    let spanning = relations.spanning_relation().map(str::to_string);
    for rel in relations.relations() {
        let is_spanning = spanning.as_deref() == Some(rel.name.as_str());
        let cardinality = match rel.cardinality {
            ProvCardinality::One => Cardinality::One,
            ProvCardinality::Many => Cardinality::Many,
        };
        let reference = |at: PathPat| {
            FieldRule::new(at)
                .ty(FieldType::Ref)
                .constraint(Constraint::Reference {
                    relation: rel.name.clone(),
                    cardinality,
                    spanning: is_spanning,
                })
                .present(
                    Presentation::default()
                        .icon(Icon::Link)
                        .tint(is_spanning.then_some(Tint::Accent)),
                )
        };
        rules.push(reference(PathPat::key(rel.name.clone())));
        // A many-relation is a list of links: also govern each item.
        if matches!(rel.cardinality, ProvCardinality::Many) {
            rules.push(reference(PathPat::each_item_of(rel.name.clone())));
        }
    }

    Schema::new(rules)
}

/// The icon a field's declared type suggests. Only a hint — flower picks the
/// widget from `ty` itself; this is what the row is labelled with.
fn icon_for(ty: Option<FieldType>) -> Icon {
    use fig::ExtKind::{LocalDate, LocalDateTime, LocalTime, OffsetDateTime};
    match ty {
        Some(FieldType::Extended(OffsetDateTime | LocalDateTime | LocalDate | LocalTime)) => {
            Icon::Clock
        }
        Some(FieldType::Bool) => Icon::Toggle,
        Some(FieldType::Ref) => Icon::Link,
        _ => Icon::Text,
    }
}

/// Translate a prov vocabulary's terms into flower [`Term`]s. prov owns the term
/// keys, each term's `means` (a human gloss), and its `retired` flag; the rest of
/// a term's payload is carried by prov and not surfaced here.
fn vocab_terms(vocab: &Vocabulary) -> Vec<Term> {
    vocab
        .terms
        .iter()
        .map(|(name, term)| {
            Term::value(name.clone())
                .description_opt(term.means.clone())
                .retired(term.retired)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    // `.enum_constraint()`/`.reference()` are an extension trait now that
    // `FieldRule` is fig-schema's generic type.
    use flower_core::{FieldRuleExt, Seg};

    fn audience_config() -> (WorkspaceConfig, BTreeMap<String, Vocabulary>) {
        let mut config = WorkspaceConfig::default();
        config.fields.insert(
            "audience".to_string(),
            prov::FieldSpec {
                ty: None,
                values: OpenClosed::Closed,
                vocabulary: Some("audiences.yaml".to_string()),
                reify: false,
            },
        );
        // A type with no vocabulary — nothing to validate against, but the
        // editor still needs to know it is a date.
        config.fields.insert(
            "created".to_string(),
            prov::FieldSpec {
                ty: Some(prov::FieldType::Extended(prov::ExtKind::LocalDate)),
                values: OpenClosed::default(),
                vocabulary: None,
                reify: false,
            },
        );

        let mut terms = BTreeMap::new();
        terms.insert(
            "public".to_string(),
            prov::Term {
                id: None,
                means: Some("Anyone".to_string()),
                retired: false,
            },
        );
        terms.insert(
            "private".to_string(),
            prov::Term {
                id: None,
                means: None,
                retired: false,
            },
        );
        let mut vocabs = BTreeMap::new();
        vocabs.insert(
            "audience".to_string(),
            Vocabulary {
                field: "audience".to_string(),
                values: OpenClosed::Closed,
                terms,
            },
        );
        (config, vocabs)
    }

    #[test]
    fn a_closed_field_becomes_a_closed_enum_over_each_item() {
        let (config, vocabs) = audience_config();
        let schema = schema_from_config(&config, &vocabs);

        // The list-item form is governed (an `audience:` sequence item).
        let rule = schema
            .rule_for(&[Seg::Key("audience".into()), Seg::Index(0)])
            .expect("an each-item rule for audience");
        let (terms, closed) = rule.enum_constraint().expect("an enum constraint");
        assert!(closed, "the field declares `values: closed`");
        assert!(terms.iter().any(|t| t.value == "public"));
        assert!(terms.iter().any(|t| t.value == "private"));
        // The scalar form is governed too.
        assert!(
            schema
                .rule_for(&[Seg::Key("audience".into())])
                .and_then(|r| r.enum_constraint())
                .is_some()
        );
    }

    /// The point of prov's `type` axis: a field nothing controls still reaches
    /// the editor with its shape, so `created` renders as a date rather than a
    /// text box — including when it is empty and there is no value to guess from.
    #[test]
    fn a_typed_field_without_a_vocabulary_yields_a_typed_unconstrained_rule() {
        let (config, vocabs) = audience_config();
        let schema = schema_from_config(&config, &vocabs);

        let rule = schema
            .rule_for(&[Seg::Key("created".into())])
            .expect("a rule for created");
        assert_eq!(
            rule.ty,
            Some(FieldType::Extended(fig::ExtKind::LocalDate)),
            "the declared type reaches the editor"
        );
        assert!(
            rule.constraint.is_none(),
            "a type is not a claim about which values are legal"
        );
        assert_eq!(rule.present.icon, Some(Icon::Clock));
    }

    #[test]
    fn the_spanning_relation_becomes_a_spanning_reference() {
        let (config, vocabs) = audience_config();
        let schema = schema_from_config(&config, &vocabs);

        // prov's default relation set spans on `contents`.
        let rule = schema
            .rule_for(&[Seg::Key("contents".into())])
            .expect("a rule for contents");
        match &rule.constraint {
            Some(Constraint::Reference {
                relation, spanning, ..
            }) => {
                assert_eq!(relation, "contents");
                assert!(*spanning, "contents is the spanning backbone");
            }
            other => panic!("expected a spanning reference, got {other:?}"),
        }

        // An overlay relation (`part_of`) is a non-spanning reference.
        let part_of = schema
            .rule_for(&[Seg::Key("part_of".into())])
            .expect("a rule for part_of");
        assert_eq!(part_of.reference(), Some("part_of"));
        assert!(matches!(
            part_of.constraint,
            Some(Constraint::Reference {
                spanning: false,
                ..
            })
        ));
    }
}
