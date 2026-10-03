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

/// The ways a filing entry may nest new records — the config spellings of
/// [`prov::filing::Nest`], one per entry in [`prov::filing::NESTS`].
///
/// Written out rather than taken from `NESTS` alone because a picker needs a
/// gloss per entry, which prov does not carry. The
/// [`every_offered_term_is_one_prov_accepts`] test holds this list to prov's own
/// parser, and [`the_nests_are_provs`] to its list, so a spelling prov renames
/// or adds fails the build here.
///
/// The bare words only. prov also takes `{ initial: n }` for a wider
/// alphabetical cut, which a dropdown has nowhere to put a number for; a
/// workspace that wants one writes it by hand and the editor leaves it alone.
///
/// Views no longer take a grain as a key of their own: a view that groups by
/// year says so in its `key:` expression (`year(created)`), which is text.
///
/// [`every_offered_term_is_one_prov_accepts`]: self
/// [`the_nests_are_provs`]: self
pub const FILING_NESTS: &[(&str, &str)] = &[
    ("year", "Under an index per year"),
    ("month", "Under an index per month, inside its year"),
    ("day", "Under an index per day, inside its month"),
    ("initial", "Under an index per first letter"),
    ("ref", "Under the document the field links to"),
];

/// Build a flower [`Schema`] for a workspace's config document.
///
/// `config` is the workspace's *resolved* config; it is read for the one thing
/// that can only be written against a particular workspace — which field names
/// a filing entry may file by.
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
    // A field is declared once, as a mapping, or several times, as a list of
    // mappings each `under:` an index (prov 0.12) — `status` as one set of
    // terms under `Tasks` and another under `Proposals`. The keys are the same
    // in both spellings, so each rule is stated at both depths.
    for form in [&["fields", "*"][..], &["fields", "*", "[]"][..]] {
        let at = |key: &str| path(&[form, &[key]].concat());
        rules.push(choice_terms(
            at("type"),
            "Value type",
            Icon::Enum,
            FIELD_TYPES
                .iter()
                .map(|t| term(t, field_type_gloss(t)))
                .collect(),
        ));
        rules.push(choice(
            at("values"),
            "Which values are legal",
            Icon::Enum,
            &[
                ("open", "Anything — the field is free text"),
                ("closed", "Only terms the vocabulary lists"),
            ],
        ));
        // A pointer to a vocabulary document. Typed as text with a link glyph
        // rather than as a `Ref`: prov resolves it as a config *value*, not
        // through a relation, so flower's Reference constraint (which names a
        // relation) would describe it wrongly.
        rules.push(text(at("vocabulary"), "Vocabulary document", Icon::Link));
        // Whether the vocabulary is a flat `terms:` store or an index of term
        // documents is not a key here: prov reads it off the store the pointer
        // names (a `vocabulary:` marker means flat), so there is no toggle that
        // could disagree with it.
        //
        // When prov writes the current time into this field. Only the unscoped
        // declaration's is read, and only one field may claim each; offered in
        // both spellings anyway, because a list may carry the unscoped
        // fallback, and prov's `check` names the scoped or repeated one.
        rules.push(choice(
            at("stamp"),
            "Stamped by prov",
            Icon::Clock,
            &[
                ("edit", "When the content changes"),
                ("create", "When the document is made"),
            ],
        ));
        // The index this declaration governs the documents under, resolved as a
        // filing entry's `under` is — a path, an `id:`, or a title.
        rules.push(text(
            at("under"),
            "Governs documents under this index (empty covers the whole workspace)",
            Icon::Link,
        ));
        // A starting value, written by `new` and never read back. Free text:
        // prov takes any value here, and its type is the field's to say.
        rules.push(text(
            at("default"),
            "What a new document starts with",
            Icon::Text,
        ));
    }

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
    // Read from the workspace node only — the one policy home reachable before
    // the root is known. Drawn as a link, not a `Ref`, for the reason
    // `fields.*.vocabulary` is.
    rules.push(text(
        path(&["root"]),
        "The root document (empty lets prov find it)",
        Icon::Link,
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
    rules.push(choice(
        path(&["confirmations"]),
        "Confirmations are measured against",
        Icon::Lock,
        // What decides whether a confirmation still stands (prov 0.19). Under
        // `content` each entry names the content digest it confirmed, which a
        // tool can also check against the document's history; under `stamp`
        // an edit stamp later than the entry unseats it.
        &[
            ("stamp", "The edit stamp"),
            ("content", "The content confirmed"),
        ],
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
    // Views and filing are two axes, not one. A view reads: `where` says which
    // records it covers and `key` how they become groups (MoReq2010's
    // *classification*). A filing entry writes: which index a new record hangs
    // under (*aggregation*). Collapsing them — deriving the folder shape from
    // the grouping grain — is the arrangement MoReq2010 §1.4.5 permits and
    // warns about, because it makes a reading preference silently relocate
    // files. prov used to spell both inside one view (`under`, `nest`); it now
    // keeps them apart, and so does this schema.
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
    // `where` and `key` are CEL expressions — `status in ['open', 'blocked']`,
    // `year(created)` — over the document's fields and `doc` itself. Text, not a
    // picker: an expression is a sentence, and the only vocabulary a row could
    // offer (a field name) is the smallest part of one. prov parses both when
    // the config is checked, so a typo is its `bad_expression` finding rather
    // than a view that silently shows nothing. The description names prov's own
    // functions, read off prov, so a reader knows `under('Tasks')` is there
    // without leaving the editor.
    let functions = prov::views::FUNCTIONS.join(", ");
    rules.push(described(
        text(
            path(&["views", "*", "where"]),
            "Only documents where (empty covers every document)",
            Icon::Text,
        ),
        &format!(
            "A CEL condition over the document's fields and `doc`. prov's functions: {functions}."
        ),
    ));
    rules.push(described(
        text(path(&["views", "*", "key"]), "Groups by", Icon::Enum),
        &format!("A CEL expression naming each document's group. prov's functions: {functions}."),
    ));

    // ── filing: where a new record goes ─────────────────────────────────────
    rules.push(text(
        path(&["filing", "*", "label"]),
        "Filing name",
        Icon::Text,
    ));
    // A link to the index new records go below. Text with a link glyph for the
    // same reason `fields.*.vocabulary` is: it is resolved as a config *value*,
    // not through a relation, so flower's `Reference` constraint (which names a
    // relation) would describe it wrongly.
    rules.push(text(
        path(&["filing", "*", "under"]),
        "Files under (empty files below the root)",
        Icon::Link,
    ));
    // Offered, not enforced, for two independent reasons. An entry may
    // legitimately name a field the workspace has not declared yet (the
    // declaration usually follows the first document that carries it), and
    // rejecting that would make the two settings orderable only one way. And
    // prov itself imposes no vocabulary here — `field:` takes any field path —
    // so a closed list would be this crate inventing a rule prov does not have.
    //
    // Every declared field is offered, not some filtered subset: which fields
    // are *worth* filing by is a judgement about a particular UI, and an app
    // that has one shadows this rule with its own (see `crate::rules`).
    //
    // Governed twice because `field:` takes both shapes prov writes: a bare
    // string for one field, a list tried in order (`[date_of_document,
    // created]`). A rule for only the scalar would leave every listed entry's
    // rows untyped.
    rules.push(open_choice_terms(
        path(&["filing", "*", "field"]),
        "Files by",
        Icon::Enum,
        field_terms(config),
    ));
    rules.push(open_choice_terms(
        path(&["filing", "*", "field", "[]"]),
        "Files by",
        Icon::Enum,
        field_terms(config),
    ));
    rules.push(choice(
        path(&["filing", "*", "nest"]),
        "Nests new entries (empty files them flat)",
        Icon::Link,
        FILING_NESTS,
    ));

    rules
}

/// A rule with the one sentence a reader needs to use it.
fn described(mut rule: FieldRule, why: &str) -> FieldRule {
    rule.present = std::mem::take(&mut rule.present).description(why);
    rule
}

/// What a filing entry's `field:` may name: any field this workspace declares.
///
/// Offered as a convenience, never as a restriction — prov accepts any field
/// path here, including one not yet declared. See the `filing.*.field` rule.
fn field_terms(config: &WorkspaceConfig) -> Vec<Term> {
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

    /// What a confirmation is measured against is a picker over prov's two
    /// answers, not free text.
    #[test]
    fn confirmations_are_a_choice_of_stamp_or_content() {
        let schema = schema();
        let rule = schema
            .rule_for(&[key("confirmations")])
            .expect("confirmations should be governed");
        let (terms, _) = rule.enum_constraint().expect("a choice");
        let values: Vec<&str> = terms.iter().map(|t| t.value.as_str()).collect();
        assert_eq!(values, ["stamp", "content"]);
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
    use prov::filing::{FilingSpec, Nest};
    use prov::grain::Grain;
    use prov::meta::{Mapping, Value};
    use prov::views::{Expression, RETIRED_VIEW_KEYS};
    use prov::{ConfigIssueKind, FieldType as ProvFieldType, Stamp, ViewSpec};

    fn config_with(fields: &[(&str, Option<ProvFieldType>)]) -> WorkspaceConfig {
        let mut config = WorkspaceConfig::default();
        for (name, ty) in fields {
            config.fields.insert(
                (*name).to_string(),
                vec![FieldSpec {
                    ty: *ty,
                    values: OpenClosed::default(),
                    vocabulary: None,
                    default: None,
                    under: None,
                    stamp: None,
                }],
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
    /// This is also what holds [`FILING_NESTS`] to `prov::filing::Nest`: the
    /// nests are a closed picker, so a spelling prov renames fails right here.
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
                // surface that places the term, which for a filing entry is one
                // `nest:` with no `field:` beside it — so prov also reports the
                // missing sibling, correctly, and about a key this test is not
                // asking after. Judging the term by that would fail every entry inside
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
        config.fields.get_mut("audience").unwrap()[0].vocabulary = Some("audiences.yaml".into());
        // A field declared twice, each under an index, is written as a list —
        // the other spelling of every `fields.<name>` key, plus the two keys
        // only a scoped declaration or a stencil carries.
        config.fields.insert(
            "status".to_string(),
            ["Tasks", "Proposals"]
                .map(|under| FieldSpec {
                    ty: Some(ProvFieldType::Str),
                    values: OpenClosed::Closed,
                    vocabulary: Some(format!("{under}.md")),
                    default: Some(Value::String("open".into())),
                    under: Some(under.to_string()),
                    stamp: None,
                })
                .to_vec(),
        );
        // The two stamps, each declared on its field — a stamp alone is a
        // whole declaration.
        for (name, stamp) in [("created", Stamp::Create), ("updated", Stamp::Edit)] {
            config.fields.insert(
                name.to_string(),
                vec![FieldSpec {
                    ty: None,
                    values: OpenClosed::default(),
                    vocabulary: None,
                    default: None,
                    under: None,
                    stamp: Some(stamp),
                }],
            );
        }
        // A view with every key it takes, and a filing entry filing by a list
        // of fields.
        config.views.push(ViewSpec {
            label: Some("Open tasks".into()),
            icon: Some("checklist".into()),
            filter: Some(Expression::parse("under('Tasks') && status == 'open'").unwrap()),
            ..ViewSpec::new("open", Expression::parse("year(created)").unwrap())
        });
        config.filing.push(FilingSpec {
            name: "daily".into(),
            label: Some("Daily".into()),
            under: Some("[[Daily]]".into()),
            field: vec!["date_of_document".into(), "created".into()],
            nest: Some(Nest::Grain(Grain::Month)),
            kind: Vec::new(),
        });
        config.root = Some("README.md".to_string());
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
                Value::Sequence(items) => {
                    for (i, child) in items.iter().enumerate() {
                        path.push(Seg::Index(i));
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

    /// The view block is governed through the wildcard: its two expressions are
    /// text, and none of the keys prov retired is governed — a row for `group:`
    /// would offer to write a view prov no longer reads.
    #[test]
    fn a_view_entry_is_governed_as_two_expressions() {
        let schema = config_schema(&config_with(&[("people", Some(ProvFieldType::Str))]));
        let view = |leaf: &str| {
            vec![
                Seg::Key("views".into()),
                Seg::Key("daily".into()),
                Seg::Key(leaf.into()),
            ]
        };

        for leaf in ["where", "key"] {
            let rule = schema
                .rule_for(&view(leaf))
                .unwrap_or_else(|| panic!("views.*.{leaf}"));
            assert_eq!(rule.ty, Some(FieldType::Str), "{leaf} is an expression");
            assert!(rule.enum_constraint().is_none(), "{leaf} is not a picker");
            let why = rule.present.description.as_deref().unwrap_or_default();
            assert!(
                why.contains("under"),
                "{leaf} names prov's functions: {why}"
            );
        }
        assert!(schema.rule_for(&view("label")).is_some());
        assert!(schema.rule_for(&view("icon")).is_some());

        for retired in RETIRED_VIEW_KEYS {
            assert!(
                schema.rule_for(&view(retired)).is_none(),
                "views.*.{retired} is retired and should not be offered"
            );
        }
    }

    /// Filing is its own block: `nest:` is a closed picker over prov's nests,
    /// and `field:` is offered rather than enforced, in both of its shapes.
    #[test]
    fn a_filing_entry_is_governed_and_its_field_stays_open() {
        let schema = config_schema(&config_with(&[("people", Some(ProvFieldType::Str))]));
        let entry = |leaf: &str| {
            vec![
                Seg::Key("filing".into()),
                Seg::Key("daily".into()),
                Seg::Key(leaf.into()),
            ]
        };

        let nest = schema.rule_for(&entry("nest")).expect("filing.*.nest");
        let (nests, closed) = nest.enum_constraint().expect("a nest picker");
        assert!(closed, "a nest prov cannot parse files nothing");
        assert_eq!(
            nests.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
            FILING_NESTS.iter().map(|(v, _)| *v).collect::<Vec<_>>()
        );

        let field = schema.rule_for(&entry("field")).expect("filing.*.field");
        let (fields, closed) = field.enum_constraint().expect("a field picker");
        assert!(!closed, "prov imposes no vocabulary on `field:`");
        assert_eq!(
            fields.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(),
            ["people"]
        );

        // The list form (`field: [a, b]`) is governed too, or every entry that
        // falls back through fields would come out untyped.
        assert!(
            schema
                .rule_for(&[
                    Seg::Key("filing".into()),
                    Seg::Key("daily".into()),
                    Seg::Key("field".into()),
                    Seg::Index(0),
                ])
                .is_some(),
            "the list form of `field:` should be governed"
        );
        assert!(schema.rule_for(&entry("label")).is_some());
        assert!(schema.rule_for(&entry("under")).is_some());
    }

    /// Every nest prov accepts is offered, and nothing else: the picker's
    /// spellings are prov's list, in prov's order.
    #[test]
    fn the_nests_are_provs() {
        assert_eq!(
            FILING_NESTS.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
            prov::filing::NESTS
        );
    }

    /// A field's stamp is a closed picker over prov's two, and the top-level
    /// `updated`/`created` keys prov stopped reading are not offered.
    #[test]
    fn a_stamp_is_declared_on_its_field() {
        let schema = config_schema(&config_with(&[]));
        let field = |leaf: &str| {
            vec![
                Seg::Key("fields".into()),
                Seg::Key("updated".into()),
                Seg::Key(leaf.into()),
            ]
        };
        let stamp = schema.rule_for(&field("stamp")).expect("fields.*.stamp");
        let (terms, closed) = stamp.enum_constraint().expect("a stamp picker");
        assert!(closed);
        for term in terms {
            assert!(
                Stamp::from_config_str(&term.value).is_some(),
                "{} is not a stamp prov reads",
                term.value
            );
        }
        assert!(schema.rule_for(&[Seg::Key("updated".into())]).is_none());
        assert!(schema.rule_for(&[Seg::Key("created".into())]).is_none());
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
        let field = vec![
            Seg::Key("filing".into()),
            Seg::Key("daily".into()),
            Seg::Key("field".into()),
        ];

        let mut overlaid = vec![open_choice_terms(
            PathPat(vec![
                SegPat::Key("filing".into()),
                SegPat::AnyKey,
                SegPat::Key("field".into()),
            ]),
            "Files by",
            Icon::Enum,
            vec![term("date", "The document's date")],
        )];
        overlaid.extend(config_rules(&config));
        let schema = Schema::new(overlaid);

        let (terms, _) = schema
            .rule_for(&field)
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
