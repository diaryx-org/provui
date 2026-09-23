//! What the host owns: which pane has the keyboard, the status line, and the
//! preparation every document gets before it is drawn or typed into. The
//! document itself is the [`DocumentSession`] passed alongside.

use flower_core::ViewMode;
use leaf_ratatui::EditorState;
use provui_core::DocumentSession;
use ratatui::layout::Rect;

use crate::{nav, ui};

/// Which pane has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Body,
    Metadata,
}

impl Focus {
    pub fn name(self) -> &'static str {
        match self {
            Focus::Body => "body",
            Focus::Metadata => "metadata",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Focus::Body => Focus::Metadata,
            Focus::Metadata => Focus::Body,
        }
    }
}

/// Everything the host owns. The document is not in here — it is the
/// [`DocumentSession`] passed alongside, and this is only what is true of the
/// window looking at it.
pub struct App {
    pub focus: Focus,
    /// leaf's per-terminal state: graphics protocol support, colour scheme,
    /// prose width. Belongs to the host because it is a fact about the terminal,
    /// not about the document.
    pub editor: EditorState,
    /// The transient line under the panes: a save's result, or the honest
    /// refusal of something this host does not do.
    pub status: Option<String>,
    /// Set by a quit that was refused for unsaved changes; the next quit is
    /// taken at its word. Cleared by any other key, so it can only ever mean
    /// "you just asked, and I just said".
    pub quit_armed: bool,
    /// Set by a save that was refused because the file changed on disk; the
    /// next save overwrites it. Cleared by any other key, like `quit_armed`.
    pub overwrite_armed: bool,
    /// The file name, for the two headers and the status line.
    pub name: String,
    /// The workspace, the way back, and this host's arrangement policy.
    pub nav: nav::Nav,
    /// Where the panes landed on the last frame, so a click can be routed to the
    /// one it was in. `None` before the first draw.
    pub panes: Option<ui::Panes>,
}

impl App {
    pub fn new(session: &DocumentSession, nav: nav::Nav) -> Self {
        let status = nav.note();
        Self {
            // The prose is the document; the metadata is what is true about it.
            // A document with no prose region has nowhere else to put the
            // keyboard.
            focus: if session.has_body() {
                Focus::Body
            } else {
                Focus::Metadata
            },
            editor: EditorState::new(),
            // Only a complaint — prov refusing to guess a root. An absent
            // workspace is the ordinary case and is drawn, not announced.
            status,
            quit_armed: false,
            overwrite_armed: false,
            name: file_name(session.path()),
            nav,
            panes: None,
        }
    }
}

pub fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Whether the loop keeps going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// Ask the workspace what is wrong with the document and hand the answer to the
/// session, which washes the body half under the prose and holds the rest.
///
/// Run on open and after every save, and nowhere else. It is a walk from the
/// document — cheap for a note, proportional to the subtree for an index — so
/// it belongs at the two moments the document's structure actually changed, not
/// on a keystroke or a frame.
pub fn refresh_findings(session: &mut DocumentSession, app: &mut App) {
    match app.nav.findings(session.path()) {
        Ok(findings) => session.apply_findings(&findings),
        // A check that cannot run is not a document that cannot be edited.
        // Clearing first so a stale wash from the last document never outlives
        // the answer it came from.
        Err(e) => {
            session.apply_findings(&[]);
            app.status = Some(format!("check failed: {e}"));
        }
    }
}

/// Put the metadata model in the shape the widget draws before anything draws or
/// types. Shared with the tests, which drive the same keys without a terminal.
pub fn begin(session: &mut DocumentSession, app: &App, screen: Rect) {
    // The widget draws the page projection and routes edits against its cursor,
    // so the model has to be in it before the first frame or the first key.
    session.metadata_mut().set_view(ViewMode::Pages);
    fit_metadata(screen, session, app);
    // Skips a lone drill row, and must run once the inline budget is known.
    session.metadata_mut().enter_document();
}

pub fn fit_metadata(area: Rect, session: &mut DocumentSession, app: &App) {
    if let Some(metadata) = ui::layout(area, app.focus, session.has_body()).metadata {
        session
            .metadata_mut()
            .fit_to_room(flower_ratatui::page_room(metadata.height));
    }
}
