//! [`DocumentSession`] — one open prov document, edited through two coordinated
//! editors.
//!
//! A prov document is a metadata region plus a prose body. The session owns:
//!
//! - a [`flower_core::Model<ProvBackend>`] for the **metadata** (structural,
//!   lossless, through prov's `MetaEditor`), and
//! - a [`leaf_core::Doc`] for the **body** (a rich-text editor over its own
//!   buffer).
//!
//! The two regions are independent — there are no shared byte offsets — so they
//! edit freely and reconcile only at [`save`](DocumentSession::save): the body's
//! current text is spliced back into the document (leaving the metadata edits in
//! place), and the reassembled bytes are written to disk.
//!
//! A workspace [`Schema`](flower_core::Schema) can be supplied at open
//! (`*_with_schema`) so the metadata model validates controlled fields and offers
//! pickers; without one the session behaves exactly as a schema-free editor.
//!
//! File I/O lives here (not in flower-core/leaf-core, which stay fs-agnostic). A
//! frontend that wants fixity and the `updated` stamp maintained routes the write
//! through prov's `Storage`/`mutate` layer instead; this foundation writes the
//! bytes directly, which is the unopinionated floor a frontend builds on.
//!
//! This is the surface a UniFFI facade will wrap, and the surface a TUI drives
//! directly.

use std::path::{Path, PathBuf};

use fig::Value;
use flower_core::{Model, Schema, Seg};
use leaf_core::{Doc, Format as BodyFormat};
use prov::{Document, MetaCarrier};

use crate::ProvBackend;

/// A session error, carrying a human-readable message. UniFFI-friendly to widen
/// into a typed enum later.
#[derive(Debug)]
pub struct SessionError(pub String);

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionError {}

fn se(e: impl std::fmt::Display) -> SessionError {
    SessionError(e.to_string())
}

/// One open prov document: a metadata editor and a body editor over the same
/// file, reconciled on save.
pub struct DocumentSession {
    path: PathBuf,
    metadata: Model<ProvBackend>,
    body: Doc,
    /// Whether the document has an editable prose body (a fenced carrier). A
    /// whole-file config document has none — its body editor stays empty.
    has_body: bool,
    /// The body text as of the last open/save, for dirty tracking.
    saved_body: String,
}

/// The grammar a document's body is written in, from its path.
///
/// leaf is grammar-agnostic and prov already knows which extensions mean what,
/// so the only thing missing was asking. A path prov does not recognize as
/// content falls back to Markdown — including a whole-file config document,
/// which has no body for the format to apply to.
fn body_format_of(path: &Path) -> BodyFormat {
    match prov::ContentFormat::from_extension(path) {
        Some(prov::ContentFormat::Djot) => BodyFormat::Djot,
        Some(prov::ContentFormat::Html) => BodyFormat::Html,
        Some(prov::ContentFormat::Markdown) | None => BodyFormat::Markdown,
    }
}

impl DocumentSession {
    /// Open a prov document from disk, parsing the body in the grammar its
    /// extension declares, with no schema.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let path = path.into();
        let format = body_format_of(&path);
        Self::open_with(path, format, None)
    }

    /// Open a prov document from disk carrying the workspace `schema`.
    pub fn open_with_schema(
        path: impl Into<PathBuf>,
        schema: Schema,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let format = body_format_of(&path);
        Self::open_with(path, format, Some(schema))
    }

    /// Open a prov document from disk, parsing the body as `body_format`, with an
    /// optional workspace schema.
    pub fn open_with(
        path: impl Into<PathBuf>,
        body_format: BodyFormat,
        schema: Option<Schema>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SessionError(format!("reading {}: {e}", path.display())))?;
        Self::from_text(path, &text, body_format, schema)
    }

    /// Build a session from in-memory `text`. The `path` still drives prov's
    /// carrier/format detection (extension for a config doc, content sniffing for a
    /// fenced block). `schema` governs the metadata model when present.
    pub fn from_text(
        path: impl Into<PathBuf>,
        text: &str,
        body_format: BodyFormat,
        schema: Option<Schema>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let parsed = Document::parse(&path, text).map_err(se)?;
        let has_body = matches!(parsed.carrier, Some(MetaCarrier::Fenced(_)));

        let backend = match schema {
            Some(schema) => ProvBackend::open_with_schema(&path, text, schema),
            None => ProvBackend::open(&path, text),
        }
        .map_err(se)?;
        let metadata = Model::new(backend).map_err(se)?;
        let body = Doc::from_source(parsed.body, body_format).map_err(se)?;
        let saved_body = body.source.clone();

        Ok(Self {
            path,
            metadata,
            body,
            has_body,
            saved_body,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the document has an editable prose body.
    pub fn has_body(&self) -> bool {
        self.has_body
    }

    /// The metadata editor (its `rows` are what a metadata pane renders).
    pub fn metadata(&self) -> &Model<ProvBackend> {
        &self.metadata
    }

    pub fn metadata_mut(&mut self) -> &mut Model<ProvBackend> {
        &mut self.metadata
    }

    /// The body editor.
    pub fn body(&self) -> &Doc {
        &self.body
    }

    pub fn body_mut(&mut self) -> &mut Doc {
        &mut self.body
    }

    /// Programmatically set the metadata value at `path` — the flat, by-path edit
    /// a UI/FFI issues (vs. driving the selection).
    pub fn set_metadata(&mut self, path: &[Seg], value: Value) {
        self.metadata.set_value_at(path, value);
    }

    /// `true` if the metadata or the body has unsaved edits.
    pub fn dirty(&self) -> bool {
        self.metadata.dirty || (self.has_body && self.body.source != self.saved_body)
    }

    /// Reconcile the body edits into the document and return the full reassembled
    /// text — exactly the bytes [`save`](Self::save) writes. Does not touch disk.
    pub fn reassemble(&mut self) -> Result<String, SessionError> {
        if self.has_body {
            let body = self.body.source.clone();
            self.metadata.backend_mut().set_body(&body).map_err(se)?;
        }
        Ok(self.metadata.source_snapshot())
    }

    /// Write the reassembled document (metadata edits + body edits) to disk.
    pub fn save(&mut self) -> Result<(), SessionError> {
        let full = self.reassemble()?;
        std::fs::write(&self.path, full.as_bytes())
            .map_err(|e| SessionError(format!("writing {}: {e}", self.path.display())))?;
        self.saved_body = self.body.source.clone();
        self.metadata.mark_saved();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
---
# the title
title: Old Title
draft: true
---
# Heading

Original body.
";

    fn preview_of(session: &DocumentSession, key: &str) -> Option<String> {
        session
            .metadata()
            .rows
            .iter()
            .find(|r| r.path == [Seg::Key(key.into())])
            .map(|r| r.preview.clone())
    }

    #[test]
    fn reassembles_both_edits_without_disk() {
        let mut session =
            DocumentSession::from_text("note.md", DOC, BodyFormat::Markdown, None).unwrap();
        assert!(session.has_body());
        assert!(!session.dirty());

        // Metadata edit (by path) + body edit (via leaf).
        session.set_metadata(&[Seg::Key("title".into())], Value::Str("New Title".into()));
        session.body_mut().insert("Edited: ");
        assert!(session.dirty());

        let out = session.reassemble().unwrap();
        assert!(out.contains("title: New Title"), "metadata edit:\n{out}");
        assert!(out.contains("# the title"), "frontmatter comment:\n{out}");
        assert!(out.contains("draft: true"), "sibling key:\n{out}");
        assert!(out.contains("Edited: "), "body edit:\n{out}");
        assert!(out.contains("Original body."), "rest of body:\n{out}");
        assert!(out.starts_with("---\n"), "fences:\n{out}");
    }

    /// The whole composition, end to end on disk: open → edit both regions →
    /// save → reopen, with comments, fences, untouched keys and untouched body
    /// all still there. The in-memory test above proves the splice; this one
    /// proves the bytes survive a round trip through the filesystem, which is
    /// the only place a lossless claim can actually be falsified.
    #[test]
    fn open_edit_save_reopen_round_trip_on_disk() {
        let path = std::env::temp_dir().join("provui_core_document_session_round_trip.md");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, DOC).unwrap();

        // Open, edit both regions, save.
        let mut session = DocumentSession::open(&path).unwrap();
        assert_eq!(preview_of(&session, "title").as_deref(), Some("Old Title"));
        session.set_metadata(&[Seg::Key("title".into())], Value::Str("New Title".into()));
        session.body_mut().insert("Edited: ");
        session.save().unwrap();
        assert!(!session.dirty(), "clean after save");

        // The bytes on disk carry both edits, everything else preserved.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("title: New Title"),
            "saved metadata:\n{saved}"
        );
        assert!(saved.contains("# the title"), "saved comment:\n{saved}");
        assert!(saved.contains("Edited: "), "saved body:\n{saved}");
        assert!(
            saved.contains("Original body."),
            "saved body rest:\n{saved}"
        );

        // Reopening parses cleanly and reflects both edits.
        let reopened = DocumentSession::open(&path).unwrap();
        assert_eq!(preview_of(&reopened, "title").as_deref(), Some("New Title"));
        assert!(reopened.body().source.contains("Edited: "));

        let _ = std::fs::remove_file(&path);
    }
}
