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
//! A prov document carries links in both of its regions: some frontmatter keys
//! are *links*, and so is a `[a](b.md)` or a `[[b.md]]` in the prose.
//! [`FOLLOW_CHORD`] opens whichever of the two the focused pane's cursor is on
//! — the metadata row, or the link the body caret is inside — and
//! [`BACK_CHORD`] returns, which makes this a two-key browser over the whole
//! document graph rather than over the spanning tree alone. Both are taken
//! before the widgets for the reason `^W` is, and both are free in leaf's Ctrl
//! table — `^G` for *go*, `^O` for the jump-back every vi has.
//!
//! Everything about *what* a link is belongs to `provui-core`; everything about
//! what this host does with the answer belongs to [`nav`]. See that module for
//! the arrangement policy — which keys decline edits, and which rows sink.

mod app;
mod follow;
mod input;
mod mouse;
mod nav;
#[cfg(test)]
mod testing;
mod ui;

use std::io::stdout;
use std::path::PathBuf;

use anyhow::{Context, Result};
use provui_core::DocumentSession;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;

pub use app::{App, Flow, Focus};

/// How the focus-switch chord is written for a reader. See the module docs for
/// why it is this one.
pub const FOCUS_CHORD: &str = "^W";

/// Open the document the link under the cursor points at — the metadata row in
/// one pane, the link the caret is inside in the other. Free in leaf's Ctrl
/// table, and *go* is what it does.
pub const FOLLOW_CHORD: &str = "^G";

/// Back to the document the last follow came from — vi's jump-back chord, and
/// likewise free in leaf's table.
pub const BACK_CHORD: &str = "^O";

/// Take back the last edit, in whichever pane made it — and `^Y` puts it
/// back.
///
/// **The one pair that is intercepted in order to take it away from a widget
/// rather than because no widget wanted it.** leaf binds `^Z`/`^⇧Z`/`^Y` for
/// the body's own undo, and flower binds `u`/`U` for the metadata's. Left
/// alone, undo would mean "the pane you are standing in", and a reader who
/// edited the frontmatter, typed a sentence, and pressed undo twice would get
/// two sentences back. The host takes the chord and asks the session, which
/// knows which editor took each step — so undo is about the *document*, the way
/// save already is.
///
/// Free by flower's half of the usual test: `z` and `y` are not bare letters it
/// navigates on, so an un-intercepted one would reach it as a plain keypress and
/// do nothing.
pub const UNDO_CHORD: &str = "^Z";

/// Put back the last thing [`UNDO_CHORD`] took. `^⇧Z` is the same key, which is
/// the other spelling every editor has.
pub const REDO_CHORD: &str = "^Y";

/// Show the link text that would point at where the caret is — *r* for
/// reference.
///
/// Free by the same two tests `^W` and `^G` passed: `^R` is unbound in leaf's
/// Ctrl table (which takes `q s a c x v z y u k p f h` and nothing else), and
/// `r` is not one of the bare letters flower navigates on (`j k l h e c C x q
/// s`), so an un-intercepted one would reach it as a plain `r` and do nothing.
/// `^L` and `^K` were the other candidates and both fail a test: `^K` is leaf's
/// kill-to-end-of-line, and `l` is how flower opens a row.
pub const REFERENCE_CHORD: &str = "^R";

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
    ^G              follow the link under the cursor (either pane)
    ^O              back to the document you followed from
    ^R              show the link text that points at the caret
    ^Z              undo — one history over both panes
    ^Y  / ^⇧Z       redo
    ^Q              quit
    body pane       leaf's keys — see `leaf --help`
    metadata pane   j/k move · l/h in/out · e pick/edit · E type · x delete
                    u/U undo/redo the metadata's own journal

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

fn run(terminal: &mut DefaultTerminal, session: &mut DocumentSession, app: &mut App) -> Result<()> {
    app::begin(session, app, screen(terminal)?);
    app::refresh_findings(session, app);

    loop {
        // How much a page inlines is a fact about the room it has, and the room
        // is the *pane's*, not the terminal's. Cheap enough to redo every frame:
        // the model only rebuilds when the budget actually changes.
        app::fit_metadata(screen(terminal)?, session, app);
        terminal.draw(|f| ui::draw(f, app, session))?;

        match event::read()? {
            // Filtered on `Press`: Windows and the kitty protocol both send
            // releases, and a release that inserts a character types everything
            // twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if input::on_key(session, app, key, screen(terminal)?) == Flow::Quit {
                    return Ok(());
                }
            }
            Event::Mouse(mouse) => mouse::on_mouse(session, app, mouse),
            Event::Paste(text) => input::on_paste(session, app, &text),
            _ => {}
        }
    }
}
