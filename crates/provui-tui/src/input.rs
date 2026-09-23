//! Keys and pastes: the host's chords, taken before either widget sees them,
//! and everything else handed to the widget that has the keyboard.

use flower_core::Mode;
use provui_core::DocumentSession;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::refresh_findings;
use crate::follow::{follow, go_back, show_reference};
use crate::{App, FOCUS_CHORD, Flow, Focus};

/// Is this one of the host's chords? Taken before either widget sees the event —
/// see the module docs for why that is the only way it can work.
pub fn is_chord(key: KeyEvent, letter: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&letter))
}

/// One key, start to finish — dispatched, and then noticed.
///
/// [`DocumentSession::sync_history`] runs after every event, whatever the event
/// turned out to be: the session reads both editors' change counters rather than
/// being told about an edit, so the only thing the host has to get right is
/// calling this once per event. See that method for why it is polled.
pub fn on_key(session: &mut DocumentSession, app: &mut App, key: KeyEvent, screen: Rect) -> Flow {
    let flow = dispatch_key(session, app, key, screen);
    session.sync_history();
    flow
}

pub fn dispatch_key(
    session: &mut DocumentSession,
    app: &mut App,
    key: KeyEvent,
    screen: Rect,
) -> Flow {
    // A refusal is only ever an answer to the key that provoked it.
    let quit_armed = std::mem::take(&mut app.quit_armed);
    let overwrite_armed = std::mem::take(&mut app.overwrite_armed);
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
    if is_chord(key, 'r') {
        show_reference(session, app);
        return Flow::Continue;
    }
    // `^⇧Z` is the other spelling of redo, and crossterm reports it as `^Z`
    // with Shift — so the shifted case is tested before the plain one.
    if is_chord(key, 'z') {
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            history(session, app, false);
        } else {
            history(session, app, true);
        }
        return Flow::Continue;
    }
    if is_chord(key, 'y') {
        history(session, app, false);
        return Flow::Continue;
    }

    match app.focus {
        Focus::Metadata => match flower_ratatui::handle_key(session.metadata_mut(), key) {
            flower_ratatui::Outcome::Continue => Flow::Continue,
            flower_ratatui::Outcome::Save => {
                save(session, app, overwrite_armed);
                Flow::Continue
            }
            flower_ratatui::Outcome::Quit => quit(session, app, quit_armed),
        },
        Focus::Body => {
            let outcome = leaf_ratatui::handle_key(session.body_mut(), key, &mut app.editor);
            match outcome {
                leaf_ratatui::Outcome::Continue => Flow::Continue,
                leaf_ratatui::Outcome::Save => {
                    save(session, app, overwrite_armed);
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

/// Whether the metadata pane has something open over its page — a value being
/// typed, or a picker being walked.
///
/// Both are modal and both are the model's own business until they close, so
/// every gesture that would leave the pane or move its cursor checks this
/// rather than checking `Mode::Editing` alone. flower gained the picker after
/// this host was written, and a check that named only the one mode would have
/// let a click commit a choice to the wrong row.
pub fn open_in_metadata(session: &DocumentSession) -> bool {
    matches!(
        session.metadata().mode,
        Mode::Editing { .. } | Mode::Choosing { .. }
    )
}

/// What to say about it, in the vocabulary of whichever one is open.
pub fn mid_edit_refusal(session: &DocumentSession) -> String {
    match session.metadata().mode {
        Mode::Choosing { .. } => "finish choosing first — Enter picks, Esc cancels".into(),
        _ => "finish the metadata edit first — Enter commits, Esc cancels".into(),
    }
}

/// Walk the session's one history, in whichever direction.
///
/// Nothing is said when it works — the change is on the screen, in one pane or
/// the other, and a message would be a second copy of it. What is worth saying
/// is that the key did nothing, which a reader cannot otherwise tell from a
/// change they were not looking at.
///
/// Refused mid-edit for the reason every other host gesture is: flower's
/// buffer is the model's business until Enter or Esc, and an undo landing
/// underneath a half-typed value would undo something the reader cannot see.
pub fn history(session: &mut DocumentSession, app: &mut App, backwards: bool) {
    if open_in_metadata(session) {
        app.status = Some(mid_edit_refusal(session));
        return;
    }
    let moved = if backwards {
        session.undo()
    } else {
        session.redo()
    };
    if !moved {
        app.status = Some(if backwards {
            "nothing to undo".into()
        } else {
            "nothing to redo".into()
        });
    }
}

pub fn switch_focus(session: &DocumentSession, app: &mut App) {
    if !session.has_body() {
        app.status = Some("this document has no prose body".into());
        return;
    }
    // Leaving mid-edit would strand a half-typed value in a pane that is no
    // longer taking keys, and flower's mode would still be `Editing` when you
    // came back. Cheaper to say so than to guess whether it was a commit or a
    // cancel.
    if open_in_metadata(session) {
        app.status = Some(mid_edit_refusal(session));
        return;
    }
    app.focus = app.focus.other();
}

/// The document is what a save writes: the metadata model's edits and the body
/// buffer's, reconciled and written as one file, from whichever pane asked.
///
/// A save that would overwrite something else's write to the file is refused
/// once, and the second save in a row is taken as meaning it — the same
/// two-press shape as quitting with unsaved changes.
pub fn save(session: &mut DocumentSession, app: &mut App, overwrite: bool) {
    // leaf's own `Doc::save`/`mark_saved` are deliberately not used: this body
    // is a *region* of a file rather than a file, the `Doc` has no path, and
    // dirtiness for the document as a whole is the session's answer to give.
    let result = if overwrite {
        session.save_over()
    } else {
        session.save()
    };
    match result {
        Ok(()) => app.status = Some(format!("saved {}", app.name)),
        Err(_) if session.changed_on_disk() => {
            app.overwrite_armed = true;
            app.status = Some(format!(
                "{} changed on disk since it was opened — ^S again to overwrite it",
                app.name
            ));
            return;
        }
        Err(e) => {
            app.status = Some(format!("save failed: {e}"));
            return;
        }
    }
    // The bytes on disk are what prov checks, so the check runs after the
    // write and not before it — a save is exactly when a link that was being
    // typed stops being half-written.
    let saved = std::mem::take(&mut app.status);
    refresh_findings(session, app);
    if app.status.is_none() {
        app.status = saved;
    }
}

pub fn quit(session: &DocumentSession, app: &mut App, armed: bool) -> Flow {
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
pub fn unhandled(outcome: leaf_ratatui::Outcome) -> &'static str {
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

pub fn on_paste(session: &mut DocumentSession, app: &mut App, text: &str) {
    dispatch_paste(session, app, text);
    session.sync_history();
}

pub fn dispatch_paste(session: &mut DocumentSession, app: &mut App, text: &str) {
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
    use crate::testing::on_key;
    use crate::testing::*;

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

    /// One undo over both panes, driven through the host's chord.
    ///
    /// The point of taking `^Z` at the host: leaf binds it for the body's own
    /// undo, so left alone it would mean "the pane you are standing in", and a
    /// reader who edited the frontmatter between two runs of typing would get
    /// the wrong thing back. Here the metadata edit is reached *from the body
    /// pane*, in its place in the order, and `^Y` replays forward.
    ///
    /// Nothing here counts keypresses. leaf decides how many undo steps a run
    /// of typing is — it may hold both runs as one — so the test presses until
    /// the body is back and asserts what is in the document, which is the part
    /// that is this host's to get right.
    #[test]
    fn undo_walks_back_through_both_panes_in_the_order_the_edits_were_made() {
        let (path, mut session, mut app) = open("provui_tui_undo.md");
        let shown = |session: &DocumentSession| {
            session
                .meta()
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let typed_body = |session: &DocumentSession| {
            session.body().source.contains("first ") || session.body().source.contains("second ")
        };

        // Body, then metadata, then body — each through the widget that owns it.
        typed(&mut session, &mut app, "first ");
        on_key(&mut session, &mut app, ctrl('w'));
        retype_metadata(&mut session, &mut app, "title", "New Title");
        on_key(&mut session, &mut app, ctrl('w'));
        typed(&mut session, &mut app, "second ");

        assert!(session.body().source.contains("second "));
        assert_eq!(shown(&session).as_deref(), Some("New Title"));
        assert_eq!(app.focus, Focus::Body, "and the keyboard never leaves it");

        // Back through the body's steps first, however many leaf made of them.
        for _ in 0..32 {
            if !typed_body(&session) {
                break;
            }
            assert_eq!(on_key(&mut session, &mut app, ctrl('z')), Flow::Continue);
        }
        assert!(!typed_body(&session), "the body is back where it started");
        assert_eq!(
            shown(&session).as_deref(),
            Some("New Title"),
            "and the metadata edit is still standing — it came first"
        );

        // The next one reaches it, from the body pane: undo is about the
        // document, the way save already is.
        on_key(&mut session, &mut app, ctrl('z'));
        assert_eq!(shown(&session).as_deref(), Some("Old Title"));
        assert_eq!(app.focus, Focus::Body);

        // Past the end it says so rather than doing something.
        app.status = None;
        on_key(&mut session, &mut app, ctrl('z'));
        assert_eq!(app.status.as_deref(), Some("nothing to undo"));
        assert_eq!(shown(&session).as_deref(), Some("Old Title"));

        // And forward again, oldest undone first: the metadata edit, then the
        // typing that was on top of it.
        on_key(&mut session, &mut app, ctrl('y'));
        assert_eq!(shown(&session).as_deref(), Some("New Title"));
        for _ in 0..32 {
            if typed_body(&session) {
                break;
            }
            on_key(&mut session, &mut app, ctrl('y'));
        }
        assert!(typed_body(&session), "the typing came back too");

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

    /// A file something else wrote while it was open is not overwritten by the
    /// first `^S`; a second in a row means it, and a key in between disarms it.
    #[test]
    fn a_save_over_a_file_changed_on_disk_asks_first() {
        let (path, mut session, mut app) = open("provui_tui_drift.md");
        typed(&mut session, &mut app, "Mine ");
        let theirs = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Original body.", "Theirs.");
        std::fs::write(&path, &theirs).unwrap();

        on_key(&mut session, &mut app, ctrl('s'));
        assert!(app.status.as_deref().unwrap().contains("changed on disk"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs);

        typed(&mut session, &mut app, "x");
        on_key(&mut session, &mut app, ctrl('s'));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            theirs,
            "a key in between disarms it"
        );

        on_key(&mut session, &mut app, ctrl('s'));
        assert_eq!(app.status.as_deref(), Some("saved provui_tui_drift.md"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("Mine "));
        assert!(!session.dirty());

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
}
