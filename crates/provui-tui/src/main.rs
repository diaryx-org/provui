//! `provui` — one prov document, two editors, one window.
//!
//! The prose body is edited by [`leaf_ratatui`] and the frontmatter by
//! [`flower_ratatui`], both of them embedded widgets that render into a `Rect`
//! and hand back an `Outcome` naming what the host must do. Underneath them
//! both is a single [`DocumentSession`], which owns the document and is the only
//! thing that writes it: a save from *either* pane writes *both* regions,
//! because the document is what is saved, not the pane you were standing in.
//!
//! What this binary is for is being the first frontend over `provui-core` — the
//! cheapest possible test of whether that composition survives a real event
//! loop. So it is deliberately thin. It owns the terminal, the split, the focus,
//! and the status line; every edit, every caret, every page of metadata belongs
//! to a widget, and the reconciling save belongs to the session.
//!
//! ## Focus
//!
//! Exactly one pane has the keyboard, and [`FOCUS_CHORD`] switches it. Both
//! widgets assume no host overlay is capturing input and both are greedy in ways
//! that make interception non-optional: leaf swallows *every* Ctrl and Alt chord
//! (returning `Continue` whether or not it is bound, so a host cannot detect a
//! free one from the return value), and flower reads `key.code` while ignoring
//! modifiers entirely (so `^X` would reach it as `x`, which deletes). Hence
//! `^W`, taken before either widget sees the event: unbound in leaf's Ctrl
//! table, not a bare letter for flower to navigate on, and `^W` is the window
//! key in every editor that has windows.
//!
//! A click is the one focus gesture that needs no chord: it moves the keyboard
//! to the pane it landed in. The widgets differ in what they do with the rest
//! of the mouse. leaf takes the event itself — caret placement, drag-select,
//! the wheel — and the host forwards it. flower takes no mouse events at all,
//! so the host maps the click back onto the row it landed on ([`ui::metadata_hit`])
//! and drives the model with the page vocabulary flower's own keys use: a click
//! stands on a row, a second click on that row is Enter, the wheel is `j`/`k`,
//! and in flower's two-pane view a click in the other pane goes where that pane
//! points — out to the parent's row, or into the previewed page.
//!
//! ## Following links
//!
//! A prov document's frontmatter is not only values: some of its keys are
//! *links*, and a workspace is what makes them resolvable. [`FOLLOW_CHORD`]
//! opens the document the metadata cursor is standing on and [`BACK_CHORD`]
//! returns, which makes this a two-key browser over the spanning tree. Both are
//! taken before the widgets for the reason `^W` is, and both are free in leaf's
//! Ctrl table — `^G` for *go*, `^O` for the jump-back every vi has.
//!
//! Everything about *what* a link is belongs to `provui-core`; everything about
//! what this host does with the answer belongs to [`nav`]. See that module for
//! the arrangement policy — which keys decline edits, and which rows sink.

mod nav;
mod ui;

use std::io::stdout;
use std::path::PathBuf;

use anyhow::{Context, Result};
use flower_core::{Mode, Model, ViewMode};
use leaf_ratatui::EditorState;
use provui_core::{DocumentSession, ProvBackend};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};

/// How the focus-switch chord is written for a reader. See the module docs for
/// why it is this one.
pub const FOCUS_CHORD: &str = "^W";

/// Open the document the metadata cursor's link points at. Free in leaf's Ctrl
/// table, and *go* is what it does.
pub const FOLLOW_CHORD: &str = "^G";

/// Back to the document the last follow came from — vi's jump-back chord, and
/// likewise free in leaf's table.
pub const BACK_CHORD: &str = "^O";

/// Which pane has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Body,
    Metadata,
}

impl Focus {
    fn name(self) -> &'static str {
        match self {
            Focus::Body => "body",
            Focus::Metadata => "metadata",
        }
    }

    fn other(self) -> Self {
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
    quit_armed: bool,
    /// The file name, for the two headers and the status line.
    pub name: String,
    /// The workspace, the way back, and this host's arrangement policy.
    pub nav: nav::Nav,
    /// Where the panes landed on the last frame, so a click can be routed to the
    /// one it was in. `None` before the first draw.
    pub panes: Option<ui::Panes>,
}

impl App {
    fn new(session: &DocumentSession, nav: nav::Nav) -> Self {
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
            name: file_name(session.path()),
            nav,
            panes: None,
        }
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Whether the loop keeps going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

const USAGE: &str = "\
provui — edit one prov document: leaf over the body, flower over the frontmatter

usage: provui <file>

    <file>          a prov document (markdown/djot/html with frontmatter, or a
                    whole-file config document)

    -h, --help      this
    -V, --version   the version

keys:
    ^W              switch panes
    ^S              save the document (both regions, from either pane)
    ^G              follow the link under the metadata cursor
    ^O              back to the document you followed from
    ^Q              quit
    body pane       leaf's keys — see `leaf --help`
    metadata pane   j/k move · l/h in/out · e edit · x delete

mouse:
    click           the pane under the pointer takes the keyboard; in the
                    metadata pane the cursor stands on the row, and a second
                    click on that row opens it
    wheel           scrolls the body, walks the metadata page
";

fn main() -> Result<()> {
    let mut path: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("provui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ if arg.starts_with('-') => {
                anyhow::bail!("unknown option {arg}\n\n{USAGE}");
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => anyhow::bail!("provui opens one document at a time\n\n{USAGE}"),
        }
    }
    let Some(path) = path else {
        print!("{USAGE}");
        anyhow::bail!("no file given");
    };

    // Discovery first: the workspace is what supplies the schema a document is
    // opened under, so it has to be known before the document is opened rather
    // than bolted on afterwards.
    let nav = nav::Nav::discover(&path);
    let mut session = nav
        .open(&path)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("opening {}", path.display()))?;
    let mut app = App::new(&session, nav);

    let mut terminal = ratatui::init();
    // Mouse capture routes clicks to the pane they landed in and gives leaf its
    // caret placement and scrolling. Bracketed paste is what makes a terminal
    // paste arrive as one `Event::Paste` rather than as N keypresses — which is
    // N undo steps and N autoformat decisions in leaf's buffer.
    let _ = execute!(stdout(), EnableMouseCapture, EnableBracketedPaste);
    // leaf's two probes talk to the controlling tty and must run after raw mode.
    app.editor.query_graphics();
    app.editor.query_color_scheme();

    let result = run(&mut terminal, &mut session, &mut app);

    let _ = execute!(stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

/// The whole terminal as a `Rect`, which is the currency the layout deals in.
fn screen(terminal: &DefaultTerminal) -> Result<Rect> {
    let size = terminal.size().context("terminal size")?;
    Ok(Rect::new(0, 0, size.width, size.height))
}

/// Put the metadata model in the shape the widget draws before anything draws or
/// types. Shared with the tests, which drive the same keys without a terminal.
fn begin(session: &mut DocumentSession, app: &App, screen: Rect) {
    // The widget draws the page projection and routes edits against its cursor,
    // so the model has to be in it before the first frame or the first key.
    session.metadata_mut().set_view(ViewMode::Pages);
    fit_metadata(screen, session, app);
    // Skips a lone drill row, and must run once the inline budget is known.
    session.metadata_mut().enter_document();
}

fn run(terminal: &mut DefaultTerminal, session: &mut DocumentSession, app: &mut App) -> Result<()> {
    begin(session, app, screen(terminal)?);

    loop {
        // How much a page inlines is a fact about the room it has, and the room
        // is the *pane's*, not the terminal's. Cheap enough to redo every frame:
        // the model only rebuilds when the budget actually changes.
        fit_metadata(screen(terminal)?, session, app);
        terminal.draw(|f| ui::draw(f, app, session))?;

        match event::read()? {
            // Filtered on `Press`: Windows and the kitty protocol both send
            // releases, and a release that inserts a character types everything
            // twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if on_key(session, app, key, screen(terminal)?) == Flow::Quit {
                    return Ok(());
                }
            }
            Event::Mouse(mouse) => on_mouse(session, app, mouse),
            Event::Paste(text) => on_paste(session, app, &text),
            _ => {}
        }
    }
}

fn fit_metadata(area: Rect, session: &mut DocumentSession, app: &App) {
    if let Some(metadata) = ui::layout(area, app.focus, session.has_body()).metadata {
        session
            .metadata_mut()
            .fit_to_room(flower_ratatui::page_room(metadata.height));
    }
}

// ── input ────────────────────────────────────────────────────────────────────

/// Is this one of the host's chords? Taken before either widget sees the event —
/// see the module docs for why that is the only way it can work.
fn is_chord(key: KeyEvent, letter: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&letter))
}

fn on_key(session: &mut DocumentSession, app: &mut App, key: KeyEvent, screen: Rect) -> Flow {
    // A refusal is only ever an answer to the key that provoked it.
    let quit_armed = std::mem::take(&mut app.quit_armed);
    app.status = None;

    if is_chord(key, 'w') {
        switch_focus(session, app);
        return Flow::Continue;
    }
    if is_chord(key, 'g') {
        follow(session, app, screen);
        return Flow::Continue;
    }
    if is_chord(key, 'o') {
        go_back(session, app, screen);
        return Flow::Continue;
    }

    match app.focus {
        Focus::Metadata => match flower_ratatui::handle_key(session.metadata_mut(), key) {
            flower_ratatui::Outcome::Continue => Flow::Continue,
            flower_ratatui::Outcome::Save => {
                save(session, app);
                Flow::Continue
            }
            flower_ratatui::Outcome::Quit => quit(session, app, quit_armed),
        },
        Focus::Body => {
            let outcome = leaf_ratatui::handle_key(session.body_mut(), key, &mut app.editor);
            match outcome {
                leaf_ratatui::Outcome::Continue => Flow::Continue,
                leaf_ratatui::Outcome::Save => {
                    save(session, app);
                    Flow::Continue
                }
                leaf_ratatui::Outcome::Quit => quit(session, app, quit_armed),
                degraded => {
                    app.status = Some(unhandled(degraded).to_string());
                    Flow::Continue
                }
            }
        }
    }
}

fn switch_focus(session: &DocumentSession, app: &mut App) {
    if !session.has_body() {
        app.status = Some("this document has no prose body".into());
        return;
    }
    // Leaving mid-edit would strand a half-typed value in a pane that is no
    // longer taking keys, and flower's mode would still be `Editing` when you
    // came back. Cheaper to say so than to guess whether it was a commit or a
    // cancel.
    if matches!(session.metadata().mode, Mode::Editing { .. }) {
        app.status = Some("finish the metadata edit first — Enter commits, Esc cancels".into());
        return;
    }
    app.focus = app.focus.other();
}

/// The document is what a save writes: the metadata model's edits and the body
/// buffer's, reconciled and written as one file, from whichever pane asked.
fn save(session: &mut DocumentSession, app: &mut App) {
    // leaf's own `Doc::save`/`mark_saved` are deliberately not used: this body
    // is a *region* of a file rather than a file, the `Doc` has no path, and
    // dirtiness for the document as a whole is the session's answer to give.
    match session.save() {
        Ok(()) => app.status = Some(format!("saved {}", app.name)),
        Err(e) => app.status = Some(format!("save failed: {e}")),
    }
}

/// Open the document the metadata cursor's link points at.
///
/// The cursor is the metadata pane's, so this only means anything there — from
/// the body pane there is no row to be standing on, and saying so beats
/// following whatever the metadata pane happened to be left on.
fn follow(session: &mut DocumentSession, app: &mut App, screen: Rect) {
    if app.focus != Focus::Metadata {
        app.status = Some(format!(
            "following is the metadata pane's ({FOCUS_CHORD} to switch)"
        ));
        return;
    }
    let landing = match app.nav.follow(session) {
        nav::Follow::NoCursor => {
            app.status = Some("nothing under the cursor".into());
            return;
        }
        nav::Follow::NotALink => {
            app.status = Some("not a link — stand on a relation's target".into());
            return;
        }
        nav::Follow::Lands(_, landing) => landing,
    };
    // A link that does not land on a file on disk is not a failure to report as
    // one: an external URL, a place inside this document and a broken target are
    // all real answers, and the destination knows how to say which it is.
    let Some(target) = landing.openable().map(std::path::Path::to_path_buf) else {
        app.status = Some(landing.describe());
        return;
    };
    if !leaving_is_allowed(session, app) {
        return;
    }
    let from = session.path().to_path_buf();
    match app.nav.go(&from, &target) {
        Ok(arrived) => {
            *session = arrived;
            arrive(session, app, screen);
        }
        Err(e) => app.status = Some(format!("opening {}: {e}", target.display())),
    }
}

/// Back to the document the last follow came from.
fn go_back(session: &mut DocumentSession, app: &mut App, screen: Rect) {
    let Some(result) = app.nav.back() else {
        app.status = Some("nothing to go back to".into());
        return;
    };
    if !leaving_is_allowed(session, app) {
        return;
    }
    match result {
        Ok(arrived) => {
            *session = arrived;
            arrive(session, app, screen);
        }
        Err(e) => app.status = Some(format!("going back: {e}")),
    }
}

/// Whether this host will leave the document it is standing in.
///
/// It will not, while there are unsaved changes. Unlike quitting there is no
/// second-press escape hatch, and deliberately: quitting twice discards work you
/// were told about and meant to discard, whereas following a link is a *reading*
/// gesture, and losing an edit to it would be losing it to something nobody
/// thinks of as destructive.
fn leaving_is_allowed(session: &DocumentSession, app: &mut App) -> bool {
    if matches!(session.metadata().mode, Mode::Editing { .. }) {
        app.status = Some("finish the metadata edit first — Enter commits, Esc cancels".into());
        return false;
    }
    if session.dirty() {
        app.status = Some("unsaved changes — ^S to save before leaving".into());
        return false;
    }
    true
}

/// Take on a document that has just replaced the one being edited.
fn arrive(session: &mut DocumentSession, app: &mut App, screen: Rect) {
    app.name = file_name(session.path());
    // The prose is the document; one with no prose region has nowhere else to
    // put the keyboard.
    app.focus = if session.has_body() {
        Focus::Body
    } else {
        Focus::Metadata
    };
    app.quit_armed = false;
    // The same preparation the first document got: a fresh model is in the row
    // projection, not the page one, and has not skipped its lone drill row.
    begin(session, app, screen);
    app.status = Some(format!("{} · {BACK_CHORD} back", app.name));
}

fn quit(session: &DocumentSession, app: &mut App, armed: bool) -> Flow {
    if session.dirty() && !armed {
        app.quit_armed = true;
        app.status = Some("unsaved changes — ^S to save, quit again to discard".into());
        return Flow::Continue;
    }
    Flow::Quit
}

/// What to say about the parts of leaf's `Outcome` this host does not implement.
///
/// leaf's surface is a full editor's — dialogs, prompts, a command palette, a
/// system clipboard — and `leaf-tui` is where all of it is handled. This host is
/// the honest minimum: the three outcomes that are about the *document* are
/// wired to the session, and the rest say so in the status line rather than
/// being dropped on the floor, so a key that does nothing is at least a key that
/// admits it.
fn unhandled(outcome: leaf_ratatui::Outcome) -> &'static str {
    use leaf_ratatui::Outcome as O;
    match outcome {
        O::Continue | O::Save | O::Quit => "",
        O::Copy => "copy: not in provui — leaf-tui has the clipboard",
        O::Cut => "cut: not in provui — leaf-tui has the clipboard",
        O::Paste | O::PastePlain => "paste: use the terminal's own paste (bracketed paste is on)",
        O::SaveAs => "save-as: not in provui — it edits the file it opened",
        O::New => "new document: not in provui — it edits the file it opened",
        O::LinkPrompt => "link: not in provui — no prompt dialog here",
        O::LanguagePrompt => "code language: not in provui — no prompt dialog here",
        O::ImagePrompt | O::VideoPrompt | O::AudioPrompt => {
            "insert media: not in provui — no prompt dialog here"
        }
        O::Palette => "command palette: not in provui — leaf-tui has it",
        O::Find => "find: not in provui — leaf-tui has it",
        O::Replace => "replace: not in provui — leaf-tui has it",
        O::Help => "help: run `provui --help`, and `leaf --help` for the body keys",
    }
}

fn on_mouse(session: &mut DocumentSession, app: &mut App, mouse: MouseEvent) {
    let Some(panes) = app.panes else { return };
    let at = Position::new(mouse.column, mouse.row);

    // A click moves the keyboard to the pane it landed in — the one focus
    // gesture that needs no chord. Wheel events do not: scrolling a pane you are
    // not typing in is a reasonable thing to want.
    let pressed = matches!(mouse.kind, MouseEventKind::Down(_));

    if panes.metadata.is_some_and(|r| r.contains(at)) {
        let arriving = pressed && app.focus != Focus::Metadata;
        if arriving {
            app.status = None;
            switch_focus(session, app);
        }
        // A value that is open stays open, and the cursor under it stays put —
        // flower's own keys leave both alone while editing, and a click that
        // moved the cursor out from under a half-typed value would commit it to
        // the wrong row.
        if matches!(session.metadata().mode, Mode::Editing { .. }) {
            return;
        }
        let model = session.metadata_mut();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let metadata = panes.metadata.expect("contains(at) held");
                if let Some(hit) = ui::metadata_hit(metadata, model, at) {
                    click_metadata(model, hit, arriving);
                }
            }
            MouseEventKind::ScrollDown => model.page_move_down(),
            MouseEventKind::ScrollUp => model.page_move_up(),
            _ => {}
        }
        return;
    }

    // The body's label and the divider are the body pane's edge, and a click on
    // an edge is a click on the pane.
    let in_body = [panes.body, panes.body_label, panes.divider]
        .into_iter()
        .flatten()
        .any(|r| r.contains(at));
    if in_body {
        if pressed && app.focus != Focus::Body {
            app.status = None;
            switch_focus(session, app);
        }
        match leaf_ratatui::handle_mouse(session.body_mut(), mouse, &mut app.editor) {
            leaf_ratatui::MouseOutcome::Continue => {}
            leaf_ratatui::MouseOutcome::ContextMenu { .. } => {
                app.status = Some("context menu: not in provui — leaf-tui has it".into());
            }
        }
    }
}

/// What a click on one of flower's rows does, in the page vocabulary its keys
/// use.
///
/// A click stands on the row. A second click on the row the cursor is already
/// on is Enter — a container opens as a page, a value opens for editing. Two
/// clicks rather than a double-click, because a terminal reports no such thing
/// and a timing guess would make a slow second click a different gesture from a
/// quick one. The click that brought the keyboard to the pane (`arriving`) only
/// ever stands: it landed wherever the pointer happened to be, and if that was
/// the cursor's row, opening a value for editing is not what focusing a pane
/// means.
///
/// In flower's two-pane view the other pane is one step along the lineage in
/// one direction or the other, and a click there takes that step first.
fn click_metadata(model: &mut Model<ProvBackend>, hit: ui::MetadataHit, arriving: bool) {
    use ui::MetadataHit as H;
    match hit {
        H::Row(i) if i == model.page_selected() && !arriving => model.page_enter(),
        H::Row(i) => stand_on_row(model, i),
        // The left pane is the page this one was opened from, still marking the
        // row it was opened through: back out, onto the row that was clicked.
        H::ParentRow(i) => {
            model.page_back();
            stand_on_row(model, i);
        }
        // The right pane previews what the cursor would open: open it, onto the
        // row that was clicked. Only a container has a preview, so this is a
        // page and never an edit.
        H::PeekRow(i) => {
            model.page_enter();
            stand_on_row(model, i);
        }
    }
}

/// Put the page cursor on row `i` by walking, which is the only way the page
/// projection moves — so a click cannot land the cursor anywhere `j`/`k` could
/// not, and a row past the end of the page is the last row.
fn stand_on_row(model: &mut Model<ProvBackend>, i: usize) {
    let i = i.min(model.page().items.len().saturating_sub(1));
    while model.page_selected() < i {
        model.page_move_down();
    }
    while model.page_selected() > i {
        model.page_move_up();
    }
}

fn on_paste(session: &mut DocumentSession, app: &mut App, text: &str) {
    match app.focus {
        Focus::Body => {
            app.status = None;
            session.body_mut().paste(text);
        }
        // flower takes a value one keystroke at a time and has no paste of its
        // own; synthesising one here would be this host inventing an edit path
        // the widget does not have.
        Focus::Metadata => {
            app.status = Some(format!("paste goes to the body pane ({FOCUS_CHORD})"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flower_core::Seg;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The 80×24 terminal every test in here drives.
    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    /// [`super::on_key`] with this file's one terminal size filled in — the
    /// argument only the navigation verbs use, and only to lay out the document
    /// they arrive at.
    fn on_key(session: &mut DocumentSession, app: &mut App, key: KeyEvent) -> Flow {
        super::on_key(session, app, key, SCREEN)
    }

    const DOC: &str = "\
---
# a comment nobody should lose
title: Old Title
draft: true
---
# Heading

Original body.
";

    fn scratch(name: &str, doc: &str) -> PathBuf {
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, doc).unwrap();
        path
    }

    /// An 80×24 terminal's worth of [`DOC`], started exactly the way [`run`]
    /// starts one.
    fn open(name: &str) -> (PathBuf, DocumentSession, App) {
        open_with(name, DOC, SCREEN)
    }

    /// `doc`, opened for a terminal of `screen`'s size.
    fn open_with(name: &str, doc: &str, screen: Rect) -> (PathBuf, DocumentSession, App) {
        let path = scratch(name, doc);
        // `Nav::none`, not `Nav::discover`: what these tests are about is this
        // host, and discovery walks the real filesystem to the root, so what it
        // finds is a property of the machine running them.
        let nav = nav::Nav::none();
        let mut session = nav.open(&path).unwrap();
        let app = App::new(&session, nav);
        begin(&mut session, &app, screen);
        (path, session, app)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(session: &mut DocumentSession, app: &mut App, text: &str) {
        for c in text.chars() {
            assert_eq!(on_key(session, app, key(KeyCode::Char(c))), Flow::Continue);
        }
    }

    /// Drive the metadata pane's keys the way a person would: walk the page to
    /// the row, open it, clear it, type, commit.
    fn retype_metadata(session: &mut DocumentSession, app: &mut App, target: &str, value: &str) {
        let wanted = vec![Seg::Key(target.into())];
        for _ in 0..16 {
            if session.metadata().selected_path().as_deref() == Some(wanted.as_slice()) {
                break;
            }
            on_key(session, app, key(KeyCode::Char('j')));
        }
        assert_eq!(
            session.metadata().selected_path(),
            Some(wanted),
            "never found {target} on the page"
        );
        on_key(session, app, key(KeyCode::Char('e')));
        assert!(matches!(session.metadata().mode, Mode::Editing { .. }));
        for _ in 0..64 {
            on_key(session, app, key(KeyCode::Backspace));
        }
        typed(session, app, value);
        on_key(session, app, key(KeyCode::Enter));
    }

    /// The whole point of the binary, driven headlessly: open a file, edit both
    /// regions through the two widgets' own key handlers, save from one pane,
    /// and find both edits — and everything neither edit touched — on disk.
    #[test]
    fn edits_both_panes_through_their_widgets_and_saves_one_document() {
        let (path, mut session, mut app) = open("provui_tui_both_panes.md");
        assert_eq!(app.focus, Focus::Body, "the prose is the primary surface");
        assert!(!session.dirty());

        // The body, through leaf.
        typed(&mut session, &mut app, "Edited: ");
        assert!(session.dirty(), "leaf's keys reached the session's body");

        // The metadata, through flower.
        assert_eq!(on_key(&mut session, &mut app, ctrl('w')), Flow::Continue);
        assert_eq!(app.focus, Focus::Metadata);
        retype_metadata(&mut session, &mut app, "title", "New Title");

        // A save from the metadata pane writes the body too: the unit is the
        // document, not the pane.
        assert_eq!(on_key(&mut session, &mut app, ctrl('s')), Flow::Continue);
        assert_eq!(
            app.status.as_deref(),
            Some("saved provui_tui_both_panes.md")
        );
        assert!(!session.dirty(), "clean after save");

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("title: New Title"),
            "metadata edit:\n{saved}"
        );
        assert!(saved.contains("Edited: "), "body edit:\n{saved}");
        assert!(saved.contains("draft: true"), "untouched key:\n{saved}");
        assert!(
            saved.contains("# a comment nobody should lose"),
            "comment:\n{saved}"
        );
        assert!(saved.contains("Original body."), "rest of body:\n{saved}");

        // And it round-trips: reopening sees exactly what was written.
        let reopened = DocumentSession::open(&path).unwrap();
        assert!(reopened.body().source.contains("Edited: "));
        assert!(!reopened.dirty());

        let _ = std::fs::remove_file(&path);
    }

    /// `^S` is flower's `s` with a modifier flower ignores, so from the metadata
    /// pane it arrives as `KeyCode::Char('s')` and saves — but only because the
    /// host never lets a chord it owns get that far.
    #[test]
    fn the_focus_chord_is_taken_before_either_widget_sees_it() {
        let (path, mut session, mut app) = open("provui_tui_chord.md");

        // In the body pane: leaf swallows every Ctrl chord, so if the host did
        // not intercept, `^W` would be a silent no-op instead of a focus switch.
        on_key(&mut session, &mut app, ctrl('w'));
        assert_eq!(app.focus, Focus::Metadata);

        // In the metadata pane: flower reads `key.code` and ignores modifiers,
        // so an unintercepted `^W` would be plain `w` — which flower does not
        // bind, but `^X` would be `x`, which deletes. The interception is what
        // makes the chord safe from both sides.
        on_key(&mut session, &mut app, ctrl('w'));
        assert_eq!(app.focus, Focus::Body);
        assert!(!session.dirty(), "switching focus is not an edit");

        let _ = std::fs::remove_file(&path);
    }

    /// A two-document vault, for the tests that move between them. `Nav::none`
    /// is still what drives them: `part_of` here is a *relative* target, which
    /// resolves lexically, so what these assert is this host's verbs rather than
    /// prov's discovery.
    fn vault(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("provui_tui_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("README.md");
        let note = dir.join("note.md");
        std::fs::write(&root, "---\ntitle: The Vault\n---\n# The Vault\n").unwrap();
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[The Vault](README.md)'\nmood: rainy\n---\n# A Note\n\nProse.\n",
        )
        .unwrap();
        (dir, note)
    }

    /// Stand on a metadata row the way the widget's own keys would leave the
    /// cursor, then take the host's chord.
    fn stand_on(session: &mut DocumentSession, app: &mut App, key: &str) {
        if app.focus != Focus::Metadata {
            on_key(session, app, ctrl('w'));
        }
        session.metadata_mut().focus_on(&[Seg::Key(key.into())]);
    }

    /// The whole navigation gesture, driven headlessly: stand on a link, follow
    /// it, and come back to where you were.
    #[test]
    fn follows_the_link_under_the_metadata_cursor_and_comes_back() {
        let (dir, note) = vault("follow");
        let nav = nav::Nav::none();
        let mut session = nav.open(&note).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);

        stand_on(&mut session, &mut app, "part_of");
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(session.path().ends_with("README.md"), "followed the link");
        assert_eq!(app.name, "README.md", "and the host caught up");
        assert_eq!(app.nav.depth(), 1);

        on_key(&mut session, &mut app, ctrl('o'));
        assert!(session.path().ends_with("note.md"), "and back again");
        assert_eq!(app.nav.depth(), 0);

        // A back with nothing behind it says so rather than doing nothing.
        on_key(&mut session, &mut app, ctrl('o'));
        assert_eq!(app.status.as_deref(), Some("nothing to go back to"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Following is a *reading* gesture, so unlike quitting it has no
    /// second-press escape hatch: an edit is never lost to one.
    #[test]
    fn following_is_refused_while_there_are_unsaved_changes() {
        let (dir, note) = vault("dirty");
        let nav = nav::Nav::none();
        let mut session = nav.open(&note).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);

        typed(&mut session, &mut app, "Edited: ");
        assert!(session.dirty());

        stand_on(&mut session, &mut app, "part_of");
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(session.path().ends_with("note.md"), "stayed put");
        assert!(
            app.status.as_deref().is_some_and(|s| s.contains("unsaved")),
            "and said why: {:?}",
            app.status
        );

        // Twice, deliberately: quitting arms on the first press and goes on the
        // second, and this must not.
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(session.path().ends_with("note.md"), "still put");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two answers that are not a document: a row that is not a link, and
    /// the chord taken from the pane that has no cursor to take it from.
    #[test]
    fn a_follow_that_opens_nothing_says_what_it_found_instead() {
        let (dir, note) = vault("nothing");
        let nav = nav::Nav::none();
        let mut session = nav.open(&note).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);

        // From the body there is no metadata cursor to be standing on, and
        // following whatever the other pane was left on would be a guess.
        assert_eq!(app.focus, Focus::Body);
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.contains("metadata")),
            "{:?}",
            app.status
        );

        stand_on(&mut session, &mut app, "mood");
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.contains("not a link")),
            "{:?}",
            app.status
        );
        assert!(session.path().ends_with("note.md"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_is_refused_once_while_the_document_is_unsaved() {
        let (path, mut session, mut app) = open("provui_tui_quit.md");

        // Clean: quit is quit.
        assert_eq!(on_key(&mut session, &mut app, ctrl('q')), Flow::Quit);

        typed(&mut session, &mut app, "x");
        assert_eq!(
            on_key(&mut session, &mut app, ctrl('q')),
            Flow::Continue,
            "refused while dirty"
        );
        assert!(app.status.as_deref().unwrap().contains("unsaved changes"));
        assert_eq!(
            on_key(&mut session, &mut app, ctrl('q')),
            Flow::Quit,
            "asked twice is meant"
        );

        // But the arming does not survive an unrelated key.
        typed(&mut session, &mut app, "y");
        assert_eq!(on_key(&mut session, &mut app, ctrl('q')), Flow::Continue);
        typed(&mut session, &mut app, "z");
        assert_eq!(
            on_key(&mut session, &mut app, ctrl('q')),
            Flow::Continue,
            "a key in between disarms it"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_leaf_outcome_this_host_does_not_implement_says_so() {
        let (path, mut session, mut app) = open("provui_tui_degraded.md");

        // ^F is leaf's Find, which needs a search bar this host does not have.
        on_key(&mut session, &mut app, ctrl('f'));
        let status = app.status.clone().expect("a refusal, not silence");
        assert!(status.starts_with("find:"), "{status}");

        // Every degradable outcome has something to say.
        use leaf_ratatui::Outcome as O;
        for outcome in [
            O::Copy,
            O::Cut,
            O::Paste,
            O::PastePlain,
            O::SaveAs,
            O::New,
            O::LinkPrompt,
            O::LanguagePrompt,
            O::ImagePrompt,
            O::VideoPrompt,
            O::AudioPrompt,
            O::Palette,
            O::Find,
            O::Replace,
            O::Help,
        ] {
            assert!(!unhandled(outcome).is_empty(), "{outcome:?} says nothing");
        }

        let _ = std::fs::remove_file(&path);
    }

    /// The two widgets and the status line, drawn together, at the sizes a
    /// terminal actually comes in — including the ones where the split is
    /// abandoned.
    #[test]
    fn draws_both_panes_at_every_size_without_panicking() {
        let (path, mut session, mut app) = open("provui_tui_draw.md");

        for (w, h) in [(80, 24), (120, 40), (64, 12), (40, 8), (20, 3), (10, 1)] {
            for focus in [Focus::Body, Focus::Metadata] {
                app.focus = focus;
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                fit_metadata(Rect::new(0, 0, w, h), &mut session, &app);
                terminal
                    .draw(|f| ui::draw(f, &mut app, &mut session))
                    .unwrap();
            }
        }

        // The status line owes the reader four things — the file, the state, the
        // focus, and the way out of it — and owes them at **80 columns**, which
        // is the width that decides whether they fit. A `Line` clips silently on
        // the right, so the only way this stays true is by asserting the last of
        // them is still on the screen.
        app.focus = Focus::Body;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| ui::draw(f, &mut app, &mut session))
            .unwrap();
        let rendered = format!("{}", terminal.backend());
        assert!(rendered.contains("provui_tui_draw.md"), "the file");
        assert!(rendered.contains("○ saved"), "the state");
        assert!(rendered.contains("focus: body"), "the focus");
        assert!(rendered.contains(FOCUS_CHORD), "the chord");
        assert!(
            rendered.contains("^Q quit"),
            "the way out survives the trim"
        );

        // And both pane labels are there, each carrying the focus marker that is
        // the only cue flower's own header bar leaves room for.
        assert!(rendered.contains("▶ leaf — body"), "focused body label");
        assert!(rendered.contains("flower —"), "flower's own header");

        // The metadata pane's hint set is the longer of the two — it carries the
        // follow chord as well — so it is the one that decides what the trim
        // gives up at 80 columns. What it must never give up is the way out.
        app.focus = Focus::Metadata;
        terminal
            .draw(|f| ui::draw(f, &mut app, &mut session))
            .unwrap();
        let rendered = format!("{}", terminal.backend());
        assert!(rendered.contains("focus: metadata"), "the focus");
        assert!(rendered.contains(FOLLOW_CHORD), "the follow chord");
        assert!(
            rendered.contains("^Q quit"),
            "the way out survives the trim"
        );

        let _ = std::fs::remove_file(&path);
    }

    // ── the mouse ────────────────────────────────────────────────────────────

    /// A document with a group too big to inline into a short pane, so the
    /// metadata view has a page to drill into and flower has two panes to draw.
    const NESTED: &str = "\
---
title: Old Title
draft: true
server:
  host: localhost
  port: 8080
  user: app
  pass: hunter2
  name: main
  zone: eu
  pool: 4
  tls: true
---
# Heading

Original body.
";

    /// One frame into a test terminal, which is what fills in `app.panes`.
    fn frame(session: &mut DocumentSession, app: &mut App, screen: Rect) -> ui::Panes {
        let mut terminal = Terminal::new(TestBackend::new(screen.width, screen.height)).unwrap();
        fit_metadata(screen, session, app);
        terminal.draw(|f| ui::draw(f, app, session)).unwrap();
        app.panes.expect("drawn")
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    /// The screen row of item `i` of a page pane whose top is `pane.y`: the
    /// header, the breadcrumb, then the items. In flower's two-pane view both
    /// panes share the header, so it holds for either half.
    fn row_y(pane: Rect, i: usize) -> u16 {
        pane.y + 2 + i as u16
    }

    fn standing_on(session: &DocumentSession) -> String {
        session
            .metadata()
            .page_item()
            .map(|item| item.label.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_click_on_a_metadata_row_stands_on_it_and_a_second_click_opens_it() {
        let screen = Rect::new(0, 0, 120, 40);
        let (path, mut session, mut app) = open_with("provui_tui_click.md", DOC, screen);
        let panes = frame(&mut session, &mut app, screen);
        let metadata = panes.metadata.expect("metadata pane");
        assert!(
            metadata.width < 64,
            "one page pane, so `row_y` holds: {metadata:?}"
        );
        let draft = session
            .metadata()
            .page()
            .position_of(&[Seg::Key("draft".into())])
            .expect("draft on the root page");
        assert_ne!(
            session.metadata().page_selected(),
            draft,
            "the cursor starts elsewhere"
        );
        assert_eq!(app.focus, Focus::Body);

        // One click: the keyboard and the cursor both go where it landed, and
        // nothing opens.
        on_mouse(
            &mut session,
            &mut app,
            click(metadata.x + 3, row_y(metadata, draft)),
        );
        assert_eq!(app.focus, Focus::Metadata);
        assert_eq!(standing_on(&session), "draft");
        assert!(matches!(session.metadata().mode, Mode::Normal));

        // The same row again is Enter: the value opens for editing.
        on_mouse(
            &mut session,
            &mut app,
            click(metadata.x + 3, row_y(metadata, draft)),
        );
        assert!(matches!(session.metadata().mode, Mode::Editing { .. }));

        // While a value is open the mouse leaves the cursor alone, as the pane's
        // own keys do.
        on_mouse(
            &mut session,
            &mut app,
            click(metadata.x + 3, row_y(metadata, 0)),
        );
        assert!(matches!(session.metadata().mode, Mode::Editing { .. }));
        assert_eq!(standing_on(&session), "draft");
        on_key(&mut session, &mut app, key(KeyCode::Esc));
        assert!(matches!(session.metadata().mode, Mode::Normal));

        // Chrome is not a row: the header and the footer move nothing.
        on_mouse(&mut session, &mut app, click(metadata.x + 3, metadata.y));
        on_mouse(
            &mut session,
            &mut app,
            click(metadata.x + 3, metadata.bottom() - 1),
        );
        assert_eq!(standing_on(&session), "draft");

        // A click in the body takes the keyboard back...
        let body = panes.body.expect("body pane");
        on_mouse(&mut session, &mut app, click(body.x + 1, body.y + 1));
        assert_eq!(app.focus, Focus::Body);

        // ...and the click that brings it over again only ever stands, even on
        // the row the cursor is already on: focusing a pane is not Enter.
        on_mouse(
            &mut session,
            &mut app,
            click(metadata.x + 3, row_y(metadata, draft)),
        );
        assert_eq!(app.focus, Focus::Metadata);
        assert_eq!(standing_on(&session), "draft");
        assert!(matches!(session.metadata().mode, Mode::Normal));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_wheel_walks_the_metadata_page_without_taking_the_keyboard() {
        let screen = Rect::new(0, 0, 120, 40);
        let (path, mut session, mut app) = open_with("provui_tui_wheel.md", DOC, screen);
        let metadata = frame(&mut session, &mut app, screen)
            .metadata
            .expect("metadata pane");
        let start = session.metadata().page_selected();
        let (x, y) = (metadata.x + 3, metadata.y + 3);

        on_mouse(
            &mut session,
            &mut app,
            mouse(MouseEventKind::ScrollDown, x, y),
        );
        assert_eq!(session.metadata().page_selected(), start + 1);
        assert_eq!(app.focus, Focus::Body, "scrolling is not focusing");

        on_mouse(
            &mut session,
            &mut app,
            mouse(MouseEventKind::ScrollUp, x, y),
        );
        assert_eq!(session.metadata().page_selected(), start);

        let _ = std::fs::remove_file(&path);
    }

    /// flower draws two panes when its pane is wide enough and the document
    /// has somewhere to go, and which page each half holds depends on where
    /// the cursor is. A click in the half that is not the cursor's page takes
    /// the step that half stands for.
    #[test]
    fn in_the_two_pane_metadata_view_a_click_goes_where_the_half_points() {
        // Wide enough that a third of it clears flower's 64-column split, and
        // short enough that `server` is a page rather than a group inlined into
        // the root.
        let screen = Rect::new(0, 0, 200, 8);
        let (path, mut session, mut app) = open_with("provui_tui_two_pane.md", NESTED, screen);
        let metadata = frame(&mut session, &mut app, screen)
            .metadata
            .expect("metadata pane");
        assert!(metadata.width >= 64, "{metadata:?}");
        let labels = |session: &DocumentSession| {
            session
                .metadata()
                .page()
                .items
                .iter()
                .map(|item| item.label.clone())
                .collect::<Vec<_>>()
        };
        assert!(
            !session.metadata().pages_would_degenerate(),
            "server should be a page of its own: {:?}",
            labels(&session)
        );
        assert!(session.metadata().page_leads_the_split());
        let server = session
            .metadata()
            .page()
            .position_of(&[Seg::Key("server".into())])
            .expect("server on the root page");
        let left = metadata.x + 3;
        let right = metadata.x + metadata.width / 2 + 3;

        // The left half is the root page; standing on `server` fills the right
        // half with a preview of its page.
        on_mouse(&mut session, &mut app, click(left, row_y(metadata, server)));
        assert_eq!(app.focus, Focus::Metadata);
        assert_eq!(standing_on(&session), "server");
        assert!(session.metadata().peek_page().is_some());

        // A click on a row of the preview opens the page, on that row.
        on_mouse(&mut session, &mut app, click(right, row_y(metadata, 1)));
        assert_eq!(session.metadata().focus(), &[Seg::Key("server".into())]);
        assert_eq!(standing_on(&session), "port", "{:?}", labels(&session));
        assert!(
            !session.metadata().page_leads_the_split(),
            "now the root is on the left and server on the right"
        );

        // A click on the parent's row backs out onto it.
        on_mouse(&mut session, &mut app, click(left, row_y(metadata, 0)));
        assert!(session.metadata().focus().is_empty());
        assert_eq!(standing_on(&session), "title", "{:?}", labels(&session));

        let _ = std::fs::remove_file(&path);
    }
}
