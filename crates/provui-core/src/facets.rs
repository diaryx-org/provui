//! What a frontmatter key *is* to prov — the classification, offered as a
//! question and never applied.
//!
//! A prov document's metadata block holds two kinds of thing side by side and
//! looks the same either way: keys prov itself reads to build the workspace
//! (`contents` is an edge, `id` is identity, `prov:` is policy) and keys prov
//! merely carries (`mood: rainy`). A schema-free editor over that block has no
//! way to tell them apart, so it draws `id` and `mood` as the same row and
//! offers to let you type into both.
//!
//! [`Facets`] answers which is which. It is built from the resolved workspace
//! config — the relation vocabulary, the `fields` declarations, the name of the
//! stamped `updated` field — so the answer is *this workspace's*, not a list
//! this crate invented, and a workspace that retracts `link_of` gets `link_of:
//! Carried` without anything here knowing it happened.
//!
//! ## Why this only classifies
//!
//! An application over prov usually separates the two halves in its UI: prov's
//! own structure goes in a sidebar, an inspector, or a footer, and the
//! user-defined values get the form. That is a good design and it is not this
//! crate's to make. The facts are general; the arrangement is a product
//! decision, and each frontend's will differ — a mobile inspector, a terminal
//! band and a settings sheet do not want the same split.
//!
//! So nothing here hides, demotes, reorders, or read-onlys a row. What it does
//! is hand a frontend the lists it would need to do any of those:
//! [`structural_keys`](Facets::structural_keys) and
//! [`managed_keys`](Facets::managed_keys) are shaped to go straight into
//! flower's [`set_demoted`](flower_core::Model::set_demoted) and
//! [`with_managed`](flower_core::Model::with_managed) — one line for a frontend
//! that wants diaryx's separation, and zero for one that wants a flat list.
//!
//! For the link half of the same question — *which* documents a relation field
//! points at, and where each link sits — see [`crate::links`].

use std::collections::BTreeMap;

use fig::Value;
use flower_core::Seg;
use prov::{Cardinality, FieldSpec, OpenClosed, Relation, RelationSet, WorkspaceConfig};

/// The root's embedded policy block — prov's `prov:` key, one of the two homes
/// workspace policy lives in.
pub const POLICY_KEY: &str = "prov";
/// The document's stable identity.
pub const IDENTITY_KEY: &str = "id";
/// The name a nominal (`[[My File]]`) reference resolves against.
pub const TITLE_KEY: &str = "title";
/// A sidecar's pointer at the opaque payload whose bytes are its body.
pub const CONTENT_KEY: &str = "content";
/// A manifest node's pointer at the store listing the directory it claims.
pub const MANIFEST_KEY: &str = "manifest";
/// The declared-opacity marker: `true` says "read my payload as bytes, not as a
/// document", and it wins over what the extension suggests.
pub const ATTACHMENT_KEY: &str = "attachment";
/// The recorded digest of the payload, maintained by prov's fixity pass.
pub const CONTENT_HASH_KEY: &str = "content_hash";

/// Which half of the opaque-payload axis a key is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    /// `content` — the payload's path.
    Content,
    /// `manifest` — the store listing a whole claimed directory.
    Manifest,
    /// `attachment` — the declared-opacity marker.
    Marker,
    /// `content_hash` — the recorded digest.
    Digest,
}

/// A relation field, with everything the vocabulary says about it.
#[derive(Debug, Clone)]
pub struct RelationFacet {
    /// The frontmatter key.
    pub name: String,
    /// One target or a list of them.
    pub cardinality: Cardinality,
    /// The reciprocal field prov maintains, when there is one.
    pub inverse: Option<String>,
    /// The spanning containment backbone — the relation the workspace unfolds
    /// along. At most one relation in a workspace is this.
    pub spanning: bool,
    /// Set when this relation is one of the five **pointers** the root uses to
    /// find the workspace's own machinery (`config`, `registry`, `recycle_bin`,
    /// `history`, `about`). Followed one way and only from the root: the target
    /// is not content, carries no back-link, and is not in the spanning tree.
    pub pointer: bool,
    /// A human gloss of what the relation means, when the workspace declared one
    /// — or prov's own for a preset relation it did not bother to declare.
    /// Carried by prov and never read back; here so a frontend can show it.
    pub means: Option<String>,
}

/// A field the workspace declared in `fields.<name>`.
#[derive(Debug, Clone)]
pub struct FieldFacet {
    /// The frontmatter key.
    pub name: String,
    /// The vocabulary document its terms are checked against, when it names one.
    /// `None` for a field that declares only a type.
    pub vocabulary: Option<String>,
    /// Whether an unknown term is rejected (`closed`) or merely unlisted.
    pub values: OpenClosed,
    /// Whether each term is a document in its own right rather than a row in a
    /// flat store — in which case its terms are ordinary content, reachable
    /// down the spanning tree as well as through this pointer.
    pub reify: bool,
}

/// What a frontmatter key is to prov.
///
/// Exhaustive over a document's *top-level* keys: every key falls in exactly one
/// of these, and [`Facet::Carried`] is the one that means "prov does not read
/// this". A nested key takes its top-level ancestor's facet — see
/// [`Facets::of`].
#[derive(Debug, Clone)]
pub enum Facet {
    /// A link field: the targets are edges in the workspace graph.
    Relation(RelationFacet),
    /// The root's `prov:` block — workspace policy, inline.
    Policy,
    /// `id` — the document's stable identity. Minted and maintained by the
    /// workspace, not typed.
    Identity,
    /// `title` — read back by prov for nominal references and for the generated
    /// `about` page, but written by a person.
    Title,
    /// One of the four keys on the opaque-payload axis.
    Payload(Payload),
    /// The field the workspace's `updated:` config names — machine-stamped in
    /// RFC 3339 UTC because prov reads it back to know when to rewrite it. The
    /// *name* is the workspace's; a human-friendly date is a different,
    /// user-owned field prov never touches.
    Stamp,
    /// Declared in `fields.<name>`: prov resolves its values against a
    /// vocabulary, or at least knows their type.
    Field(FieldFacet),
    /// Carried by prov and never read by it. The default, and the majority of
    /// an ordinary document.
    Carried,
}

impl Facet {
    /// Whether prov reads this key at all. `false` only for
    /// [`Carried`](Facet::Carried).
    pub fn read_by_prov(&self) -> bool {
        !matches!(self, Facet::Carried)
    }

    /// Whether this is prov's own *structure* rather than something the document
    /// says about itself — the line a frontend that separates the two draws.
    ///
    /// `contents`, `part_of`, `config`, `prov:`, `id`, `content_hash` are
    /// structure. `title` is not, and neither is a declared field: prov reads
    /// both, but a person wrote them, and putting `audience: public` behind the
    /// same fold as `id` hides the thing the reader came for.
    ///
    /// A question, not a policy. Nothing in this crate acts on it.
    pub fn structural(&self) -> bool {
        !matches!(self, Facet::Title | Facet::Field(_) | Facet::Carried)
    }

    /// Whether the *workspace* maintains this value, so an editor should draw
    /// the row and decline the edit rather than offer a text box.
    ///
    /// `id` is minted, `content_hash` is computed, the `updated` stamp is
    /// written on save. Typing into any of the three does not change what it
    /// will say after the next prov operation; it only makes the document
    /// briefly wrong. This is exactly flower's `derived` set — see
    /// [`managed_keys`](Facets::managed_keys).
    pub fn managed(&self) -> bool {
        matches!(
            self,
            Facet::Identity | Facet::Stamp | Facet::Payload(Payload::Digest)
        )
    }

    /// The relation this key declares, when it is one — the test a frontend
    /// applies before offering to follow a row.
    pub fn relation(&self) -> Option<&RelationFacet> {
        match self {
            Facet::Relation(rel) => Some(rel),
            _ => None,
        }
    }

    /// A short, frontend-neutral name for the kind — for a badge, a filter, or a
    /// status line that wants to say what a row is without a match arm.
    pub fn kind(&self) -> &'static str {
        match self {
            Facet::Relation(rel) if rel.pointer => "pointer",
            Facet::Relation(_) => "relation",
            Facet::Policy => "policy",
            Facet::Identity => "identity",
            Facet::Title => "title",
            Facet::Payload(_) => "payload",
            Facet::Stamp => "stamp",
            Facet::Field(_) => "field",
            Facet::Carried => "carried",
        }
    }
}

/// The classifier: one workspace's answer to "what is this key?".
///
/// Cheap to build and cheap to hold — it is the config's vocabulary, resolved
/// once, and every lookup is a map hit. Build it from the workspace config when
/// there is one and take [`Facets::default`] when there is not: a lone document
/// opened outside any workspace is still read with prov's built-in vocabulary,
/// which is what makes `contents` mean `contents` in a file nobody has
/// configured.
#[derive(Debug, Clone)]
pub struct Facets {
    relations: RelationSet,
    /// Per relation name, everything the vocabulary says about it — built once
    /// so `of_key` is a lookup rather than a scan of `relations()`.
    by_relation: BTreeMap<String, RelationFacet>,
    fields: BTreeMap<String, FieldFacet>,
    /// The workspace's stamped-`updated` field name; empty means the axis is off.
    stamp: Option<String>,
}

impl Default for Facets {
    /// prov's built-in vocabulary and nothing else — the right answer for a
    /// document read outside a workspace, which is still a prov document.
    fn default() -> Self {
        Self::from_config(&WorkspaceConfig::default())
    }
}

impl Facets {
    /// Classify against a resolved workspace config.
    pub fn from_config(config: &WorkspaceConfig) -> Self {
        let relations = config.relation_set();
        let mut by_relation = BTreeMap::new();
        for relation in relations.relations() {
            by_relation.insert(
                relation.name.clone(),
                relation_facet(relation, &relations, config),
            );
        }
        // The workspace-wide declaration of each field; see
        // `schema::workspace_fields` for what a scoped one is to this crate.
        let fields = crate::schema::workspace_fields(config)
            .map(|(name, spec)| (name.clone(), field_facet(name, spec)))
            .collect();
        Self {
            relations,
            by_relation,
            fields,
            stamp: (!config.updated.is_empty()).then(|| config.updated.clone()),
        }
    }

    /// The relation vocabulary these facets read by — what [`crate::links`]
    /// walks, and what a frontend hands prov when it resolves a target.
    pub fn relations(&self) -> &RelationSet {
        &self.relations
    }

    /// Classify a top-level key.
    ///
    /// Relations first, so a workspace that declares `fields.contents` — legal,
    /// and a thing a confused config can say — still gets a link field for the
    /// key prov will follow. The `fields` half only reaches keys the relation
    /// vocabulary left alone.
    pub fn of_key(&self, key: &str) -> Facet {
        if let Some(relation) = self.by_relation.get(key) {
            return Facet::Relation(relation.clone());
        }
        if self.stamp.as_deref() == Some(key) {
            return Facet::Stamp;
        }
        match key {
            POLICY_KEY => return Facet::Policy,
            IDENTITY_KEY => return Facet::Identity,
            TITLE_KEY => return Facet::Title,
            CONTENT_KEY => return Facet::Payload(Payload::Content),
            MANIFEST_KEY => return Facet::Payload(Payload::Manifest),
            ATTACHMENT_KEY => return Facet::Payload(Payload::Marker),
            CONTENT_HASH_KEY => return Facet::Payload(Payload::Digest),
            _ => {}
        }
        match self.fields.get(key) {
            Some(field) => Facet::Field(field.clone()),
            None => Facet::Carried,
        }
    }

    /// Classify a metadata path.
    ///
    /// The **first** segment decides, so `contents[2]` is the relation
    /// `contents` and `prov.relations.see_also.inverse` is policy. That is not a
    /// shortcut: a path's facet is a fact about which of prov's axes it belongs
    /// to, and every segment below the first is a part of the same one. It also
    /// matches how flower scopes its own managed sets, which are root keys
    /// matched exactly — so a list built here goes into `set_demoted` meaning
    /// what it meant on the way out.
    ///
    /// An empty path — the document itself — is [`Facet::Carried`]: the document
    /// is not one of prov's keys.
    pub fn of(&self, path: &[Seg]) -> Facet {
        match path.first() {
            Some(Seg::Key(key)) => self.of_key(key),
            _ => Facet::Carried,
        }
    }

    /// Every top-level key of `meta`, in document order, with its facet.
    ///
    /// Document order, not sorted: the order keys are written in is the
    /// document's own and a lossless editor's whole point. A caller that wants
    /// them grouped groups them.
    pub fn classify(&self, meta: &Value) -> Vec<(String, Facet)> {
        top_level_keys(meta)
            .into_iter()
            .map(|key| {
                let facet = self.of_key(&key);
                (key, facet)
            })
            .collect()
    }

    /// The keys present in `meta` that are prov's structure
    /// ([`Facet::structural`]) — shaped for
    /// [`Model::set_demoted`](flower_core::Model::set_demoted).
    ///
    /// Present in the document, not every key prov knows: demoting a key the
    /// document does not have is harmless but tells a reader nothing, and the
    /// list is short enough to be worth being exact about.
    pub fn structural_keys(&self, meta: &Value) -> Vec<String> {
        self.keys_where(meta, |facet| facet.structural())
    }

    /// The keys present in `meta` that the workspace maintains
    /// ([`Facet::managed`]) — shaped for the `derived` argument of
    /// [`Model::with_managed`](flower_core::Model::with_managed).
    pub fn managed_keys(&self, meta: &Value) -> Vec<String> {
        self.keys_where(meta, |facet| facet.managed())
    }

    /// Every key this workspace maintains, whether or not a given document
    /// carries it — the same list as [`managed_keys`](Self::managed_keys), asked
    /// without a document.
    ///
    /// The form a *constructor* needs: flower takes its derived set before the
    /// first row list exists, which is before there is a parsed document to ask.
    /// Naming a key the document does not have is inert (there is no row to mark
    /// read-only), and naming one it gains later is the point — a document that
    /// acquires an `id` should not become editable in the same breath.
    pub fn managed_key_names(&self) -> Vec<String> {
        let mut names = vec![IDENTITY_KEY.to_string(), CONTENT_HASH_KEY.to_string()];
        names.extend(self.stamp.clone());
        names
    }

    /// The keys present in `meta` that prov carries and never reads — the
    /// complement a frontend showing "just this document's own values" wants.
    pub fn carried_keys(&self, meta: &Value) -> Vec<String> {
        self.keys_where(meta, |facet| !facet.read_by_prov())
    }

    fn keys_where(&self, meta: &Value, want: impl Fn(&Facet) -> bool) -> Vec<String> {
        self.classify(meta)
            .into_iter()
            .filter(|(_, facet)| want(facet))
            .map(|(key, _)| key)
            .collect()
    }
}

/// A relation's facet, with the pointer flag and the gloss resolved.
fn relation_facet(
    relation: &Relation,
    relations: &RelationSet,
    config: &WorkspaceConfig,
) -> RelationFacet {
    let name = relation.name.as_str();
    let pointer = [
        relations.registry_relation(),
        relations.config_relation(),
        relations.recycle_relation(),
        relations.history_relation(),
        relations.about_relation(),
    ]
    .into_iter()
    .flatten()
    .any(|p| p == name);
    RelationFacet {
        name: relation.name.clone(),
        cardinality: relation.cardinality,
        inverse: relation.inverse.clone(),
        spanning: relations.spanning_relation() == Some(name),
        pointer,
        // The workspace's own gloss wins; prov's preset gloss is the fallback,
        // so an undeclared `contents` reads as prov's `contents` rather than as
        // a blank. A name the preset does not know has no fallback and stays
        // `None` — better an absent gloss than an invented one.
        means: config
            .relation_defs
            .get(name)
            .and_then(|def| def.means.clone())
            .or_else(|| RelationSet::diaryx_means(name).map(str::to_string)),
    }
}

fn field_facet(name: &str, spec: &FieldSpec) -> FieldFacet {
    FieldFacet {
        name: name.to_string(),
        vocabulary: spec.vocabulary.clone(),
        values: spec.values,
        reify: spec.reify,
    }
}

/// The top-level mapping keys of a metadata tree, in document order. Empty for
/// anything that is not a mapping — a document whose whole block is a list is
/// legal input and has no keys to classify.
fn top_level_keys(meta: &Value) -> Vec<String> {
    let Some(entries) = meta.as_mapping() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(key, _)| key.as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov::{Document, FieldType, RelationDef};

    const DOC: &str = "\
---
title: A Note
id: ajp7eq
contents:
- '[Child](child.md)'
part_of: '[Root](/README.md)'
audience: public
mood: rainy
content_hash: sha256-abc
---
# Note
";

    fn meta_of(text: &str) -> Value {
        let doc = Document::parse("note.md", text).expect("parse");
        Value::from(&doc.meta)
    }

    fn workspace() -> WorkspaceConfig {
        let mut config = WorkspaceConfig::default();
        config.fields.insert(
            "audience".to_string(),
            vec![FieldSpec {
                ty: None,
                values: OpenClosed::Closed,
                vocabulary: Some("audiences.yaml".to_string()),
                reify: false,
                default: None,
                under: None,
            }],
        );
        config.updated = "updated".to_string();
        config
    }

    #[test]
    fn separates_provs_own_keys_from_the_ones_it_only_carries() {
        let facets = Facets::from_config(&workspace());
        let meta = meta_of(DOC);

        assert_eq!(
            facets.structural_keys(&meta),
            ["id", "contents", "part_of", "content_hash"],
            "prov's structure, in document order"
        );
        assert_eq!(
            facets.carried_keys(&meta),
            ["mood"],
            "only what prov never reads"
        );
        // `title` and a declared field are read by prov and are still not
        // structure — the distinction the two lists exist to keep apart.
        assert!(facets.of_key("title").read_by_prov());
        assert!(!facets.of_key("title").structural());
        assert!(facets.of_key("audience").read_by_prov());
        assert!(!facets.of_key("audience").structural());
    }

    #[test]
    fn the_workspace_maintains_id_the_digest_and_the_stamp() {
        let facets = Facets::from_config(&workspace());
        let meta = meta_of(DOC);
        assert_eq!(facets.managed_keys(&meta), ["id", "content_hash"]);
        // The same answer asked without a document, which is the form a
        // constructor needs — and it names the stamp this workspace declared.
        assert_eq!(
            facets.managed_key_names(),
            ["id", "content_hash", "updated"]
        );
        assert_eq!(
            Facets::default().managed_key_names(),
            ["id", "content_hash"],
            "no declared stamp, no stamped key"
        );
        // The stamp's *name* is the workspace's, so it is only managed where the
        // workspace declared one.
        assert!(facets.of_key("updated").managed());
        assert!(!Facets::default().of_key("updated").managed());
    }

    /// The point of reading the vocabulary rather than a list this crate keeps:
    /// a workspace that retracts a relation gets an ordinary carried field, and
    /// one that adds a relation gets a followable link, without a line here.
    #[test]
    fn the_vocabulary_is_the_workspaces_not_this_crates() {
        let mut config = WorkspaceConfig::default();
        config.relation_defs.insert(
            "link_of".to_string(),
            RelationDef {
                off: true,
                ..RelationDef::default()
            },
        );
        config.relation_defs.insert(
            "see_also".to_string(),
            RelationDef {
                cardinality: Some(Cardinality::Many),
                means: Some("worth reading beside this".to_string()),
                ..RelationDef::default()
            },
        );
        let facets = Facets::from_config(&config);

        assert!(
            matches!(facets.of_key("link_of"), Facet::Carried),
            "a retracted name is an ordinary field"
        );
        let see_also = facets
            .of_key("see_also")
            .relation()
            .cloned()
            .expect("a declared relation");
        assert_eq!(see_also.means.as_deref(), Some("worth reading beside this"));
        assert!(!see_also.spanning);

        // The preset's own gloss stands in for a relation nobody declared.
        let contents = facets.of_key("contents");
        let contents = contents.relation().expect("contents is a relation");
        assert!(contents.spanning, "contents is the backbone");
        assert_eq!(
            contents.means.as_deref(),
            Some("documents contained by this one")
        );
    }

    #[test]
    fn a_pointer_relation_is_marked_as_machinery() {
        let facets = Facets::default();
        let config = facets.of_key("config");
        let config = config.relation().expect("config is a relation");
        assert!(config.pointer, "config points at machinery");
        assert!(!config.spanning);
        assert_eq!(Facet::Relation(config.clone()).kind(), "pointer");

        let contents = facets.of_key("contents");
        assert!(!contents.relation().expect("relation").pointer);
    }

    /// A path's facet is its top-level key's — which is also how flower scopes
    /// the managed sets these lists feed.
    #[test]
    fn a_nested_path_takes_its_top_level_keys_facet() {
        let facets = Facets::default();
        let nested = [Seg::Key("contents".into()), Seg::Index(2)];
        assert!(matches!(facets.of(&nested), Facet::Relation(_)));
        assert!(matches!(
            facets.of(&[Seg::Key("prov".into()), Seg::Key("spanning".into())]),
            Facet::Policy
        ));
        assert!(matches!(facets.of(&[]), Facet::Carried), "the document");
    }

    #[test]
    fn a_declared_field_carries_its_vocabulary() {
        let mut config = workspace();
        config.fields.insert(
            "created".to_string(),
            vec![FieldSpec {
                ty: Some(FieldType::Str),
                values: OpenClosed::default(),
                vocabulary: None,
                reify: false,
                default: None,
                under: None,
            }],
        );
        let facets = Facets::from_config(&config);
        match facets.of_key("audience") {
            Facet::Field(field) => {
                assert_eq!(field.vocabulary.as_deref(), Some("audiences.yaml"));
                assert!(matches!(field.values, OpenClosed::Closed));
            }
            other => panic!("expected a declared field, got {other:?}"),
        }
        match facets.of_key("created") {
            Facet::Field(field) => assert!(field.vocabulary.is_none()),
            other => panic!("expected a declared field, got {other:?}"),
        }
    }
}
