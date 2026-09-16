//! The links a document's metadata declares — where each one sits, what it says,
//! and what shape of target it names.
//!
//! prov already extracts a document's edges
//! ([`RelationSet::edges`](prov::RelationSet::edges)), but an editor needs one
//! thing that answer does not carry: **where** each link is. An edge is a
//! `(relation, target)` pair, and a frontend asking "the row under the cursor —
//! is that a link, and if so which one?" needs `contents[2]`, not the third
//! string of the `contents` field. So [`MetaLink`] is prov's edge with its
//! metadata path attached, and [`link_at`] is the cursor question asked directly.
//!
//! Everything here is **lexical and offline**: a target is parsed, classified by
//! syntax, and handed back. No filesystem, no registry, no claim that anything
//! exists. Turning a [`MetaLink`] into a document you can open is
//! [`crate::workspace`], because that is the step that needs a workspace to do
//! it in — and a frontend that only wants to *draw* links differently (an icon,
//! a tint, the label instead of the path) needs none of that and should not
//! link it.

use fig::Value;
use flower_core::Seg;
use prov::Link;
use prov::link::IdRef;

use crate::facets::{Facet, Facets};

/// What shape of thing a link's target names, by syntax alone.
///
/// prov's own [`Target`](prov::Target) is the *resolved* answer and needs a
/// workspace to produce; this is what can be known from the string, which is
/// what an editor drawing a row has. The two line up variant for variant, so a
/// frontend that starts with this and later gains a workspace does not restate
/// its rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetKind {
    /// A path — relative to the document, or workspace-absolute with a leading
    /// `/`. The ordinary case, and the only one resolvable without a registry.
    Path,
    /// An `id:<id>` handle into this workspace's registry.
    Id,
    /// An `id:<workspace>/<id>` handle into *another* workspace. prov holds no
    /// map from a workspace name to a location, so this names something without
    /// locating it.
    Foreign {
        /// The workspace qualifier, as written.
        workspace: String,
    },
    /// A URL or mail address — off-workspace, never resolved and never
    /// rewritten by a move.
    External,
    /// A target that is *only* a `#locator`: a place inside the document the
    /// link is written in, naming no other document.
    SameDocument,
    /// The `id:` scheme with a body that is no reference at all (`id:`,
    /// `id:ws/`, `id:a/b/c`).
    ///
    /// Its own case rather than a path, on prov's grounds: the author wrote
    /// `id:`, so reading the rest as a filename would turn a typo into a
    /// dangling path and hide what actually went wrong.
    MalformedId,
}

/// One link declared by a document's metadata.
#[derive(Debug, Clone)]
pub struct MetaLink {
    /// Where the link sits in the metadata: `[Key("part_of")]` for a scalar
    /// relation, `[Key("contents"), Index(2)]` for the third item of a list.
    /// The path a frontend compares against its cursor.
    pub path: Vec<Seg>,
    /// The relation that declared it, and everything the vocabulary says about
    /// that relation — including whether it is the spanning backbone and whether
    /// it is a one-way pointer at machinery.
    pub relation: crate::facets::RelationFacet,
    /// The parsed link: its label, its target, and which wrapper it was written
    /// in. [`Link::render`](prov::Link::render) puts it back the way it came.
    pub link: Link,
    /// What shape of thing the target names, by syntax.
    pub kind: TargetKind,
}

impl MetaLink {
    /// The target as written, locator and all.
    pub fn target(&self) -> &str {
        &self.link.target
    }

    /// The `#locator` suffix, when the target has one — a place inside the
    /// document the rest of the target names. Carried by prov, never resolved.
    pub fn locator(&self) -> Option<&str> {
        self.link.locator()
    }

    /// What to put on a row: the link's label when it was written with one, and
    /// the target otherwise. A labeled link is labeled *because* the target is
    /// not what a reader wants to read.
    pub fn display(&self) -> &str {
        self.link.label.as_deref().unwrap_or(&self.link.target)
    }

    /// Whether following this link would leave the workspace — a URL, or a
    /// reference into a workspace prov cannot locate from here.
    pub fn leaves_the_workspace(&self) -> bool {
        matches!(self.kind, TargetKind::External | TargetKind::Foreign { .. })
    }
}

/// A link, wherever it was written — the shape [`crate::WorkspaceView`] resolves.
///
/// Resolution needs exactly two things from a link: the parsed [`Link`] prov
/// reads the target off, and the [`TargetKind`] that says whether there is
/// anything to resolve at all. Neither is a fact about *where* the link sits, so
/// neither [`MetaLink`]'s metadata path nor [`BodyLink`]'s byte span appears
/// here — and a body link consequently resolves through the same code a
/// frontmatter link does, in a workspace or without one.
///
/// [`BodyLink`]: crate::BodyLink
pub trait AnyLink {
    /// The parsed link: label, target, wrapper.
    fn link(&self) -> &Link;

    /// What shape of thing the target names, by syntax.
    fn kind(&self) -> &TargetKind;

    /// The target as written, locator and all.
    fn target(&self) -> &str {
        &self.link().target
    }

    /// Whether following this link would leave the workspace — a URL, or a
    /// reference into a workspace prov cannot locate from here.
    fn leaves_the_workspace(&self) -> bool {
        matches!(
            self.kind(),
            TargetKind::External | TargetKind::Foreign { .. }
        )
    }
}

impl AnyLink for MetaLink {
    fn link(&self) -> &Link {
        &self.link
    }

    fn kind(&self) -> &TargetKind {
        &self.kind
    }
}

impl AnyLink for crate::BodyLink {
    fn link(&self) -> &Link {
        &self.link
    }

    fn kind(&self) -> &TargetKind {
        &self.kind
    }
}

/// Classify a target string by syntax. The order matters: `id:` handles are
/// checked before anything else because `id:ajp7eq` is also a syntactically
/// valid relative path, and prov reads the scheme first.
///
/// Crate-visible rather than private because [`crate::body_links`] classifies a
/// prose link with it: the same target written in `contents` and written in a
/// paragraph is the same target, and two copies of this would be two chances to
/// disagree about `id:`.
pub(crate) fn kind_of(link: &Link) -> TargetKind {
    if link.is_same_document() {
        return TargetKind::SameDocument;
    }
    match link.id_ref() {
        Some(IdRef::Local(_)) => return TargetKind::Id,
        Some(IdRef::Foreign { workspace, .. }) => {
            return TargetKind::Foreign {
                workspace: workspace.to_string(),
            };
        }
        Some(IdRef::Malformed) => return TargetKind::MalformedId,
        None => {}
    }
    if link.is_external() {
        return TargetKind::External;
    }
    TargetKind::Path
}

/// Every link `meta` declares, in relation order and then in list order.
///
/// A relation field holding a scalar yields one link at `[Key(name)]`; one
/// holding a sequence yields one per **string** item, at `[Key(name),
/// Index(i)]`. A non-string item is skipped rather than guessed at: prov reads a
/// relation's targets as strings, and a map inside a `contents:` list is a
/// document that needs fixing, not a link this crate should invent.
pub fn links_in(meta: &Value, facets: &Facets) -> Vec<MetaLink> {
    let mut links = Vec::new();
    for relation in facets.relations().relations() {
        let Facet::Relation(facet) = facets.of_key(&relation.name) else {
            continue;
        };
        let Some(value) = meta.get(relation.name.as_str()) else {
            continue;
        };
        let at = |path: Vec<Seg>, raw: &str| MetaLink {
            path,
            relation: facet.clone(),
            link: parse(raw),
            kind: TargetKind::Path, // replaced below; `parse` is needed first
        };
        match value {
            Value::Seq(items) => {
                for (index, item) in items.iter().enumerate() {
                    if let Some(raw) = item.as_str() {
                        let path = vec![Seg::Key(relation.name.clone()), Seg::Index(index)];
                        links.push(finish(at(path, raw)));
                    }
                }
            }
            other => {
                if let Some(raw) = other.as_str() {
                    let path = vec![Seg::Key(relation.name.clone())];
                    links.push(finish(at(path, raw)));
                }
            }
        }
    }
    links
}

/// The link at exactly `path`, if there is one — the cursor question.
///
/// Exact, not prefix: standing on the `contents` row itself is standing on a
/// *list*, not on a link, and answering with its first item would follow
/// somewhere the cursor was not. A frontend that wants "the list's first link"
/// asks [`links_under`] instead and says so.
pub fn link_at(meta: &Value, facets: &Facets, path: &[Seg]) -> Option<MetaLink> {
    links_in(meta, facets)
        .into_iter()
        .find(|link| link.path == path)
}

/// Every link at or below `path` — the list under a relation row, or the one
/// link a scalar relation row holds.
pub fn links_under(meta: &Value, facets: &Facets, path: &[Seg]) -> Vec<MetaLink> {
    links_in(meta, facets)
        .into_iter()
        .filter(|link| link.path.starts_with(path))
        .collect()
}

/// Parse a relation target.
///
/// [`Link::parse`](prov::Link::parse), not `parse_path_only`: a relation field
/// is exactly where prov permits the Obsidian `[[target]]` wrapper, and reading
/// one as a literal path would make a wikilink vault's every link unfollowable.
/// The opt-out exists for path *properties*, which relation fields are not.
fn parse(raw: &str) -> Link {
    Link::parse(raw)
}

/// Fill in the kind, which needs the parsed link.
fn finish(mut link: MetaLink) -> MetaLink {
    link.kind = kind_of(&link.link);
    link
}

#[cfg(test)]
mod tests {
    use super::*;
    use prov::{Document, WorkspaceConfig};

    const DOC: &str = "\
---
title: A Note
contents:
- '[Child](child.md)'
- '[[notes/other.md|Other]]'
- 'id:ajp7eq'
part_of: '[Root](/README.md)'
links:
- 'https://example.com/'
- '#section-2'
- 'id:otherws/bkq8fr'
config: prov.yaml
mood: rainy
---
# Note
";

    fn meta() -> Value {
        let doc = Document::parse("notes/note.md", DOC).expect("parse");
        Value::from(&doc.meta)
    }

    fn facets() -> Facets {
        Facets::from_config(&WorkspaceConfig::default())
    }

    fn at(links: &[MetaLink], path: &[Seg]) -> MetaLink {
        links
            .iter()
            .find(|l| l.path == path)
            .unwrap_or_else(|| panic!("no link at {path:?}"))
            .clone()
    }

    #[test]
    fn every_link_knows_where_it_sits() {
        let meta = meta();
        let links = links_in(&meta, &facets());
        let paths: Vec<Vec<Seg>> = links.iter().map(|l| l.path.clone()).collect();
        assert!(paths.contains(&vec![Seg::Key("contents".into()), Seg::Index(1)]));
        assert!(paths.contains(&vec![Seg::Key("part_of".into())]));
        assert!(paths.contains(&vec![Seg::Key("config".into())]));
        // `mood` is not a relation, so it contributes nothing.
        assert!(!paths.iter().any(|p| p == &vec![Seg::Key("mood".into())]));
    }

    #[test]
    fn the_label_and_the_wrapper_survive_the_round_trip() {
        let meta = meta();
        let links = links_in(&meta, &facets());

        let child = at(&links, &[Seg::Key("contents".into()), Seg::Index(0)]);
        assert_eq!(child.display(), "Child");
        assert_eq!(child.target(), "child.md");
        assert_eq!(child.link.render(), "[Child](child.md)");

        // A wikilink is a wikilink, not a literal — this is the relation field
        // where prov permits the wrapper.
        let other = at(&links, &[Seg::Key("contents".into()), Seg::Index(1)]);
        assert!(other.link.wikilink);
        assert_eq!(other.display(), "Other");
        assert_eq!(other.link.render(), "[[notes/other.md|Other]]");
    }

    #[test]
    fn a_target_is_classified_by_syntax_alone() {
        let meta = meta();
        let links = links_in(&meta, &facets());

        assert_eq!(
            at(&links, &[Seg::Key("contents".into()), Seg::Index(0)]).kind,
            TargetKind::Path
        );
        assert_eq!(
            at(&links, &[Seg::Key("contents".into()), Seg::Index(2)]).kind,
            TargetKind::Id
        );
        assert_eq!(
            at(&links, &[Seg::Key("links".into()), Seg::Index(0)]).kind,
            TargetKind::External
        );
        assert_eq!(
            at(&links, &[Seg::Key("links".into()), Seg::Index(1)]).kind,
            TargetKind::SameDocument
        );
        assert_eq!(
            at(&links, &[Seg::Key("links".into()), Seg::Index(2)]).kind,
            TargetKind::Foreign {
                workspace: "otherws".into()
            }
        );
        assert!(at(&links, &[Seg::Key("links".into()), Seg::Index(2)]).leaves_the_workspace());
    }

    /// The vocabulary is what says a key is a link, so the spanning backbone and
    /// the one-way machinery pointer arrive marked as what they are — which is
    /// the difference between "open this document" and "open the workspace's
    /// config".
    #[test]
    fn a_link_carries_what_its_relation_is() {
        let meta = meta();
        let links = links_in(&meta, &facets());

        let child = at(&links, &[Seg::Key("contents".into()), Seg::Index(0)]);
        assert!(child.relation.spanning);
        assert!(!child.relation.pointer);

        let config = at(&links, &[Seg::Key("config".into())]);
        assert!(config.relation.pointer);
        assert!(!config.relation.spanning);
    }

    #[test]
    fn the_cursor_question_is_exact() {
        let meta = meta();
        let facets = facets();

        assert!(
            link_at(&meta, &facets, &[Seg::Key("part_of".into())]).is_some(),
            "a scalar relation row is a link"
        );
        assert!(
            link_at(&meta, &facets, &[Seg::Key("contents".into())]).is_none(),
            "standing on the list is not standing on a link"
        );
        assert_eq!(
            links_under(&meta, &facets, &[Seg::Key("contents".into())]).len(),
            3,
            "the list's own links, for a caller that asks for them"
        );
    }

    /// A relation whose value is a list holding something that is not a string
    /// is a document to fix, not a link to invent.
    #[test]
    fn a_non_string_item_is_skipped_rather_than_guessed_at() {
        const ODD: &str = "---\ncontents:\n- ok.md\n- {a: b}\n---\n# x\n";
        let doc = Document::parse("note.md", ODD).expect("parse");
        let links = links_in(&Value::from(&doc.meta), &facets());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target(), "ok.md");
    }
}
