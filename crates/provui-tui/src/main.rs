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

mod ui;

use std::io::stdout;
use std::path::PathBuf;

use anyhow::{Context, Result};
use flower_core::{Mode, ViewMode};
use leaf_ratatui::EditorState;
use provui_core::DocumentSession;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;

/// How the focus-switch chord is written for a reader. See the module docs for
/// why it is this one.
pub const FOCUS_CHORD: &str = "^W";

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
    /// Where the panes landed on the last frame, so a click can be routed to the
    /// one it was in. `None` before the first draw.
    pub panes: Option<ui::Panes>,
}

impl App {
    fn new(session: &DocumentSession) -> Self {
        let name = session
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| session.path().display().to_string());
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
            status: None,
            quit_armed: false,
            name,
            panes: None,
        }
    }
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
    ^Q              quit
    body pane       leaf's keys — see `leaf --help`
    metadata pane   j/k move · l/h in/out · e edit · x delete
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

    let mut session =
        DocumentSession::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut app = App::new(&session);

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
                if on_key(session, app, key) == Flow::Quit {
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

/// Is this the host's chord? Taken before either widget sees the event — see
/// the module docs for why that is the only way it can work.
fn is_focus_chord(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('w' | 'W'))
}

fn on_key(session: &mut DocumentSession, app: &mut App, key: KeyEvent) -> Flow {
    // A refusal is only ever an answer to the key that provoked it.
    let quit_armed = std::mem::take(&mut app.quit_armed);
    app.status = None;

    if is_focus_chord(key) {
        switch_focus(session, app);
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
    let at = ratatui::layout::Position::new(mouse.column, mouse.row);

    // A click moves the keyboard to the pane it landed in — the one focus
    // gesture that needs no chord. Wheel events do not: scrolling a pane you are
    // not typing in is a reasonable thing to want.
    let pressed = matches!(mouse.kind, MouseEventKind::Down(_));

    if panes.metadata.is_some_and(|r| r.contains(at)) {
        if pressed && app.focus != Focus::Metadata {
            app.status = None;
            switch_focus(session, app);
        }
        // flower has no mouse handling to forward to.
        return;
    }

    if panes.body.is_some_and(|r| r.contains(at)) {
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

    const DOC: &str = "\
---
# a comment nobody should lose
title: Old Title
draft: true
---
# Heading

Original body.
";

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, DOC).unwrap();
        path
    }

    /// An 80×24 terminal's worth of document, started exactly the way [`run`]
    /// starts one.
    fn open(name: &str) -> (PathBuf, DocumentSession, App) {
        let path = scratch(name);
        let mut session = DocumentSession::open(&path).unwrap();
        let app = App::new(&session);
        begin(&mut session, &app, Rect::new(0, 0, 80, 24));
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
        assert!(rendered.contains("^Q quit"), "clipped off the right edge");

        // And both pane labels are there, each carrying the focus marker that is
        // the only cue flower's own header bar leaves room for.
        assert!(rendered.contains("▶ leaf — body"), "focused body label");
        assert!(rendered.contains("flower —"), "flower's own header");

        let _ = std::fs::remove_file(&path);
    }
}
