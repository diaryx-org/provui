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
//!   links). The relation's `means:` gloss travels with it, so a row can say
//!   what `part_of` is for rather than leaving a reader to infer it.
//! - prov's own **kernel** keys → [`kernel_rules`]. `title`, `id`, the
//!   opaque-payload axis and the root's inline `prov:` policy block are prov
//!   vocabulary exactly as much as `contents` is, and a document schema that
//!   governed only the *declared* fields would leave the keys prov always reads
//!   as untyped text boxes. They come **last**, so a workspace that declares a
//!   field of the same name shadows them.
//!
//! For the *config* document rather than the content documents, see
//! [`crate::config_schema`].

use std::collections::BTreeMap;

use flower_core::schema::{Constraint, FieldRule, Schema};
use flower_core::{Cardinality, FieldType, Icon, PathPat, Presentation, SegPat, Term, Tint};
use prov::{Cardinality as ProvCardinality, OpenClosed, Vocabulary, WorkspaceConfig};

use crate::facets::{self, Facets};
use crate::rules::{path, text, toggle};

/// Build a flower [`Schema`] from a resolved prov workspace config and the
/// vocabularies its controlled fields point at (keyed by field name). Vocabularies
/// the caller could not load are simply absent, yielding an enum with no offered
/// terms — still a rule (so a closed field with no store rejects everything, which
/// is the honest signal that its vocabulary is missing).
pub fn schema_from_config(
    config: &WorkspaceConfig,
    vocabularies: &BTreeMap<String, Vocabulary>,
) -> Schema {
    Schema::new(document_rules(config, vocabularies))
}

/// Each declared field with the declaration that governs the whole workspace —
/// the one without an `under:` — skipping a field declared only under indexes.
pub(crate) fn workspace_fields(
    config: &WorkspaceConfig,
) -> impl Iterator<Item = (&String, &prov::FieldSpec)> {
    config
        .fields
        .keys()
        .filter_map(|name| config.field(name).map(|spec| (name, spec)))
}

/// [`schema_from_config`]'s rules, before they become a schema — the
/// composition point for an application overlay, and the peer of
/// [`config_rules`](crate::config_schema::config_rules) one document over.
///
/// A [`Schema`] resolves a path by first match wins, so an app that governs its
/// own frontmatter keys *prepends* its rules to these. See [`crate::rules`] for
/// why the order is that way round.
pub fn document_rules(
    config: &WorkspaceConfig,
    vocabularies: &BTreeMap<String, Vocabulary>,
) -> Vec<FieldRule> {
    let mut rules = Vec::new();

    // Field declarations → a typed rule, carrying an Enum constraint when the
    // field also names a vocabulary. The declaration read is the one governing
    // the whole workspace: prov 0.12 lets a field be declared again `under:` an
    // index, and which of those governs a document is a question this schema
    // — built once per workspace, not per document — does not yet ask. A
    // field declared only under indexes reads as undeclared here, as it did
    // before prov could parse it.
    for (field, spec) in workspace_fields(config) {
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
    let facets = Facets::from_config(config);
    let relations = config.relation_set();
    let spanning = relations.spanning_relation().map(str::to_string);
    for rel in relations.relations() {
        let is_spanning = spanning.as_deref() == Some(rel.name.as_str());
        // The vocabulary's own gloss, or prov's for a preset relation nobody
        // declared. Carried by prov and never read back — which is exactly why
        // it is worth putting on the row: it is documentation that travels with
        // the data, and a reader who has it does not have to guess.
        let means = facets
            .of_key(&rel.name)
            .relation()
            .and_then(|r| r.means.clone());
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
                        .tint(is_spanning.then_some(Tint::Accent))
                        .description_opt(means.clone()),
                )
        };
        rules.push(reference(PathPat::key(rel.name.clone())));
        // A many-relation is a list of links: also govern each item.
        if matches!(rel.cardinality, ProvCardinality::Many) {
            rules.push(reference(PathPat::each_item_of(rel.name.clone())));
        }
    }

    // Last, so a workspace that declares `fields.title` shadows prov's own rule
    // for it rather than being shadowed by it.
    rules.extend(kernel_rules(config));
    rules
}

/// prov's own frontmatter keys — the ones every document may carry whether or
/// not the workspace declared anything.
///
/// The `fields` block says what *this* workspace controls; the kernel is what
/// prov reads regardless (spec §1). Without these, `id` and `content_hash`
/// arrive at a schema-driven editor as anonymous text boxes beside `mood`, and
/// the root's whole `prov:` policy block arrives as an untyped nested map — a
/// document schema that knows about `audience` and not about `id` has the story
/// backwards.
///
/// Two of them are more than presentation:
///
/// - **`attachment`** is a boolean, and typing `attachment: yes` where prov
///   wants `true` is a marker prov does not honour.
/// - **`prov:`** is the *same vocabulary as the config document*, nested one
///   level (spec §3: "the identical keys sit at top level" in a config
///   document, with no `prov:` wrapper). So it is governed by
///   [`config_rules`](crate::config_schema::config_rules) with every pattern
///   prefixed — one vocabulary, stated once, reaching both of its homes. A
///   workspace that inlines its policy gets the same pickers as one that keeps
///   a `prov.yaml`.
///
/// What is **not** here is a `Consequence` on any of it. These keys are managed
/// rather than costly — see [`Facet::managed`](crate::Facet::managed), which is
/// the question a frontend asks before offering an edit at all.
pub fn kernel_rules(config: &WorkspaceConfig) -> Vec<FieldRule> {
    // A row that says what the key is for. `described` is the local shorthand
    // for "the builder in `rules`, plus the one sentence a reader needs".
    let described = |mut rule: FieldRule, why: &str| {
        rule.present = std::mem::take(&mut rule.present).description(why);
        rule
    };
    let mut rules = vec![
        described(
            text(path(&[facets::TITLE_KEY]), "Title", Icon::Text),
            "The name a nominal reference resolves against.",
        ),
        described(
            text(path(&[facets::IDENTITY_KEY]), "Identity", Icon::Lock),
            "Minted by the workspace; references depend on it.",
        ),
        described(
            text(path(&[facets::CONTENT_KEY]), "Payload", Icon::Link),
            "The opaque file whose bytes are this node's body.",
        ),
        described(
            text(path(&[facets::MANIFEST_KEY]), "Manifest", Icon::Link),
            "The store listing every opaque file under the directory this node claims.",
        ),
        described(
            toggle(path(&[facets::ATTACHMENT_KEY]), "Opaque payload"),
            "Read the payload as bytes, not as a document.",
        ),
        described(
            text(
                path(&[facets::CONTENT_HASH_KEY]),
                "Content digest",
                Icon::Lock,
            ),
            "Recorded by prov's fixity pass; not typed.",
        ),
    ];

    // The stamped field, under whatever name this workspace gave it. Absent
    // when the workspace disabled the axis, which is the default — there is no
    // key to govern, and inventing `updated` would govern a field somebody else
    // owns.
    if !config.updated.is_empty() {
        rules.push(described(
            text(path(&[&config.updated]), "Last updated", Icon::Clock),
            "Stamped in RFC 3339 UTC when the content changes; prov reads it back.",
        ));
    }

    // The root's inline policy block: the config document's vocabulary, one
    // level down.
    rules.extend(nested_under(
        facets::POLICY_KEY,
        crate::config_schema::config_rules(config),
    ));
    rules
}

/// Re-root a rule set one key deeper — `spanning` becomes `prov.spanning`.
///
/// The mechanical half of "one vocabulary, two homes". Prefixing the *pattern*
/// rather than restating the rules is what keeps the two homes from drifting: a
/// term added to the config schema reaches the inline block in the same commit,
/// because it is the same list.
fn nested_under(key: &str, rules: Vec<FieldRule>) -> Vec<FieldRule> {
    rules
        .into_iter()
        .map(|mut rule| {
            let mut segments = vec![SegPat::Key(key.to_string())];
            segments.extend(rule.at.0);
            rule.at = PathPat(segments);
            rule
        })
        .collect()
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
            vec![prov::FieldSpec {
                ty: None,
                values: OpenClosed::Closed,
                vocabulary: Some("audiences.yaml".to_string()),
                reify: false,
                default: None,
                under: None,
            }],
        );
        // A type with no vocabulary — nothing to validate against, but the
        // editor still needs to know it is a date.
        config.fields.insert(
            "created".to_string(),
            vec![prov::FieldSpec {
                ty: Some(prov::FieldType::Extended(prov::ExtKind::LocalDate)),
                values: OpenClosed::default(),
                vocabulary: None,
                reify: false,
                default: None,
                under: None,
            }],
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

    /// prov's kernel keys are prov vocabulary too. Without these a
    /// schema-driven editor draws `id` as an anonymous text box beside `mood`.
    #[test]
    fn the_keys_prov_always_reads_are_governed_even_when_nothing_is_declared() {
        let schema = schema_from_config(&WorkspaceConfig::default(), &BTreeMap::new());

        for (key, icon) in [
            ("title", Icon::Text),
            ("id", Icon::Lock),
            ("content", Icon::Link),
            ("content_hash", Icon::Lock),
        ] {
            let rule = schema
                .rule_for(&[Seg::Key(key.into())])
                .unwrap_or_else(|| panic!("a rule for {key}"));
            assert_eq!(rule.present.icon.as_ref(), Some(&icon), "{key}");
            assert!(rule.present.description.is_some(), "{key} says what it is");
        }

        // The one that is more than presentation: `attachment: yes` is a marker
        // prov does not honour, and a typed rule is what stops it.
        let marker = schema
            .rule_for(&[Seg::Key("attachment".into())])
            .expect("a rule for attachment");
        assert_eq!(marker.ty, Some(FieldType::Bool));
    }

    /// The stamped field is named by the workspace, so it is governed only where
    /// the workspace named one — inventing `updated` would govern a key someone
    /// else owns.
    #[test]
    fn the_stamped_field_is_governed_under_the_name_the_workspace_gave_it() {
        let bare = schema_from_config(&WorkspaceConfig::default(), &BTreeMap::new());
        assert!(bare.rule_for(&[Seg::Key("modified".into())]).is_none());

        let config = WorkspaceConfig {
            updated: "modified".to_string(),
            ..WorkspaceConfig::default()
        };
        let schema = schema_from_config(&config, &BTreeMap::new());
        let rule = schema
            .rule_for(&[Seg::Key("modified".into())])
            .expect("the workspace's own stamp name");
        assert_eq!(rule.present.icon, Some(Icon::Clock));
    }

    /// One vocabulary, two homes: the root's inline `prov:` block is the config
    /// document's keys nested one level, so it gets the config document's rules
    /// with the pattern prefixed rather than a second copy of them.
    #[test]
    fn the_roots_inline_policy_block_is_governed_by_the_config_documents_rules() {
        let schema = schema_from_config(&WorkspaceConfig::default(), &BTreeMap::new());

        let inline = schema
            .rule_for(&[Seg::Key("prov".into()), Seg::Key("fixity".into())])
            .expect("prov.fixity");
        let (terms, closed) = inline.enum_constraint().expect("a picker, not a text box");
        assert!(closed);
        assert!(terms.iter().any(|t| t.value == "on"));

        // The same key at top level is the config *document*'s, and is not
        // governed here — a content document has no bare `fixity`.
        assert!(schema.rule_for(&[Seg::Key("fixity".into())]).is_none());
    }

    /// A workspace that declares a field of a kernel key's name wins: the
    /// kernel rules go last, and first match wins.
    #[test]
    fn a_declared_field_shadows_the_kernel_rule_of_the_same_name() {
        let mut config = WorkspaceConfig::default();
        config.fields.insert(
            "title".to_string(),
            vec![prov::FieldSpec {
                ty: None,
                values: OpenClosed::Closed,
                vocabulary: Some("titles.yaml".to_string()),
                reify: false,
                default: None,
                under: None,
            }],
        );
        let schema = schema_from_config(&config, &BTreeMap::new());
        let rule = schema
            .rule_for(&[Seg::Key("title".into())])
            .expect("a rule for title");
        assert!(
            rule.enum_constraint().is_some(),
            "the workspace's declaration, not prov's kernel rule"
        );
    }

    /// The gloss prov carries and never reads is exactly what a row should say.
    #[test]
    fn a_relations_gloss_travels_onto_its_row() {
        let schema = schema_from_config(&WorkspaceConfig::default(), &BTreeMap::new());
        let rule = schema
            .rule_for(&[Seg::Key("part_of".into())])
            .expect("a rule for part_of");
        assert_eq!(
            rule.present.description.as_deref(),
            Some("the document that contains this one")
        );
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
