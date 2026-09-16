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
//! ## One undo over two histories
//!
//! Each editor keeps its own history and neither knows the other exists, so the
//! session keeps a **journal of which one took each step** — nothing more. A
//! host calls [`sync_history`](DocumentSession::sync_history) once per event
//! loop, which reads both editors' change counters and records whichever moved;
//! [`undo`](DocumentSession::undo) pops the most recent entry and calls that
//! editor's own undo. So "body edit, metadata edit, body edit" undoes in that
//! order, and the *meaning* of a step stays with the editor that owns the bytes.
//!
//! A workspace-maintained key still refuses its undo, because flower replays the
//! inverse through the same backend the edit went through. And leaf still
//! decides its own step boundaries: see `sync_history` for the one limit that
//! follows from that.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fig::Value;
use flower_core::annotate;
use flower_core::{Annotation, Choice, Model, Schema, Seg, ViewMode};
use leaf_core::{Doc, Format as BodyFormat};
use prov::{Document, MetaCarrier};

use crate::ProvBackend;
use crate::body_links::BodyLink;
use crate::findings::{Finding, Severity, Site};

/// The answer for a document whose metadata block does not resolve — a `&Value`
/// to hand back without an allocation or an `Option` every caller would unwrap
/// the same way.
static EMPTY_META: Value = Value::Null;

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

/// A heading in a document's prose body.
///
/// The piece of a document a `#locator` names: prov carries a locator on a link
/// target and never resolves it, leaf's [`Doc::locate`](leaf_core::Doc::locate)
/// resolves one it is given, and this is the third corner — the locator you
/// would *write* for where the caret is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// The heading's own words, without its `#` marker.
    pub text: String,
    /// `1` for an `# H1`, `6` for an `###### H6`.
    pub level: u32,
    /// Byte range of the whole heading line within the **body text** — the same
    /// coordinates [`crate::BodyLink::span`] and the caret are in.
    pub span: std::ops::Range<usize>,
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
    /// The findings most recently handed to
    /// [`apply_findings`](DocumentSession::apply_findings). Kept because the
    /// body half of them becomes leaf highlights the widget draws by itself,
    /// while the metadata half has nowhere to go yet — see
    /// [`meta_findings`](DocumentSession::meta_findings).
    findings: Vec<Finding>,
    /// Which editor took each step, oldest first — the order
    /// [`undo`](DocumentSession::undo) walks back through. See
    /// [`sync_history`](DocumentSession::sync_history).
    journal: Vec<Region>,
    /// The steps [`undo`](DocumentSession::undo) has taken back, most recent
    /// last. Cleared by the next fresh edit in either region.
    redo_journal: Vec<Region>,
    /// leaf's [`revision`](leaf_core::Doc::revision) as of the last
    /// [`sync_history`](DocumentSession::sync_history).
    seen_body: u64,
    /// flower's [`edit_seq`](flower_core::Model::edit_seq) as of the last
    /// [`sync_history`](DocumentSession::sync_history).
    seen_meta: u64,
}

/// Which of a session's two editors a history step belongs to.
///
/// The whole of the session's journal: an ordered list of these is what makes
/// one undo out of two independent histories, and neither editor learns that
/// the other exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// The prose body — a [`leaf_core::Doc`] step.
    Body,
    /// The metadata — a [`flower_core::Model`] step.
    Meta,
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

/// The metadata half of a finding list, in flower's own vocabulary.
///
/// Split out from [`DocumentSession::apply_findings`] because it is the whole
/// of the translation, and a frontend composing its own annotation list wants
/// the prov half of it without the ownership.
pub fn annotations_of(findings: &[Finding]) -> Vec<Annotation> {
    findings
        .iter()
        .filter_map(|finding| {
            let path = match &finding.site {
                Site::Meta(path) => path.clone(),
                // The empty path is flower's "the document".
                Site::Document => Vec::new(),
                Site::Body(_) => return None,
            };
            let severity = match finding.severity {
                Severity::Error => annotate::Severity::Error,
                Severity::Warning => annotate::Severity::Warning,
            };
            Some(Annotation::new(path, severity, finding.message.clone()))
        })
        .collect()
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

    /// Open a prov document declaring the keys the **workspace** maintains, so
    /// the metadata model draws their rows and declines every edit to them.
    ///
    /// `derived` is
    /// [`Facets::managed_key_names`](crate::Facets::managed_key_names) — `id`,
    /// `content_hash`, and whatever the workspace named as its `updated` stamp.
    /// It is a separate entry point rather than something
    /// [`open_with_schema`](Self::open_with_schema) does for you because
    /// declining an edit is a *policy*, and a repair tool that means to rewrite a
    /// stale `id` is as legitimate a frontend as an editor that must not. This
    /// crate hands over the list and lets the frontend decide (see
    /// [`crate::facets`]); most editors want it, and this is the one line that
    /// says so.
    ///
    /// The set has to arrive here rather than being applied afterwards: flower
    /// takes it before it builds its first row list.
    pub fn open_managed(
        path: impl Into<PathBuf>,
        schema: Option<Schema>,
        derived: Vec<String>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let format = body_format_of(&path);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SessionError(format!("reading {}: {e}", path.display())))?;
        Self::build(path, &text, format, schema, derived)
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
        Self::build(path, text, body_format, schema, Vec::new())
    }

    /// The one constructor the rest are written in terms of.
    fn build(
        path: impl Into<PathBuf>,
        text: &str,
        body_format: BodyFormat,
        schema: Option<Schema>,
        derived: Vec<String>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let parsed = Document::parse(&path, text).map_err(se)?;
        let has_body = matches!(parsed.carrier, Some(MetaCarrier::Fenced(_)));

        let backend = match schema {
            Some(schema) => ProvBackend::open_with_schema(&path, text, schema),
            None => ProvBackend::open(&path, text),
        }
        .map_err(se)?;
        // `with_managed`, not `new`: a derived key keeps its row and declines
        // every edit, which is a different thing from hiding it. Empty `hidden`
        // — nothing about a prov document is a key this host should make
        // invisible, and a frontend that wants one says so itself.
        let metadata = Model::with_managed(backend, Vec::new(), derived).map_err(se)?;
        let body = Doc::from_source(parsed.body, body_format).map_err(se)?;
        let saved_body = body.source.clone();

        Ok(Self {
            seen_body: body.revision(),
            seen_meta: metadata.edit_seq(),
            path,
            metadata,
            body,
            has_body,
            saved_body,
            findings: Vec::new(),
            journal: Vec::new(),
            redo_journal: Vec::new(),
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

    /// The metadata value tree — what [`crate::links`] and [`crate::facets`] ask
    /// their questions of.
    ///
    /// The model's own copy, not a reparse: it is rebuilt on every edit, so this
    /// is current and free.
    pub fn meta(&self) -> &Value {
        self.metadata.value_at(&[]).unwrap_or(&EMPTY_META)
    }

    /// The metadata path the cursor is on, whichever projection the model is
    /// showing.
    ///
    /// flower has two — a flat row list and a page stack — and asks the question
    /// a different way in each. A frontend that wants "the row under the cursor"
    /// should not have to know which one it set, least of all a frontend that
    /// switches between them; getting it wrong reads as a link that follows the
    /// wrong document rather than as an error.
    pub fn cursor_path(&self) -> Option<Vec<Seg>> {
        match self.metadata.view() {
            ViewMode::Pages => self.metadata.page_item().map(|item| item.path.clone()),
            _ => self.metadata.selected_path(),
        }
    }

    /// The body editor.
    pub fn body(&self) -> &Doc {
        &self.body
    }

    /// The grammar the body is written in, as **prov** spells it.
    ///
    /// leaf's `Format` is twig's, which is the wider list — it also names XML
    /// and AsciiDoc, which prov has no content format for. Anything outside
    /// prov's three reads as Markdown, which is the same fallback
    /// [`DocumentSession::open`] applies on the way in, so the answer here is
    /// the format the body was actually parsed under rather than a second
    /// guess at it.
    pub fn body_format(&self) -> prov::ContentFormat {
        match self.body.format {
            BodyFormat::Djot => prov::ContentFormat::Djot,
            BodyFormat::Html => prov::ContentFormat::Html,
            _ => prov::ContentFormat::Markdown,
        }
    }

    /// Every link the prose body declares, as it stands.
    ///
    /// Parsed on each call rather than cached: the body is a live buffer, and a
    /// cached span list is one edit away from pointing at the wrong bytes.
    /// Following a link is a keystroke, not a frame, so one twig parse of one
    /// document's prose is the right price for an answer that is never stale.
    pub fn body_links(&self) -> Result<Vec<BodyLink>, SessionError> {
        crate::body_links::body_links(&self.body.source, self.body_format())
    }

    /// The body link the caret is standing inside, if any — the body pane's
    /// half of "the row under the cursor, is that a link?".
    ///
    /// leaf keeps the caret as a byte offset into the same buffer the spans are
    /// measured in ([`leaf_core::Doc::caret`]), so the two meet without a
    /// conversion.
    pub fn body_link_at_caret(&self) -> Result<Option<BodyLink>, SessionError> {
        let links = self.body_links()?;
        Ok(crate::body_links::body_link_at(&links, self.body.caret).cloned())
    }

    pub fn body_mut(&mut self) -> &mut Doc {
        &mut self.body
    }

    /// The heading the caret is under — the nearest one at or above it, and
    /// `None` when the caret sits above the document's first heading (or there
    /// are none).
    ///
    /// "At or above" is the rule every table of contents and every anchor
    /// implementation uses: a caret three paragraphs into a section is in that
    /// section, and the heading that opened it is the thing a reader would name
    /// to point at where they are.
    ///
    /// Parsed through twig directly — `prov::twig` is the same copy prov and
    /// leaf are both built on, so this is the tree leaf is already holding
    /// rather than a second one with its own opinions. It is parsed again here
    /// because leaf's own `Doc::nodes` is private: `Doc` exposes `locate` (a
    /// fragment to a landing) and `link_destination_at_caret` (a caret to a
    /// link) but nothing that hands back the node array, and nothing that
    /// answers the caret-to-heading question. The parse is one document's prose
    /// on a keystroke, which is the same price [`body_links`](Self::body_links)
    /// pays and for the same reason.
    pub fn heading_at_caret(&self) -> Option<Heading> {
        use prov::twig;

        let mut parsed = twig::Document::parse_str(&self.body.source, self.body.format).ok()?;
        let nodes = parsed.nodes().ok()?;
        let caret = self.body.caret;
        let node = nodes
            .iter()
            .filter(|n| n.kind == twig::Kind::Heading)
            .filter(|n| n.span.start <= caret)
            .max_by_key(|n| n.span.start)?;
        // `content_span` is the words without the `#` marker; `text` is twig's
        // own flattening of the same, and is what a heading with inline marks
        // in it (`## The *hard* part`) reads as. Prefer the source slice, so
        // the locator is derived from what is actually written.
        let text = node
            .content_span
            .clone()
            .and_then(|span| self.body.source.get(span))
            .map(str::to_string)
            .or_else(|| node.text.clone())?;
        Some(Heading {
            text: text.trim().to_string(),
            level: node.level.unwrap_or(1),
            span: node.span.clone(),
        })
    }

    /// The `#locator` naming where the caret is — [`prov::link::slug`] of the
    /// heading above it.
    ///
    /// prov's slug rather than a local one, because prov is what has to read it
    /// back: the fragment this writes is the fragment `prov check` resolves and
    /// the fragment leaf's `Doc::locate` lands, and `locate`'s third reading —
    /// a heading's own words, slugged — is the one that applies to Markdown,
    /// where there are no ids to name at all.
    pub fn locator_at_caret(&self) -> Option<String> {
        self.heading_at_caret()
            .map(|heading| prov::link::slug(&heading.text))
    }

    /// Hand the metadata backend the candidate lists a reference field's picker
    /// should offer, per relation — see
    /// [`ProvBackend::set_candidates`](crate::ProvBackend::set_candidates) for
    /// what it costs and
    /// [`WorkspaceView::candidates_map`](crate::WorkspaceView::candidates_map)
    /// for where a list comes from.
    ///
    /// Through the session rather than through `metadata_mut().backend_mut()`
    /// because it is the same kind of out-of-band fact as the schema and the
    /// findings: something only a host with a workspace can know, handed to the
    /// one document that cannot work it out.
    pub fn set_candidates(&mut self, candidates: HashMap<String, Vec<Choice>>) {
        self.metadata.backend_mut().set_candidates(candidates);
    }

    /// Programmatically set the metadata value at `path` — the flat, by-path edit
    /// a UI/FFI issues (vs. driving the selection).
    pub fn set_metadata(&mut self, path: &[Seg], value: Value) {
        self.metadata.set_value_at(path, value);
    }

    /// Take on a set of findings: wash the body ones under the text they are
    /// about, and hold the rest for the host to read.
    ///
    /// **The highlight list is owned by this call.** leaf's
    /// [`set_highlights`](leaf_core::Doc::set_highlights) replaces the whole
    /// set rather than adding to it — deliberately, so the host and the
    /// document can never disagree about what is on screen — so there is no way
    /// to "clear the finding highlights and keep the others". A session whose
    /// findings are being applied is a session whose body highlights are the
    /// findings; a host that also wants search hits or annotations in the body
    /// composes its own list and calls leaf directly instead of calling this.
    ///
    /// Each highlight's `id` is the finding's [`kind`](Finding::kind), which is
    /// what leaf hands back when a reader activates one, and its `marker` is
    /// `"finding"` — the name is opaque to leaf, and a frontend reads it as
    /// whatever glyph it draws in the margin.
    ///
    /// **The metadata half is owned the same way**, and by the same argument:
    /// flower's [`set_annotations`](flower_core::Model::set_annotations)
    /// replaces the whole set rather than adding to it, so the rows a session's
    /// findings are applied to carry those findings and nothing else. A host
    /// with annotations of its own composes the list and calls the model
    /// directly.
    ///
    /// A [`Site::Meta`] finding becomes an [`Annotation`](flower_core::Annotation)
    /// at the same path — so the row `contents[2]` was narrowed to is the row
    /// that gets the marker — and a [`Site::Document`] one becomes an annotation
    /// at the **empty** path, which is flower's spelling for "the document".
    /// That is deliberately not a row: nothing draws the root, so a finding
    /// about the file rather than about anything written in it stays the host's
    /// to report, which is what [`findings`](Self::findings) is for.
    /// [`Site::Body`] findings go to leaf and nowhere else.
    ///
    /// The severity map is total in one direction only: this crate draws two
    /// levels and flower draws three, so nothing here ever produces
    /// [`Severity::Info`](flower_core::annotate::Severity::Info). prov has no
    /// severity at all (see [`crate::findings`]), and inventing a third here
    /// would be inventing it twice.
    pub fn apply_findings(&mut self, findings: &[Finding]) {
        let highlights = findings
            .iter()
            .filter_map(|finding| match &finding.site {
                Site::Body(span) => Some(leaf_core::Highlight {
                    start: span.start,
                    end: span.end,
                    id: finding.kind.to_string(),
                    color: None,
                    marker: Some("finding".to_string()),
                }),
                _ => None,
            })
            .collect();
        self.body.set_highlights(highlights);
        self.metadata.set_annotations(annotations_of(findings));
        self.findings = findings.to_vec();
    }

    /// Every finding [`apply_findings`](Self::apply_findings) was last given.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// The findings that sit in the **metadata**.
    ///
    /// [`apply_findings`](Self::apply_findings) has already handed these to the
    /// model as annotations, so a widget over it draws them; this is the same
    /// half as prov reported it, for a host that wants the `kind` or the
    /// severity rather than the sentence.
    pub fn meta_findings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| matches!(f.site, Site::Meta(_)))
    }

    /// The finding sitting at metadata `path`, if there is one.
    ///
    /// Exact, not inherited: a finding on `contents` does not answer for
    /// `contents[2]`. flower's
    /// [`annotation_at`](flower_core::Model::annotation_at) is the other
    /// question and inherits from the nearest annotated ancestor.
    pub fn meta_finding_at(&self, path: &[Seg]) -> Option<&Finding> {
        self.meta_findings()
            .find(|f| matches!(&f.site, Site::Meta(at) if at == path))
    }

    // ── one history over two editors ─────────────────────────────────────

    /// Notice whatever either editor has just done, and record which one did
    /// it. **The host calls this once per event-loop iteration**, after
    /// dispatching the event and before reading the next.
    ///
    /// ## Why it is polled rather than pushed
    ///
    /// Neither editor has an edit *entry point* the session could wrap. A
    /// keystroke reaches leaf through `leaf_ratatui::handle_key` and flower
    /// through `flower_ratatui::handle_key`, both of which take the editor
    /// directly, and a host holding [`body_mut`](Self::body_mut) and
    /// [`metadata_mut`](Self::metadata_mut) can edit through either without
    /// passing through anything of this crate's. What both editors *do* expose
    /// is a counter that moves on every change and on nothing else —
    /// [`Doc::revision`](leaf_core::Doc::revision) and
    /// [`Model::edit_seq`](flower_core::Model::edit_seq) — so the session reads
    /// those instead of asking the host to remember to tell it. A host that
    /// forgets to call this loses undo; it cannot get the *order* wrong, which
    /// is the failure worth designing against.
    ///
    /// ## The known limit
    ///
    /// **leaf coalesces keystrokes into steps on its own schedule.** Typing a
    /// word moves the revision once per character, and twig may hold the whole
    /// word as a single undo step. So a [`Region::Body`] journal entry is not a
    /// leaf step, and a count of entries is not a count of undos: what
    /// [`undo`](Self::undo) does is take **one leaf step**, never a keystroke,
    /// and then drop whatever further `Body` entries leaf has nothing left to
    /// answer for before it reaches the next `Meta` one. That is what keeps the
    /// *ordering* exact — body, then metadata, then body undoes in that order —
    /// while leaving the granularity to the editor that owns the bytes, which
    /// is the only component that can decide it.
    ///
    /// flower has no such coalescing: one commit is one step.
    ///
    /// A fresh edit in either region clears the redo journal, the way a fresh
    /// edit clears either editor's own.
    pub fn sync_history(&mut self) {
        let revision = self.body.revision();
        if revision != self.seen_body {
            self.seen_body = revision;
            self.journal.push(Region::Body);
            self.redo_journal.clear();
        }
        let seq = self.metadata.edit_seq();
        if seq != self.seen_meta {
            self.seen_meta = seq;
            self.journal.push(Region::Meta);
            self.redo_journal.clear();
        }
    }

    /// The steps recorded so far, oldest first — what
    /// [`sync_history`](Self::sync_history) has seen. For a frontend drawing a
    /// history, and for a test asserting the order.
    pub fn journal(&self) -> &[Region] {
        &self.journal
    }

    /// Whether there is a step to take back. See [`undo`](Self::undo) for why
    /// this is not `!journal().is_empty()`.
    ///
    /// A hint, in the direction hints should err: it can say yes where the body
    /// entries left are all coalesced away, because leaf's own `can_undo` is a
    /// step counter rather than its history — see [`undo`](Self::undo). It
    /// never says no while there is something to take back, which is the half a
    /// greyed-out menu item needs to be right about.
    pub fn can_undo(&self) -> bool {
        self.journal.iter().rev().any(|r| self.has_undo(*r))
    }

    /// Whether there is an undone step to put back.
    pub fn can_redo(&self) -> bool {
        self.redo_journal.iter().rev().any(|r| self.has_redo(*r))
    }

    /// Take back the most recent step, in whichever editor made it.
    ///
    /// The journal says which editor, and that editor's own undo says what —
    /// `Doc::undo` for the body, `Model::undo` for the metadata. Neither is
    /// reimplemented here and neither is second-guessed: flower replays an
    /// inverse op through the same `Backend::apply` the edit went through, so a
    /// workspace-maintained key refuses its undo exactly as it refuses its
    /// edit, and a refusal here is a refusal that leaves the journal as it was.
    ///
    /// **Entries leaf has nothing to answer for are dropped, not pressed.**
    /// Because leaf coalesces (see [`sync_history`](Self::sync_history)), eight
    /// `Body` entries may face one leaf step: the first `undo` spends the step
    /// and the next one walks past the remaining seven to the `Meta` entry
    /// underneath. Without that, a reader would press the key seven times for
    /// nothing before the metadata edit came back.
    ///
    /// `true` when something was undone.
    pub fn undo(&mut self) -> bool {
        while let Some(region) = self.journal.pop() {
            if !self.has_undo(region) {
                continue;
            }
            let before = self.counters();
            // flower's undo says whether the document moved; leaf's does not.
            // The counters below answer that for both, so neither is asked.
            match region {
                Region::Body => self.body.undo(),
                Region::Meta => {
                    self.metadata.undo();
                }
            }
            if self.counters() != before {
                self.mark_seen();
                self.redo_journal.push(region);
                return true;
            }
            if self.nothing_happened(region) {
                return false;
            }
        }
        false
    }

    /// What a step that changed nothing means, which is not the same thing in
    /// the two editors — and `true` when it means the caller should stop.
    ///
    /// **flower's `history_len`/`redo_len` are its actual journals**, so a
    /// history move that changes nothing there is a *refusal*: a
    /// workspace-maintained key declining its own undo, which flower reports in
    /// its status. A refusal has to stop the walk. Reaching past it for an
    /// older edit would undo something the reader did not ask about, in answer
    /// to a key press that was answered "no".
    ///
    /// **leaf's `can_undo`/`can_redo` are step *counters*,** incremented once
    /// per edit where twig coalesces several into one step — so they can say
    /// yes when there is nothing left. A body move that changes nothing is
    /// therefore an exhausted run rather than a refusal, and the walk carries on
    /// to the next entry, which is what keeps a metadata edit from being
    /// stranded behind a word someone typed. (A genuinely read-only body would
    /// read the same way, and correctly: there is nothing there to take back.)
    fn nothing_happened(&mut self, region: Region) -> bool {
        match region {
            Region::Body => {
                self.mark_seen();
                false
            }
            Region::Meta => {
                self.journal.push(region);
                true
            }
        }
    }

    /// Put back the most recently undone step, in the editor that made it — the
    /// mirror of [`undo`](Self::undo), exhaustion-skipping and refusals
    /// included.
    ///
    /// `true` when something was redone.
    pub fn redo(&mut self) -> bool {
        while let Some(region) = self.redo_journal.pop() {
            if !self.has_redo(region) {
                continue;
            }
            let before = self.counters();
            match region {
                Region::Body => self.body.redo(),
                Region::Meta => {
                    self.metadata.redo();
                }
            }
            if self.counters() != before {
                self.mark_seen();
                self.journal.push(region);
                return true;
            }
            match region {
                Region::Body => self.mark_seen(),
                Region::Meta => {
                    self.redo_journal.push(region);
                    return false;
                }
            }
        }
        false
    }

    /// Whether `region`'s editor has a step to take back.
    fn has_undo(&self, region: Region) -> bool {
        match region {
            Region::Body => self.has_body && self.body.can_undo(),
            Region::Meta => self.metadata.history_len() > 0,
        }
    }

    /// Whether `region`'s editor has an undone step to put back.
    fn has_redo(&self, region: Region) -> bool {
        match region {
            Region::Body => self.has_body && self.body.can_redo(),
            Region::Meta => self.metadata.redo_len() > 0,
        }
    }

    /// Both editors' change counters, for telling a step that happened from one
    /// that was declined.
    fn counters(&self) -> (u64, u64) {
        (self.body.revision(), self.metadata.edit_seq())
    }

    /// Take the counters as read without journalling — what an undo or a redo
    /// does, since both editors count their own history moves as changes and a
    /// step back is not a new step.
    fn mark_seen(&mut self) {
        let (body, meta) = self.counters();
        self.seen_body = body;
        self.seen_meta = meta;
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

    /// A key the workspace maintains keeps its row and refuses the edit — the
    /// difference between "not shown" and "not yours to type".
    #[test]
    fn a_managed_key_is_drawn_and_declines_every_edit() {
        const WITH_ID: &str = "---\ntitle: A Note\nid: ajp7eq\nmood: rainy\n---\n# Note\n";
        let path = std::env::temp_dir().join("provui_core_session_managed.md");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, WITH_ID).unwrap();

        let facets = crate::Facets::default();
        let mut session =
            DocumentSession::open_managed(&path, None, facets.managed_key_names()).unwrap();

        // Drawn: the row is there, with its value.
        assert_eq!(preview_of(&session, "id").as_deref(), Some("ajp7eq"));
        assert!(session.metadata().is_derived(&[Seg::Key("id".into())]));

        // And declined: the document is untouched and still clean.
        session.set_metadata(&[Seg::Key("id".into())], Value::Str("typed".into()));
        assert!(!session.dirty(), "a derived key takes no edit");
        assert_eq!(preview_of(&session, "id").as_deref(), Some("ajp7eq"));

        // An ordinary key beside it is unaffected.
        session.set_metadata(&[Seg::Key("mood".into())], Value::Str("clear".into()));
        assert!(session.dirty());

        let _ = std::fs::remove_file(&path);
    }

    /// The heading above the caret, and the locator it names — the three
    /// positions a caret can be in relative to a document's headings.
    #[test]
    fn the_heading_above_the_caret_is_what_a_locator_names() {
        const PROSE: &str = "\
---
title: Notes
---
Preamble, above everything.

# Crash Safety

Why the journal is written first.

## The *hard* part

And what it costs.
";
        let mut session =
            DocumentSession::from_text("notes.md", PROSE, BodyFormat::Markdown, None).unwrap();
        let body = session.body().source.clone();
        let at = |needle: &str| body.find(needle).expect(needle);

        // Above the first heading there is nothing to name — not the document's
        // title, which is not a place in the prose.
        session.body_mut().caret = at("Preamble");
        assert_eq!(session.heading_at_caret(), None);
        assert_eq!(session.locator_at_caret(), None);

        // Inside a section: the heading that opened it, not the nearest one in
        // either direction.
        session.body_mut().caret = at("Why the journal");
        let heading = session.heading_at_caret().expect("under a heading");
        assert_eq!(heading.text, "Crash Safety");
        assert_eq!(heading.level, 1);
        assert_eq!(&body[heading.span.clone()], "# Crash Safety");
        assert_eq!(session.locator_at_caret().as_deref(), Some("crash-safety"));

        // Deeper in, under the sub-heading — and its inline emphasis is part of
        // the words, so the slug drops the markup the way prov's does.
        session.body_mut().caret = at("And what it costs");
        let heading = session.heading_at_caret().expect("under a heading");
        assert_eq!(heading.level, 2);
        assert_eq!(session.locator_at_caret().as_deref(), Some("the-hard-part"));
    }

    /// One undo across two editors, in the order the edits were actually made.
    ///
    /// The composition's real claim: leaf and flower each keep their own
    /// history and neither knows the other exists, so a reader pressing undo
    /// three times should walk back through body, metadata, body — not through
    /// one editor's history and then the other's.
    #[test]
    fn one_undo_walks_back_through_both_editors_in_order() {
        const DOC: &str = "---\ntitle: Old Title\n---\n# Heading\n\nOriginal body.\n";
        let mut session =
            DocumentSession::from_text("note.md", DOC, BodyFormat::Markdown, None).unwrap();
        let title = [Seg::Key("title".into())];
        let meta_value = |s: &DocumentSession| {
            s.meta()
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        assert!(!session.can_undo(), "nothing has happened yet");

        // Body, metadata, body — each followed by the sync a host does once
        // per event-loop iteration.
        session.body_mut().caret = 0;
        session.body_mut().insert("first ");
        session.sync_history();
        session.set_metadata(&title, Value::Str("New Title".into()));
        session.sync_history();
        session.body_mut().insert("second ");
        session.sync_history();

        assert_eq!(
            session.journal(),
            [Region::Body, Region::Meta, Region::Body],
            "who took each step"
        );
        assert!(session.can_undo());
        assert!(!session.can_redo());
        assert!(session.body().source.contains("second "));
        assert_eq!(meta_value(&session).as_deref(), Some("New Title"));

        // Back through them, newest first. Each step names one editor, and the
        // other is untouched by it.
        assert!(session.undo(), "the second body edit");
        assert!(!session.body().source.contains("second "));
        assert!(session.body().source.contains("first "));
        assert_eq!(
            meta_value(&session).as_deref(),
            Some("New Title"),
            "the metadata edit is not what was undone"
        );

        assert!(session.undo(), "the metadata edit");
        assert_eq!(meta_value(&session).as_deref(), Some("Old Title"));
        assert!(
            session.body().source.contains("first "),
            "and the body is where the last undo left it"
        );

        assert!(session.undo(), "the first body edit");
        assert!(!session.body().source.contains("first "));
        assert_eq!(meta_value(&session).as_deref(), Some("Old Title"));

        assert!(!session.can_undo(), "back at the document that was opened");
        assert!(!session.undo());

        // And forward again, in the order they were made.
        assert!(session.can_redo());
        assert!(session.redo());
        assert!(session.body().source.contains("first "));
        assert_eq!(meta_value(&session).as_deref(), Some("Old Title"));

        assert!(session.redo());
        assert_eq!(meta_value(&session).as_deref(), Some("New Title"));

        assert!(session.redo());
        assert!(session.body().source.contains("second "));
        assert!(!session.can_redo());

        // A fresh edit closes the redo journal, the way it closes either
        // editor's own.
        session.undo();
        assert!(session.can_redo());
        session.set_metadata(&title, Value::Str("A Third Title".into()));
        session.sync_history();
        assert!(!session.can_redo(), "the branch that was not taken is gone");
    }

    /// leaf decides its own step boundaries, so a run of typing is some number
    /// of journal entries and some smaller number of leaf steps. The session
    /// undoes *one leaf step* and then walks past the entries leaf has nothing
    /// left to answer for, rather than making a reader press the key once per
    /// character for nothing.
    #[test]
    fn a_coalesced_run_of_typing_does_not_cost_a_keypress_per_character() {
        const DOC: &str = "---\ntitle: Old Title\n---\n# Heading\n\nOriginal body.\n";
        let mut session =
            DocumentSession::from_text("note.md", DOC, BodyFormat::Markdown, None).unwrap();
        session.set_metadata(&[Seg::Key("title".into())], Value::Str("New Title".into()));
        session.sync_history();

        session.body_mut().caret = 0;
        for c in "typed".chars() {
            session.body_mut().insert(&c.to_string());
            session.sync_history();
        }
        assert_eq!(
            session
                .journal()
                .iter()
                .filter(|r| **r == Region::Body)
                .count(),
            5,
            "one entry per character, because one revision per character"
        );

        // However many leaf steps those five characters became, walking the
        // body back to where it started and then once more reaches the metadata
        // edit — it is never stranded behind the run.
        let mut presses = 0;
        while session.body().source.starts_with("typed") {
            assert!(session.undo(), "still something to take back");
            presses += 1;
            assert!(presses <= 5, "no more presses than there were characters");
        }
        eprintln!(
            "JOURNAL {:?} hist={} presses={} body={:?}",
            session.journal(),
            session.metadata().history_len(),
            presses,
            &session.body().source[..20.min(session.body().source.len())]
        );
        assert!(session.undo(), "and the metadata edit is next, not buried");
        assert_eq!(
            session.meta().get("title").and_then(|v| v.as_str()),
            Some("Old Title")
        );
        assert!(!session.can_undo());
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
