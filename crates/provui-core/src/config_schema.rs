//! The **config document** → flower schema adapter — [`crate::schema`] one
//! module over.
//!
//! [`schema_from_config`](crate::schema_from_config) turns a workspace's config
//! into a schema for its *content* documents: what a controlled field accepts,
//! which keys are links. This module turns the same config into a schema for
//! **the config document itself**, so the metadata editor a frontend already
//! ships can edit a workspace's policy and its views instead of a hand-written
//! settings form.
//!
//! That is the whole point: `prov.yaml` is a document. flower can already
//! render, type-direct and validate any prov document; the only thing missing
//! was a schema saying that `fixity` is one of two words and `fields.<name>`
//! is a field declaration. With it, an "add a field" menu becomes possible,
//! `id_storage` becomes a picker instead of free text, and a typo like `fixity:
//! alll` — which prov silently ignores, keeping the default — stops being
//! reachable.
//!
//! **Every spelling here is prov's, not ours.** The term lists mirror
//! [`prov::diagnose`]'s accepted values, and the tests assert exactly that: each
//! offered term round-trips through prov's own linter, so a spelling prov
//! renames fails here rather than drifting into a picker that writes values prov
//! ignores.
//!
//! ## What this module does not govern
//!
//! prov permits a config surface to carry keys it never reads — an application
//! block (`myapp.default_view`, `myapp.publish`, …) is legal and prov neither
//! lints nor rewrites it. Nothing here governs such a block, because there is
//! nothing generic to say about it: its vocabulary is the app's.
//!
//! An app supplies its own rules by prepending them to [`config_rules`] — see
//! [`crate::rules`] for why the order is that way round, and for the builders
//! that make an overlay's rows look like the ones beside them.

use flower_core::schema::{FieldRule, Schema};
use flower_core::{Consequence, FieldType, Icon, Severity, Term, Tint};
use prov::{FIELD_TYPES, WorkspaceConfig};

use crate::rules::{
    choice, choice_terms, costly, costly_when, open_choice, open_choice_terms, path, present, term,
    text, toggle,
};

/// The config-document keys the editor should show but refuse to edit: `spec` is
/// prov's config-format version, not a preference — the workspace stamps it, and
/// hand-editing it claims conformance to a format the file may not have.
pub const CONFIG_READONLY_KEYS: &[&str] = &["spec"];

/// The grains a view may group (or nest) by — the config spellings of
/// [`prov::views::Grain`].
///
/// Written out rather than read off the enum because prov exports the type but
/// not its spelling table, and a picker needs a gloss per entry regardless. The
/// [`every_offered_term_is_one_prov_accepts`] test holds this list to prov's own
/// parser, so a grain prov renames fails the build here.
///
/// The bare words only. prov also takes `{ initial: n }` for a wider
/// alphabetical cut, which a dropdown has nowhere to put a number for; a
/// workspace that wants one writes it by hand and the editor leaves it alone.
///
/// [`every_offered_term_is_one_prov_accepts`]: self
pub const VIEW_GRAINS: &[(&str, &str)] = &[
    ("year", "One group per year"),
    ("month", "One group per month"),
    ("day", "One group per day"),
    ("initial", "One group per first letter"),
];

/// Build a flower [`Schema`] for a workspace's config document.
///
/// `config` is the workspace's *resolved* config; it is read for the one thing
/// that can only be written against a particular workspace — which field names
/// a view may group by.
pub fn config_schema(config: &WorkspaceConfig) -> Schema {
    Schema::new(config_rules(config))
}

/// [`config_schema`]'s rules, before they become a schema — the composition
/// point for an application overlay.
///
/// A [`Schema`] resolves a path by first match wins, so an app prepends its own
/// rules to these rather than appending them. See [`crate::rules`].
pub fn config_rules(config: &WorkspaceConfig) -> Vec<FieldRule> {
    let mut rules = vec![
        text(path(&["title"]), "Title", Icon::Text),
        FieldRule::new(path(&["spec"]))
            .ty(FieldType::Int)
            .present(present("Config format version", Icon::Lock)),
        choice(
            path(&["content_format"]),
            "Content format",
            Icon::Enum,
            &[
                ("markdown", "Markdown documents"),
                ("djot", "Djot documents"),
                ("html", "HTML documents"),
            ],
        ),
        // ── metadata ────────────────────────────────────────────────────────
        // Open rather than closed: prov compiles `json`/`toml`/`fig` behind
        // cargo features, so a build that lacks one would reject a spelling that
        // is perfectly legal elsewhere. Offering without rejecting is the honest
        // shape for a vocabulary this crate cannot fully see.
        costly(
            open_choice(
                path(&["metadata", "format"]),
                "Metadata format",
                Icon::Enum,
                &[
                    ("yaml", "YAML frontmatter"),
                    ("json", "JSON metadata"),
                    ("toml", "TOML metadata"),
                    ("fig", "fig metadata"),
                ],
            ),
            Tint::Warning,
            "Rewrites the metadata of every document in the workspace.",
        ),
        costly(
            choice(
                path(&["metadata", "embed"]),
                "How metadata is embedded",
                Icon::Enum,
                &[
                    ("delimited", "Fenced frontmatter (`---`)"),
                    ("code_block", "A fenced code block"),
                    ("html_script", "A <script> tag"),
                    ("html_code", "An HTML <code> block"),
                    ("separate", "A sidecar file beside the document"),
                ],
            ),
            Tint::Warning,
            "Rewrites the metadata of every document in the workspace.",
        ),
    ];

    // ── references, and the per-relation overrides that mirror them ─────────
    for prefix in [&["references"][..], &["relations", "*"][..]] {
        let at = |leaf: &str| {
            let mut segs = prefix.to_vec();
            segs.push(leaf);
            path(&segs)
        };
        rules.push(costly(
            choice(
                at("notation"),
                "Link notation",
                Icon::Link,
                &[
                    ("markdown", "[Title](/path.md)"),
                    ("wikilink", "[[path]]"),
                    ("bare", "A bare path, unwrapped"),
                ],
            ),
            Tint::Warning,
            "Rewrites every link in the workspace.",
        ));
        rules.push(costly(
            choice(
                at("path_style"),
                "Link paths",
                Icon::Link,
                // `canonical` was retired upstream: it rendered a workspace-relative
                // path *without* the leading slash, which is the one spelling that
                // does not resolve from anywhere. prov now accepts `root | relative`
                // only and migrates a workspace set to `canonical` onto `root`, which
                // renders the same path plus the slash that makes the reading
                // explicit. Offering it here would have kept writing a setting prov
                // rejects.
                &[
                    ("root", "From the workspace root (/notes/a.md)"),
                    ("relative", "Relative to the linking document"),
                ],
            ),
            Tint::Warning,
            "Rewrites every link in the workspace.",
        ));
        rules.push(costly(
            choice(
                at("target"),
                "What a link addresses",
                Icon::Link,
                &[
                    ("path", "The target's path — renames rewrite links"),
                    ("id", "The target's id — renames rewrite nothing"),
                    ("alias", "A human alias"),
                ],
            ),
            Tint::Warning,
            "Rewrites every link in the workspace.",
        ));
        rules.push(toggle(at("label"), "Carry the target's title as a label"));
    }
    rules.push(costly(
        text(path(&["spanning"]), "The containment relation", Icon::Link),
        Tint::Warning,
        "The relation the whole workspace is organised by. Changing it rebuilds the tree.",
    ));
    // What this workspace calls itself: the qualifier in `id:<workspace>/<id>`
    // for references *into* it from another workspace. An app that mints
    // permalinks will also read it as the namespace it hands out under, which is
    // why it is drawn as an identity (`Lock`) rather than a preference — editing
    // it silently re-points every reference already written.
    rules.push(text(
        path(&["workspace_id"]),
        "This workspace's id",
        Icon::Lock,
    ));
    rules.push(choice(
        path(&["relations", "*", "cardinality"]),
        "How many targets",
        Icon::Enum,
        &[("one", "At most one"), ("many", "A list")],
    ));
    rules.push(text(
        path(&["relations", "*", "inverse"]),
        "The relation pointing back",
        Icon::Link,
    ));
    rules.push(text(
        path(&["relations", "*", "means"]),
        "What this relation means",
        Icon::Text,
    ));

    // ── fields: one entry per declared field ────────────────────────────────
    rules.push(choice_terms(
        path(&["fields", "*", "type"]),
        "Value type",
        Icon::Enum,
        FIELD_TYPES
            .iter()
            .map(|t| term(t, field_type_gloss(t)))
            .collect(),
    ));
    rules.push(choice(
        path(&["fields", "*", "values"]),
        "Which values are legal",
        Icon::Enum,
        &[
            ("open", "Anything — the field is free text"),
            ("closed", "Only terms the vocabulary lists"),
        ],
    ));
    // A pointer to a vocabulary document. Typed as text with a link glyph rather
    // than as a `Ref`: prov resolves it as a config *value*, not through a
    // relation, so flower's Reference constraint (which names a relation) would
    // describe it wrongly.
    rules.push(text(
        path(&["fields", "*", "vocabulary"]),
        "Vocabulary document",
        Icon::Link,
    ));
    rules.push(costly_when(
        toggle(
            path(&["fields", "*", "reify"]),
            "Give each value its own document",
        ),
        true,
        Severity::Confirm,
        "Creates a document for every distinct value of this field across the workspace.",
    ));

    // ── policy axes ─────────────────────────────────────────────────────────
    let id_storage = choice(
        path(&["id_storage"]),
        "Where ids live",
        Icon::Lock,
        &[
            ("registry", "The registry only"),
            ("frontmatter", "Each document only"),
            ("both", "Both — the registry stays rebuildable"),
        ],
    );
    // Declared on each single store rather than as a transition off `both`:
    // `when` names a destination, and a host that knows the current value
    // suppresses the notice when nothing changed. Landing on `both` costs
    // nothing, so it carries none.
    rules.push(
        id_storage
            .on_change(Consequence::when(
                "registry",
                "Ids leave the documents. The registry becomes the only copy.",
            ))
            .on_change(Consequence::when(
                "frontmatter",
                "Ids leave the registry, so it can no longer be rebuilt from itself.",
            )),
    );
    rules.push(text(
        path(&["updated"]),
        "Field stamped on save (empty turns it off)",
        Icon::Clock,
    ));
    rules.push(costly_when(
        choice(
            path(&["identity"]),
            "When a document earns an id",
            Icon::Lock,
            &[
                ("none", "Never"),
                ("lazy", "When something first needs one"),
                ("eager", "At creation"),
            ],
        ),
        "none",
        Severity::Confirm,
        "New documents stop earning ids, so nothing can link to them by id.",
    ));
    rules.push(choice(
        path(&["fixity"]),
        "Content checksums",
        Icon::Lock,
        // Two answers, not three. It was a coverage tier (`attachments`/`all`)
        // until prov 0.11; what a checksum covers is now read off each
        // document's shape — an attachment's payload and a separated body get
        // one, a combined body never does — so the only question left is
        // whether to write them.
        &[("off", "No checksums"), ("on", "Checksums on")],
    ));
    rules.push(costly_when(
        toggle(path(&["record_deletions"]), "Record deletions"),
        false,
        Severity::ConfirmExplicitly,
        "Deleting stops being undoable. The file goes either way — this is what \
         writes down that it existed, and without a record nothing can put a \
         deleted page back, even where a snapshot still holds it.",
    ));
    // A picker rather than a toggle, for `about` and the pickers above alike:
    // they spell their states as words in the config, and prov may add a third
    // without the surface having to change kind. A toggle would also have to
    // invent which word means "on".
    rules.push(choice(
        path(&["about"]),
        "Generated “how to read this” page",
        Icon::Text,
        &[
            ("off", "Don't generate one"),
            ("structure", "Describe this workspace's structure"),
        ],
    ));

    // ── views: the lenses a workspace declares for itself ───────────────────
    //
    // Top-level and prov's own format, so every tool over the workspace reads
    // the same views.
    //
    // The three clauses below are deliberately three, not one. `group`/`by` say
    // how records become groups (MoReq2010's *classification*); `under` says
    // which records the view covers at all (*aggregation*); `nest` says how a
    // new record is filed. Collapsing them — deriving the folder shape from the
    // grouping grain — is the arrangement MoReq2010 §1.4.5 permits and warns
    // about, because it makes a reading preference silently relocate files.
    rules.push(text(
        path(&["views", "*", "label"]),
        "View name",
        Icon::Text,
    ));
    rules.push(text(
        path(&["views", "*", "icon"]),
        "View glyph",
        Icon::Text,
    ));
    // Offered, not enforced, for two independent reasons. A view may
    // legitimately name a field the workspace has not declared yet (the
    // declaration usually follows the first document that carries it), and
    // rejecting that would make the two settings orderable only one way. And
    // prov itself imposes no vocabulary here — `group:` takes any field key —
    // so a closed list would be this crate inventing a rule prov does not have.
    //
    // Every declared field is offered, not some filtered subset: which fields
    // are *worth* grouping by is a judgement about a particular UI, and an app
    // that has one shadows this rule with its own (see `crate::rules`).
    //
    // Governed twice because `group:` takes both shapes prov writes: a bare
    // string for a one-field view, a list for a chain (`[date_of_document,
    // created, updated]`). A rule for only the scalar would leave every chained
    // view's rows untyped.
    rules.push(open_choice_terms(
        path(&["views", "*", "group"]),
        "Groups by",
        Icon::Enum,
        group_terms(config),
    ));
    rules.push(open_choice_terms(
        path(&["views", "*", "group", "[]"]),
        "Groups by",
        Icon::Enum,
        group_terms(config),
    ));
    rules.push(choice(
        path(&["views", "*", "by"]),
        "Grain",
        Icon::Clock,
        VIEW_GRAINS,
    ));
    // A link to the index this view's records hang under. Text with a link glyph
    // for the same reason `fields.*.vocabulary` is: it is resolved as a config
    // *value*, not through a relation, so flower's `Reference` constraint (which
    // names a relation) would describe it wrongly.
    rules.push(text(
        path(&["views", "*", "under"]),
        "Filed under (empty covers the whole workspace)",
        Icon::Link,
    ));
    rules.push(choice(
        path(&["views", "*", "nest"]),
        "New entries nest by (empty files them flat)",
        Icon::Link,
        VIEW_GRAINS,
    ));
    // `where:` — the conditions a document in scope must also meet. Only the two
    // *predicates* are governed: `has` names a field, and `equals` is a mapping
    // of field to value, so both have a leaf a picker can fill. The combinators
    // (`not`, `any-of`, `all-of`) nest conditions inside conditions to arbitrary
    // depth, which is a tree the row editor has no shape for — those stay
    // untyped text, which reads them back unchanged rather than rewriting them
    // into something else.
    rules.push(open_choice_terms(
        path(&["views", "*", "where", "has"]),
        "Only documents that have",
        Icon::Enum,
        group_terms(config),
    ));
    rules.push(open_choice_terms(
        path(&["views", "*", "where", "has", "[]"]),
        "Only documents that have",
        Icon::Enum,
        group_terms(config),
    ));
    rules.push(text(
        path(&["views", "*", "where", "equals", "*"]),
        "…and whose value is",
        Icon::Text,
    ));

    rules
}

/// What a view's `group:` may name: any field this workspace declares.
///
/// Offered as a convenience, never as a restriction — prov accepts any field key
/// here, including one not yet declared. See the `views.*.group` rule.
fn group_terms(config: &WorkspaceConfig) -> Vec<Term> {
    config
        .fields
        .keys()
        .map(|name| Term::value(name.clone()).description(format!("The document's {name}")))
        .collect()
}

/// A one-line gloss for each `fields.<name>.type` spelling prov accepts.
fn field_type_gloss(ty: &str) -> &'static str {
    match ty {
        "str" => "Text",
        "bool" => "True or false",
        "int" => "A whole number",
        "float" => "A number",
        "date" => "A calendar day",
        "datetime" => "An instant with a time zone",
        "local-datetime" => "A date and time, no zone",
        "time" => "A time of day",
        "ref" => "A link to another document",
        "map" => "A block of keys",
        "seq" => "A list",
        _ => "",
    }
}

#[cfg(test)]
mod costly_tests {
    use super::*;
    use fig::Value;
    use flower_core::{FieldRuleExt, Seg};

    fn schema() -> Schema {
        config_schema(&WorkspaceConfig::default())
    }

    fn warned(path: &[Seg]) -> Option<(String, String)> {
        let schema = schema();
        let rule = schema.rule_for(path)?;
        let tint = rule.present.tint?;
        Some((format!("{tint:?}"), rule.present.description.clone()?))
    }

    fn key(k: &str) -> Seg {
        Seg::Key(k.into())
    }

    /// A field with no safe answer says so, and says what the cost is.
    #[test]
    fn a_field_that_rewrites_the_workspace_carries_the_warning_and_the_sentence() {
        let schema = schema();
        for path in [
            vec![key("metadata"), key("format")],
            vec![key("metadata"), key("embed")],
            vec![key("references"), key("notation")],
            vec![key("references"), key("path_style")],
            vec![key("references"), key("target")],
            vec![key("spanning")],
        ] {
            let (tint, why) = warned(&path).unwrap_or_else(|| panic!("{path:?} should warn"));
            assert_eq!(tint, "Warning", "{path:?}");
            assert!(!why.is_empty(), "{path:?} warns without saying why");
            // The tint draws the row; the consequence is what Apply reads. A
            // field carrying one and not the other would either look dangerous
            // and apply silently, or apply loudly and look ordinary.
            let rule = schema.rule_for(&path).expect("rule");
            assert!(
                rule.severity_of(&Value::Str("anything".into())).is_some(),
                "{path:?} is tinted but declares no consequence"
            );
        }
    }

    /// A field costly in one direction only warns on that direction and stays
    /// quiet on the other.
    ///
    /// This is the assertion the whole per-value design exists for. Before
    /// `Consequence` the only vocabulary was a tint on the row, which would have
    /// warned on the harmless answer too — and a warning that fires either way
    /// is how a reader learns to click through warnings.
    #[test]
    fn a_field_costly_in_one_direction_warns_on_that_direction_only() {
        let schema = schema();
        let cases: &[(&str, Value, Value)] = &[
            // (field, the costly answer, a safe one)
            ("record_deletions", Value::Bool(false), Value::Bool(true)),
            (
                "identity",
                Value::Str("none".into()),
                Value::Str("eager".into()),
            ),
        ];
        for (field, costly, safe) in cases {
            let rule = schema
                .rule_for(&[key(field)])
                .unwrap_or_else(|| panic!("no rule for {field}"));
            assert!(
                rule.severity_of(costly).is_some(),
                "{field} should warn on {costly:?}"
            );
            assert!(
                rule.severity_of(safe).is_none(),
                "{field} warns on {safe:?}, which costs nothing"
            );
        }
    }

    /// Turning off the deletion record is the one change nothing can undo
    /// afterwards, and the only one asking for the strongest gesture.
    #[test]
    fn only_the_unrecoverable_change_asks_for_a_deliberate_yes() {
        let schema = schema();
        let strongest = |field: &str, value: Value| {
            schema
                .rule_for(&[key(field)])
                .and_then(|r| r.severity_of(&value))
        };
        assert_eq!(
            strongest("record_deletions", Value::Bool(false)),
            Some(Severity::ConfirmExplicitly)
        );
        assert_eq!(
            strongest("identity", Value::Str("none".into())),
            Some(Severity::Confirm)
        );
    }

    /// `id_storage` is the transition case: narrowing off `both` costs
    /// something, but `when` names a destination and the crate has no memory of
    /// what the field held. So both single stores declare the cost and the host
    /// suppresses it when the value has not changed — asking twice about the
    /// same value answers the same way, by design.
    #[test]
    fn a_narrowing_declares_its_destinations_and_leaves_the_no_op_to_the_host() {
        let schema = schema();
        let rule = schema.rule_for(&[key("id_storage")]).expect("rule");
        for narrow in ["registry", "frontmatter"] {
            assert!(
                rule.severity_of(&Value::Str(narrow.into())).is_some(),
                "narrowing to {narrow} should say what is lost"
            );
        }
        assert!(
            rule.severity_of(&Value::Str("both".into())).is_none(),
            "widening to `both` loses nothing"
        );
    }

    /// Every guard names a term the vocabulary actually has.
    ///
    /// The one failure with no other detector: a guard written `"of"` for
    /// `"off"` never fires, and at commit time that is indistinguishable from a
    /// value nobody chose. Checked here, where a typo fails the build.
    #[test]
    fn no_guard_names_a_value_the_vocabulary_does_not_have() {
        let schema = schema();
        for field in ["identity", "id_storage", "about", "fixity"] {
            let rule = schema.rule_for(&[key(field)]).expect("rule");
            let terms = match rule.enum_constraint() {
                Some((terms, _)) => terms.to_vec(),
                None => continue,
            };
            let orphans = flower_core::guards_without_terms(&rule.on_change, &terms);
            assert!(
                orphans.is_empty(),
                "{field} guards values its vocabulary does not offer: {orphans:?}"
            );
        }
    }

    /// An ordinary field is untouched by any of this.
    #[test]
    fn an_ordinary_field_carries_no_warning() {
        assert!(warned(&[key("title")]).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flower_core::{FieldRuleExt, PathPat, Seg, SegPat};
    use prov::config::{FieldSpec, OpenClosed};
    use prov::meta::{Mapping, Value};
    use prov::{ConfigIssueKind, FieldType as ProvFieldType};

    fn config_with(fields: &[(&str, Option<ProvFieldType>)]) -> WorkspaceConfig {
        let mut config = WorkspaceConfig::default();
        for (name, ty) in fields {
            config.fields.insert(
                (*name).to_string(),
                FieldSpec {
                    ty: *ty,
                    values: OpenClosed::default(),
                    vocabulary: None,
                    reify: false,
                },
            );
        }
        config
    }

    /// A concrete path for a rule's pattern, with a stand-in key wherever the
    /// pattern says "any key" (`fields.*.type` → `fields.x.type`).
    fn concrete(rule: &FieldRule) -> Option<Vec<String>> {
        rule.at
            .0
            .iter()
            .map(|seg| match seg {
                SegPat::Key(k) => Some(k.clone()),
                SegPat::AnyKey => Some("x".to_string()),
                _ => None,
            })
            .collect()
    }

    /// Nest `value` under `path`, innermost last — the config surface prov's
    /// linter reads.
    fn nested(path: &[String], value: &str) -> Value {
        let mut current = Value::String(value.to_string());
        for key in path.iter().rev() {
            let mut map = Mapping::new();
            map.insert(key.clone(), current);
            current = Value::Mapping(map);
        }
        current
    }

    /// The load-bearing test: **every term this schema offers is a term prov
    /// accepts.** A picker whose values prov silently ignores is worse than no
    /// picker — the setting would look applied and do nothing — so the spellings
    /// are checked against prov's own linter rather than against a copy of them.
    ///
    /// This is also what holds [`VIEW_GRAINS`] to `prov::views::Grain`: the
    /// grains are a closed picker, so a grain prov renames fails right here.
    #[test]
    fn every_offered_term_is_one_prov_accepts() {
        let schema = config_schema(&config_with(&[]));
        let mut checked = 0;
        for rule in schema.rules() {
            let Some((terms, closed)) = rule.enum_constraint() else {
                continue;
            };
            if !closed {
                continue; // offered, not enforced — see `metadata.format`
            }
            let Some(path) = concrete(rule) else { continue };
            let dotted = path.join(".");
            for term in terms {
                let issues = prov::diagnose(&nested(&path, &term.value));
                // Only findings *about this key*. `nested` builds the smallest
                // surface that places the term, which for a view is one `by:`
                // with no `group:` beside it — so prov also reports the missing
                // sibling, correctly, and about a key this test is not asking
                // after. Judging the term by that would fail every entry inside
                // a container key that has a required member.
                let bad: Vec<_> = issues
                    .iter()
                    .filter(|i| i.key == dotted)
                    .filter(|i| matches!(i.kind, ConfigIssueKind::InvalidValue { .. }))
                    .collect();
                assert!(
                    bad.is_empty(),
                    "schema offers `{dotted}: {}`, which prov rejects: {bad:?}",
                    term.value
                );
                checked += 1;
            }
        }
        assert!(
            checked > 20,
            "expected to have checked real terms, got {checked}"
        );
    }

    /// The type list is prov's own const, not a copy — so a type prov adds shows
    /// up in the picker without anyone remembering to add it here.
    #[test]
    fn field_types_come_from_prov() {
        let schema = config_schema(&config_with(&[]));
        let rule = schema
            .rule_for(&[
                Seg::Key("fields".into()),
                Seg::Key("people".into()),
                Seg::Key("type".into()),
            ])
            .expect("fields.<name>.type should be governed");
        let (terms, closed) = rule.enum_constraint().expect("a closed type picker");
        assert!(closed);
        let offered: Vec<&str> = terms.iter().map(|t| t.value.as_str()).collect();
        assert_eq!(offered, FIELD_TYPES);
    }

    /// Coverage in the other direction: **every value prov itself writes into a
    /// config document is governed by a rule.** Without this, an axis prov adds
    /// later would render as untyped free text and nobody would notice until a
    /// user typed a value that silently did nothing.
    #[test]
    fn every_key_prov_writes_is_governed() {
        let mut config = config_with(&[("audience", Some(ProvFieldType::Str))]);
        config.fields.get_mut("audience").unwrap().vocabulary = Some("audiences.yaml".into());
        let schema = config_schema(&config);

        fn walk(schema: &Schema, path: &mut Vec<Seg>, value: &Value, ungoverned: &mut Vec<String>) {
            match value {
                Value::Mapping(map) => {
                    for (key, child) in map {
                        path.push(Seg::Key(key.clone()));
                        walk(schema, path, child, ungoverned);
                        path.pop();
                    }
                }
                // Only leaves need a rule; a container's rows take their shape
                // from their children.
                _ => {
                    if schema.rule_for(path).is_none() {
                        ungoverned.push(
                            path.iter()
                                .map(|s| match s {
                                    Seg::Key(k) => k.clone(),
                                    other => format!("{other:?}"),
                                })
                                .collect::<Vec<_>>()
                                .join("."),
                        );
                    }
                }
            }
        }

        let mut ungoverned = Vec::new();
        walk(
            &schema,
            &mut Vec::new(),
            &Value::Mapping(config.to_mapping()),
            &mut ungoverned,
        );
        assert!(
            ungoverned.is_empty(),
            "prov writes these keys and the schema does not govern them: {ungoverned:?}"
        );
    }

    /// Policy axes are typed, so the editor renders a toggle rather than free
    /// text — the thing that made `fixity: alll` possible.
    #[test]
    fn policy_axes_are_typed() {
        let schema = config_schema(&config_with(&[]));
        let deletions = schema
            .rule_for(&[Seg::Key("record_deletions".into())])
            .expect("record_deletions should be governed");
        assert_eq!(deletions.ty, Some(FieldType::Bool));

        let fixity = schema
            .rule_for(&[Seg::Key("fixity".into())])
            .expect("fixity should be governed");
        let (terms, closed) = fixity.enum_constraint().expect("a closed picker");
        assert!(closed, "an unparseable fixity silently keeps the default");
        assert_eq!(
            terms.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
            ["off", "on"],
            "two answers since prov 0.11 — coverage follows the document's shape"
        );
    }

    /// The workspace's own id is a top-level key, not something filed under an
    /// application's block: it answers "what does this workspace call itself"
    /// for prov's cross-workspace references, and any app that also uses it for
    /// permalinks is reading the same answer.
    #[test]
    fn the_workspace_id_is_governed_at_the_top_level() {
        let schema = config_schema(&config_with(&[]));
        let id = schema
            .rule_for(&[Seg::Key("workspace_id".into())])
            .expect("the workspace's id should be governed");
        assert_eq!(id.present.title.as_deref(), Some("This workspace's id"));
        assert_eq!(id.present.icon, Some(Icon::Lock));
    }

    /// A relation's per-entry overrides are governed the same way the global
    /// `references` block is — same leaves, same pickers.
    #[test]
    fn a_relation_entry_is_governed_like_the_references_block() {
        let schema = config_schema(&config_with(&[]));
        for path in [
            vec![Seg::Key("references".into()), Seg::Key("target".into())],
            vec![
                Seg::Key("relations".into()),
                Seg::Key("contents".into()),
                Seg::Key("target".into()),
            ],
        ] {
            let rule = schema.rule_for(&path).expect("target should be governed");
            let (terms, _) = rule.enum_constraint().expect("a picker");
            assert_eq!(
                terms.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
                ["path", "id", "alias"]
            );
        }
    }

    /// The view block is governed through the wildcard, and `group:` is offered
    /// rather than enforced — prov accepts any field key there, including one
    /// the workspace has not declared yet.
    #[test]
    fn a_view_entry_is_governed_and_its_grouping_stays_open() {
        let schema = config_schema(&config_with(&[("people", Some(ProvFieldType::Str))]));
        let view = |leaf: &str| {
            vec![
                Seg::Key("views".into()),
                Seg::Key("daily".into()),
                Seg::Key(leaf.into()),
            ]
        };

        let by = schema.rule_for(&view("by")).expect("views.*.by");
        let (grains, closed) = by.enum_constraint().expect("a grain picker");
        assert!(closed, "a grain prov cannot parse is not a grain");
        assert_eq!(
            grains.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
            VIEW_GRAINS.iter().map(|(v, _)| *v).collect::<Vec<_>>()
        );

        let group = schema.rule_for(&view("group")).expect("views.*.group");
        let (fields, closed) = group.enum_constraint().expect("a field picker");
        assert!(!closed, "prov imposes no vocabulary on `group:`");
        assert_eq!(
            fields.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
            ["people"]
        );

        // The chained form (`group: [a, b]`) is governed too, or every chained
        // view's rows would come out untyped.
        assert!(
            schema
                .rule_for(&[
                    Seg::Key("views".into()),
                    Seg::Key("daily".into()),
                    Seg::Key("group".into()),
                    Seg::Index(0),
                ])
                .is_some(),
            "the list form of `group:` should be governed"
        );
        assert!(schema.rule_for(&view("label")).is_some());
        assert!(schema.rule_for(&view("under")).is_some());
        assert!(schema.rule_for(&view("nest")).is_some());
    }

    /// The composition an application overlay depends on: a rule *prepended* to
    /// [`config_rules`] shadows the generic one, because a schema resolves a
    /// path by first match wins.
    ///
    /// Asserted here rather than left to a doc comment, because the failure mode
    /// is silent — an overlay appended instead of prepended never fires, and the
    /// generic rule answers in its place with no error anywhere.
    #[test]
    fn a_prepended_rule_shadows_the_generic_one() {
        let config = config_with(&[("people", Some(ProvFieldType::Str))]);
        let group = vec![
            Seg::Key("views".into()),
            Seg::Key("daily".into()),
            Seg::Key("group".into()),
        ];

        let mut overlaid = vec![open_choice_terms(
            PathPat(vec![
                SegPat::Key("views".into()),
                SegPat::AnyKey,
                SegPat::Key("group".into()),
            ]),
            "Groups by",
            Icon::Enum,
            vec![term("date", "The document's date")],
        )];
        overlaid.extend(config_rules(&config));
        let schema = Schema::new(overlaid);

        let (terms, _) = schema
            .rule_for(&group)
            .and_then(|r| r.enum_constraint())
            .expect("the overlay rule");
        assert_eq!(
            terms.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
            ["date"],
            "the prepended rule should win"
        );

        // And an app block prov never reads is simply ungoverned here — there is
        // nothing generic to say about it.
        assert!(
            config_schema(&config)
                .rule_for(&[Seg::Key("myapp".into()), Seg::Key("default_view".into())])
                .is_none()
        );
    }
}
