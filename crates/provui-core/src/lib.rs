//! provui-core — a frontend-neutral UI composition core over
//! [`prov`](https://docs.rs/prov).
//!
//! prov describes a plaintext workspace; [`flower`](https://docs.rs/flower-core)
//! edits structured metadata; [`leaf`](https://docs.rs/leaf-core) edits prose.
//! This crate is the composition of the three, with no opinion about what draws
//! it — the same core is meant to sit under a TUI, a SwiftUI app behind UniFFI,
//! or a test harness:
//!
//! - [`ProvBackend`] — a [`flower_core::Backend`] that edits a prov document's
//!   embedded metadata through prov's carrier-aware
//!   [`prov::edit::MetaEditor`]. Lossless: comments, key order, the
//!   carrier/format, and the prose body are all preserved. Unlike
//!   [`flower_core::FigBackend`] (a standalone config file, schema-free), a
//!   `ProvBackend` can carry the workspace **schema** — the controlled
//!   vocabularies and relations resolved from the prov config — so a frontend
//!   renders term pickers, spanning-link widgets, and type-directed edits.
//! - [`DocumentSession`] — one open prov document edited through a flower metadata
//!   model *and* a leaf body editor, reconciled on save.
//! - [`schema_from_config`] — the adapter turning a resolved prov
//!   [`WorkspaceConfig`](prov::config::WorkspaceConfig) (+ its vocabularies) into a
//!   generic [`flower_core::Schema`] for the workspace's **content** documents.
//!   This is where prov's controlled vocabularies and spanning relation reach the
//!   UI. [`schema_for_document`] is the same adapter for one document: a field
//!   prov declares `under:` an index governs only the documents below it, so
//!   which declaration a document is edited under is a fact about where it sits.
//! - [`config_schema()`] — the same trick turned on the config document itself, so
//!   the metadata editor a frontend already ships can edit a workspace's policy
//!   instead of a hand-written settings form.
//! - [`facets`] — what each frontmatter key *is* to prov: a relation, a pointer
//!   at machinery, identity, policy, a declared field, or a value prov only
//!   carries. Read off the workspace's own vocabulary rather than a list this
//!   crate keeps.
//! - [`links`] — the links a document's *frontmatter* declares, each with the
//!   metadata **path** it sits at, so "is the row under the cursor a link?" is a
//!   question with an answer. Lexical: no filesystem, no registry.
//! - [`mod@body_links`] — the same question of the *prose*, answered with a byte
//!   range into the body instead of a metadata path. Both kinds carry a
//!   [`prov::Link`] and a [`TargetKind`], and [`AnyLink`] is what lets one
//!   resolver answer for both — so following a link from the body caret and
//!   following one from the metadata cursor are the same code.
//! - [`workspace`] — [`WorkspaceView`], which finds the workspace a document
//!   belongs to and resolves a link to a document you can open. The one piece
//!   that reads the filesystem, and read-only. It also runs that backwards:
//!   [`WorkspaceView::reference_to`] and [`reference_here`] give the link text
//!   to *write* to a document — or to a place inside one — in the workspace's
//!   own reference style. Still a read; nothing is written and nothing is
//!   registered.
//!
//! ## What this crate will not do for you
//!
//! It classifies, and it never arranges. Nothing here hides a row, sinks one,
//! reorders them, or makes one read-only — even where it plainly knows enough
//! to: [`Facets`] can tell you `id` is minted and `contents` is structure, and
//! hands you the lists shaped to go straight into flower's `derived` and
//! `demoted` sets, and then stops.
//!
//! That is deliberate. An application over prov usually does separate prov's
//! structure from the values a person typed — diaryx does — but *how* is a
//! product decision, and a mobile inspector, a terminal band and a settings
//! sheet do not want the same one. The classification is general and lives here
//! once; the arrangement is local and lives in the frontend. `provui-tui`'s
//! `nav` module is a worked example of the whole policy, and it is two lines.
//!
//! Scope: the single-document metadata surface (prov's `edit` layer), plus
//! read-only navigation across documents. Relation fields that *maintain inverse
//! links* across documents belong to prov's `mutate` layer — a later,
//! relationship-aware backend, not this one. Following a link reads; retargeting
//! one would write two documents, and this crate's backend edits one.

pub mod body_links;
pub mod config_schema;
pub mod facets;
pub mod links;
pub mod rules;
pub mod schema;
mod session;
pub mod workspace;

pub use body_links::{BodyLink, body_link_at, body_links};
pub use config_schema::{CONFIG_READONLY_KEYS, config_schema};
pub use facets::{Facet, Facets};
pub use links::{AnyLink, MetaLink, TargetKind, link_at, links_in, links_under};
pub use schema::{Vocabularies, schema_for_document, schema_from_config};
pub use session::{DocumentSession, Heading, SessionError};
pub use workspace::{Destination, WorkspaceView, reference_here, reference_without_workspace};

use fig::Value;
use flower_core::tree::{self, to_fig};
use flower_core::{Backend, BackendError, EditOp, Schema, Seg};
use prov::edit::MetaEditor;
use prov::{Document, MetaCarrier};

fn be(e: impl std::fmt::Display) -> BackendError {
    BackendError(e.to_string())
}

/// Run one expression against whichever fig editor sits behind a
/// [`MetaEditor`]. The fenced and whole-file editors share every comment
/// method by name and signature without sharing a trait, so a comment op is
/// one body written once and matched into both arms.
macro_rules! with_fig {
    ($editor:expr, |$e:ident| $body:expr) => {
        match $editor {
            MetaEditor::Fenced($e) => $body,
            MetaEditor::Whole($e) => $body,
        }
    };
}

/// A comment read is an answer, not a failure, on a format with no comment
/// syntax: a page over JSON frontmatter has no comments on it, rather than a
/// read error on every row. A write to such a format still refuses.
fn comment_read(read: Result<Option<String>, fig::Error>) -> Result<Option<String>, BackendError> {
    match read {
        Err(fig::Error::UnsupportedFormat) => Ok(None),
        other => other.map_err(be),
    }
}

/// A backend over a single prov document, editing its embedded metadata.
pub struct ProvBackend {
    /// The document path — drives carrier/format detection (extension for a
    /// whole-file config doc, content sniffing for a fenced block).
    path: std::path::PathBuf,
    /// The current full document text (frontmatter + body); the source of truth.
    text: String,
    /// The schema governing this document, when the embedder resolved one from the
    /// workspace config (see [`schema_from_config`]). Returned via
    /// [`Backend::schema`] so the flower model validates values and a frontend can
    /// pick schema-driven widgets. `None` for a bare document with no workspace.
    schema: Option<Schema>,
}

impl ProvBackend {
    /// Open a prov document from its full `text`, with no schema. Errors if prov
    /// cannot parse it.
    pub fn open(
        path: impl Into<std::path::PathBuf>,
        text: impl Into<String>,
    ) -> Result<Self, BackendError> {
        Self::open_with_schema_opt(path, text, None)
    }

    /// Open a prov document carrying the workspace `schema` — the prov-aware path,
    /// so the flower model validates controlled fields and offers pickers.
    pub fn open_with_schema(
        path: impl Into<std::path::PathBuf>,
        text: impl Into<String>,
        schema: Schema,
    ) -> Result<Self, BackendError> {
        Self::open_with_schema_opt(path, text, Some(schema))
    }

    fn open_with_schema_opt(
        path: impl Into<std::path::PathBuf>,
        text: impl Into<String>,
        schema: Option<Schema>,
    ) -> Result<Self, BackendError> {
        let path = path.into();
        let text = text.into();
        // Fail fast if the document doesn't parse.
        Document::parse(&path, &text).map_err(be)?;
        Ok(Self { path, text, schema })
    }

    fn document(&self) -> Result<Document, BackendError> {
        Document::parse(&self.path, &self.text).map_err(be)
    }

    /// An editor over the metadata block as it stands, for a read — `None` when
    /// the document has no block, which is a document with no comments on it.
    /// (`apply` opens with `open_or_init` instead, since an edit to a block-less
    /// document synthesizes one.)
    fn editor(&self) -> Result<Option<MetaEditor>, BackendError> {
        match self.document()?.carrier {
            Some(carrier) => MetaEditor::open(&self.text, carrier).map(Some).map_err(be),
            None => Ok(None),
        }
    }

    /// The prose body outside the metadata block — the region a `leaf` editor
    /// would own. Empty for a whole-file config document.
    pub fn body(&self) -> Result<String, BackendError> {
        Ok(self.document()?.body)
    }

    /// Whether the document has an editable prose body (a fenced carrier). A
    /// whole-file config document has none — its body cannot be replaced.
    pub fn has_body(&self) -> Result<bool, BackendError> {
        Ok(matches!(
            self.document()?.carrier,
            Some(MetaCarrier::Fenced(_))
        ))
    }

    /// Replace the prose body, leaving the metadata block untouched — the write
    /// path for edits a `leaf` editor makes to [`body`](Self::body).
    ///
    /// Uses fig's `Embed::replace_body` (the same lossless primitive prov edits
    /// through). A frontend that wants fixity/`updated` restamping routes this
    /// through prov's write path instead; here it demonstrates that the metadata
    /// and body regions edit independently over one document.
    pub fn set_body(&mut self, body: &str) -> Result<(), BackendError> {
        match self.document()?.carrier {
            Some(MetaCarrier::Fenced(kind)) => {
                let mut embed = fig::Embed::open(self.text.as_bytes(), kind).map_err(be)?;
                embed.replace_body(body).map_err(be)?;
                self.text = embed.render().map_err(be)?.to_string();
                Ok(())
            }
            _ => Err(BackendError(
                "document has no fenced body to replace".into(),
            )),
        }
    }
}

impl Backend for ProvBackend {
    fn apply(&mut self, op: EditOp) -> Result<(), BackendError> {
        let carrier = self.document()?.carrier;
        // `open_or_init` so an edit to a document with no block synthesizes one
        // (frontmatter for a prose file) rather than failing.
        let mut editor = MetaEditor::open_or_init(&self.text, carrier).map_err(be)?;

        match op {
            EditOp::ReplaceValue { path, value } => {
                let segs = to_fig(&path);
                // Mirror prov's `set_in_text`: an index-terminated path is a pure
                // replacement (there is no "insert at absent index"); a
                // key-terminated path upserts.
                match path.last() {
                    Some(Seg::Index(_)) => editor.replace_value(&segs, value).map_err(be)?,
                    _ => editor.set_value(&segs, value).map_err(be)?,
                }
            }
            EditOp::DeleteKey { path } => editor.delete(&to_fig(&path)).map_err(be)?,
            EditOp::RemoveItem { seq_path, index } => {
                editor.remove_item(&to_fig(&seq_path), index).map_err(be)?
            }
            // prov's MetaEditor has no distinct "insert": `set_value` at the new
            // key path upserts, which is exactly an insert for an absent key.
            EditOp::InsertKey {
                map_path,
                key,
                value,
            } => {
                let mut path = map_path;
                path.push(Seg::Key(key));
                editor.set_value(&to_fig(&path), value).map_err(be)?
            }
            EditOp::AppendItem { seq_path, value } => {
                editor.append_value(&to_fig(&seq_path), value).map_err(be)?
            }
            // No `move_item` on MetaEditor; express the move as a full index
            // permutation through `reorder_items`, sized from the current sequence.
            // The index arithmetic is flower's, so a move here means what a move
            // means through any other backend.
            EditOp::MoveItem { seq_path, from, to } => {
                let len = tree::seq_len(&self.to_value()?, &seq_path)
                    .ok_or_else(|| BackendError("target is not a sequence".into()))?;
                if let Some(order) = flower_core::backend::move_permutation(len, from, to) {
                    editor
                        .reorder_items(&to_fig(&seq_path), &order)
                        .map_err(be)?;
                }
            }
            EditOp::ReorderKeys { map_path, keys } => {
                editor.reorder_keys(&to_fig(&map_path), &keys).map_err(be)?
            }
            EditOp::RenameKey { path, new_key } => {
                editor.replace_key(&to_fig(&path), &new_key).map_err(be)?
            }
            // `MetaEditor` stops at the value ops prov's own mutations need; the
            // comment surface is fig's, reached through whichever editor is
            // behind it. Two fig calls for a leading set, and still atomic: the
            // text is only replaced once every call has succeeded, so a refused
            // add after a delete leaves the document as it was.
            EditOp::SetLeadingComment { path, text } => {
                let path = to_fig(&path);
                with_fig!(&mut editor, |e| {
                    e.delete_leading_comments(&path).map_err(be)?;
                    if let Some(text) = &text {
                        e.add_leading_comment(&path, text).map_err(be)?;
                    }
                })
            }
            EditOp::SetTrailingComment { path, text } => {
                let path = to_fig(&path);
                with_fig!(&mut editor, |e| match &text {
                    Some(text) => e.set_trailing_comment(&path, text).map_err(be)?,
                    None => e.delete_trailing_comment(&path).map_err(be)?,
                })
            }
        }

        self.text = editor.render().map_err(be)?;
        Ok(())
    }

    fn to_value(&self) -> Result<Value, BackendError> {
        // prov's metadata tree → fig's value tree (the serde-free bridge).
        Ok(Value::from(&self.document()?.meta))
    }

    fn source(&self) -> Result<String, BackendError> {
        Ok(self.text.clone())
    }

    fn schema(&self) -> Option<Schema> {
        self.schema.clone()
    }

    fn leading_comment(&self, path: &[Seg]) -> Result<Option<String>, BackendError> {
        let Some(editor) = self.editor()? else {
            return Ok(None);
        };
        let path = to_fig(path);
        comment_read(with_fig!(&editor, |e| e.leading_comment(&path)))
    }

    fn trailing_comment(&self, path: &[Seg]) -> Result<Option<String>, BackendError> {
        let Some(editor) = self.editor()? else {
            return Ok(None);
        };
        let path = to_fig(path);
        comment_read(with_fig!(&editor, |e| e.trailing_comment(&path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flower_core::{Mode, Model};

    const DOC: &str = "\
---
# the title
title: Old Title
draft: true
tags:
- a
- b
---
# Heading

Body prose that must survive metadata edits.
";

    fn model() -> Model<ProvBackend> {
        let backend = ProvBackend::open("note.md", DOC).expect("open prov doc");
        Model::new(backend).expect("build model")
    }

    fn select(model: &mut Model<ProvBackend>, path: &[Seg]) {
        // `select_row`, not a write to `selected`: the field is flower's own now,
        // and the setter is what asserts the tree projection this index belongs to.
        let index = model
            .rows
            .iter()
            .position(|r| r.path == path)
            .unwrap_or_else(|| panic!("no row for {path:?}"));
        model.select_row(index);
    }

    fn type_value(model: &mut Model<ProvBackend>, text: &str) {
        // `..`: an edit now also carries the path it belongs to, which this
        // helper has no use for — it types into whatever is already open.
        if let Mode::Editing { buffer, .. } = &mut model.mode {
            buffer.clear();
        }
        for c in text.chars() {
            model.edit_push(c);
        }
        model.edit_commit();
    }

    /// The `EditOp` contract, checked against flower's own suite.
    ///
    /// `ProvBackend` is the second implementation of that trait, and a trait with
    /// one implementation has only a behavior — this is where the two would
    /// silently part ways. Running flower's suite rather than restating it means a
    /// guarantee added upstream arrives here as a failing test, not as a difference
    /// nobody looked for.
    ///
    /// The fixture is written as frontmatter because that is the carrier a prose
    /// vault uses; the suite asserts on the value tree, so the format is ours to
    /// pick.
    #[test]
    fn prov_backend_satisfies_the_edit_op_contract() {
        const FIXTURE: &str = "\
---
title: note
tags:
- alpha
- beta
- gamma
nested:
  k: v
  j: w
---
# Note

Body prose.
";
        flower_core::backend::conformance::check(|| {
            ProvBackend::open("note.md", FIXTURE).expect("open fixture")
        })
        .expect("prov backend honors the EditOp contract");
    }

    #[test]
    fn renders_frontmatter_as_a_tree() {
        let model = model();
        let keys: Vec<&str> = model
            .rows
            .iter()
            .filter(|r| r.depth == 0)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            keys,
            ["title", "draft", "tags"],
            "top-level frontmatter keys"
        );
    }

    #[test]
    fn edits_metadata_leaving_the_body_untouched() {
        let mut model = model();

        select(&mut model, &[Seg::Key("title".into())]);
        model.begin_edit();
        type_value(&mut model, "New Title");

        let out = model.source_snapshot();
        assert!(out.contains("title: New Title"), "value changed:\n{out}");
        assert!(out.contains("# the title"), "comment preserved:\n{out}");
        assert!(out.starts_with("---\n"), "fences intact:\n{out}");
        assert!(
            out.contains("Body prose that must survive metadata edits."),
            "body preserved:\n{out}"
        );
    }

    #[test]
    fn deletes_a_key() {
        let mut model = model();

        select(&mut model, &[Seg::Key("draft".into())]);
        model.delete_selected();

        let out = model.source_snapshot();
        assert!(!out.contains("draft:"), "key removed:\n{out}");
        assert!(out.contains("title: Old Title"), "siblings kept:\n{out}");
        assert!(out.contains("Body prose"), "body kept:\n{out}");
    }

    #[test]
    fn comments_are_read_per_node_and_edited_in_place_leaving_the_body_alone() {
        let backend = ProvBackend::open("note.md", DOC).expect("open");
        let mut model = Model::new(backend).expect("model");
        let title = [Seg::Key("title".into())];
        let draft = [Seg::Key("draft".into())];

        // The block above `title` is read through the backend, into the page.
        assert_eq!(
            model.leading_comment_at(&title).as_deref(),
            Some("the title")
        );
        assert_eq!(model.leading_comment_at(&draft), None);
        assert_eq!(model.trailing_comment_at(&title), None);

        model.set_leading_comment(&title, Some("what it is called"));
        model.set_trailing_comment(&draft, Some("for now"));
        let out = model.source_snapshot();
        assert!(
            out.contains("# what it is called\ntitle: Old Title"),
            "{out}"
        );
        assert!(
            !out.contains("# the title"),
            "the block is replaced:\n{out}"
        );
        assert!(out.contains("draft: true # for now"), "{out}");
        assert!(
            out.contains("Body prose that must survive"),
            "body kept:\n{out}"
        );

        model.set_leading_comment(&title, None);
        let out = model.source_snapshot();
        assert!(out.starts_with("---\ntitle: Old Title"), "{out}");
    }

    #[test]
    fn a_comment_write_that_fig_refuses_leaves_the_document_as_it_was() {
        let mut backend = ProvBackend::open("note.md", DOC).expect("open");
        let before = backend.source().unwrap();
        // A trailing comment is one line; a second line is refused whole, and
        // the text is not replaced by a partial edit.
        let result = backend.apply(EditOp::SetTrailingComment {
            path: vec![Seg::Key("title".into())],
            text: Some("two\nlines".into()),
        });
        assert!(result.is_err());
        assert_eq!(backend.source().unwrap(), before);
    }

    #[test]
    fn json_frontmatter_has_no_comments_to_read_and_refuses_to_write_one() {
        // `;;;` is the JSON frontmatter fence; `---` around `{…}` would be
        // YAML, which a `{…}` is a flow mapping of, and which has comments.
        let doc = ";;;\n{\"title\": \"Note\"}\n;;;\n# Note\n";
        let mut backend = ProvBackend::open("note.md", doc).expect("open");
        let title = vec![Seg::Key("title".into())];
        assert_eq!(backend.leading_comment(&title).unwrap(), None);
        assert_eq!(backend.trailing_comment(&title).unwrap(), None);
        let before = backend.source().unwrap();
        assert!(
            backend
                .apply(EditOp::SetLeadingComment {
                    path: title,
                    text: Some("nope".into()),
                })
                .is_err()
        );
        assert_eq!(backend.source().unwrap(), before);
    }

    #[test]
    fn a_document_with_no_metadata_block_has_no_comments() {
        let backend = ProvBackend::open("note.md", "# Just prose\n").expect("open");
        assert_eq!(
            backend
                .leading_comment(&[Seg::Key("title".into())])
                .unwrap(),
            None
        );
    }

    #[test]
    fn schema_backed_backend_rejects_a_term_outside_a_closed_vocabulary() {
        use flower_core::schema::{Constraint, FieldRule};
        use flower_core::{FieldType, PathPat, Term};
        // A prov document whose `audience` is a closed vocabulary.
        let doc = "---\ntitle: Note\naudience:\n- public\n---\n# Note\n";
        let schema = Schema::new(vec![
            FieldRule::new(PathPat::each_item_of("audience"))
                .ty(FieldType::Str)
                .constraint(Constraint::Enum {
                    values: vec![Term::value("public"), Term::value("private")],
                    closed: true,
                }),
        ]);
        let backend = ProvBackend::open_with_schema("note.md", doc, schema).expect("open");
        let mut model = Model::new(backend).expect("model");

        // The schema traveled through the backend into the model: an unknown
        // term is rejected, the document untouched.
        select(&mut model, &[Seg::Key("audience".into()), Seg::Index(0)]);
        model.begin_edit();
        type_value(&mut model, "familly");
        assert!(
            model.status.contains("rejected"),
            "status: {}",
            model.status
        );
        assert!(model.source_snapshot().contains("- public"), "unchanged");

        // A known value commits.
        model.begin_edit();
        type_value(&mut model, "private");
        assert!(model.source_snapshot().contains("- private"), "applied");
    }
}
