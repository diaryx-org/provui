//! Moving between documents: following the link under the cursor, coming
//! back, and naming the place the caret is. What a link *is* belongs to
//! `provui-core`, and the arrangement policy to [`nav`]; this is
//! what the host does with the answers.

use provui_core::DocumentSession;
use ratatui::layout::Rect;

use crate::app::{begin, file_name, refresh_findings};
use crate::input::{mid_edit_refusal, open_in_metadata};
use crate::{App, BACK_CHORD, Focus, nav};

/// Open the document the link under the cursor points at, in either pane.
///
/// Each pane has its own cursor and its own kind of link, and the chord means
/// the same thing in both: the metadata pane follows the row it is standing on,
/// the body pane follows the link the caret is inside. What it must *not* do is
/// follow the other pane's cursor — a body caret in the middle of a paragraph is
/// not evidence about which frontmatter row was last selected, and following one
/// from the other would be this host guessing.
pub fn follow(session: &mut DocumentSession, app: &mut App, screen: Rect) {
    let found = match app.focus {
        Focus::Metadata => app.nav.follow(session),
        Focus::Body => match app.nav.follow_in_body(session) {
            Ok(found) => found,
            // Reading the prose's links means parsing the prose. A body this
            // host cannot parse is worth saying out loud rather than reporting
            // as "no link here", which would be the same message a caret in an
            // ordinary paragraph gets.
            Err(e) => {
                app.status = Some(format!("reading the body's links: {e}"));
                return;
            }
        },
    };
    let landing = match found {
        nav::Follow::NoCursor => {
            app.status = Some("nothing under the cursor".into());
            return;
        }
        nav::Follow::NotALink => {
            app.status = Some(match app.focus {
                Focus::Metadata => "not a link — stand on a relation's target".into(),
                Focus::Body => "no link under the caret".to_string(),
            });
            return;
        }
        nav::Follow::Lands(_, landing) | nav::Follow::BodyLands(_, landing) => landing,
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

/// Put the link text that would point at the caret's position in the status
/// line.
///
/// Showing it is the whole deliverable, and deliberately: this host has no
/// clipboard — `^C` and `^X` in the body already say so — so writing the
/// reference into the status line is the honest maximum, and it is the terminal
/// that copies from there. A workspace is what supplies the spelling; without
/// one it is a relative markdown link, which is the only form two paths alone
/// can justify.
pub fn show_reference(session: &DocumentSession, app: &mut App) {
    if !session.has_body() {
        app.status = Some("this document has no prose body to point into".into());
        return;
    }
    let from = session.path().to_path_buf();
    app.status = Some(format!(
        "link to here: {}",
        app.nav.reference_here(session, &from)
    ));
}

/// Back to the document the last follow came from.
pub fn go_back(session: &mut DocumentSession, app: &mut App, screen: Rect) {
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
pub fn leaving_is_allowed(session: &DocumentSession, app: &mut App) -> bool {
    if open_in_metadata(session) {
        app.status = Some(mid_edit_refusal(session));
        return false;
    }
    if session.dirty() {
        app.status = Some("unsaved changes — ^S to save before leaving".into());
        return false;
    }
    true
}

/// Take on a document that has just replaced the one being edited.
pub fn arrive(session: &mut DocumentSession, app: &mut App, screen: Rect) {
    app.name = file_name(session.path());
    // The prose is the document; one with no prose region has nowhere else to
    // put the keyboard.
    app.focus = if session.has_body() {
        Focus::Body
    } else {
        Focus::Metadata
    };
    app.quit_armed = false;
    app.overwrite_armed = false;
    // The same preparation the first document got: a fresh model is in the row
    // projection, not the page one, and has not skipped its lone drill row.
    begin(session, app, screen);
    refresh_findings(session, app);
    app.status = Some(format!("{} · {BACK_CHORD} back", app.name));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::on_key;
    use crate::testing::*;
    use crate::ui;
    use flower_core::{Mode, Seg};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use std::path::PathBuf;

    /// A two-document vault, for the tests that move between them. `Nav::none`
    /// is still what drives them: `part_of` here is a *relative* target, which
    /// resolves lexically, so what these assert is this host's verbs rather than
    /// prov's discovery.
    ///
    /// The note reaches the root twice — once through `part_of` in the
    /// frontmatter and once through a link in the prose — because the follow
    /// chord now works from either pane and the two paths should land in the
    /// same place.
    fn vault(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("provui_tui_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.join("README.md");
        let note = dir.join("note.md");
        std::fs::write(&root, "---\ntitle: The Vault\n---\n# The Vault\n").unwrap();
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[The Vault](README.md)'\nmood: rainy\n---\n# A Note\n\nProse.\n\nSee [the vault](README.md).\n",
        )
        .unwrap();
        (dir, note)
    }

    /// Put the body caret at the first byte of `needle` in the prose.
    ///
    /// The caret is a byte offset into the same buffer the link spans are
    /// measured in, which is the whole reason the body half of the follow
    /// gesture needs no coordinate conversion — so a test can place one by
    /// searching the source, exactly as leaf places one by clicking.
    fn caret_at(session: &mut DocumentSession, needle: &str) {
        let at = session
            .body()
            .source
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in the body"));
        session.body_mut().caret = at;
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

        // What the body pane shows after each move, as drawn. Arriving swaps
        // the whole session under both widgets, and the one that has to be
        // watched is leaf: it keeps per-terminal state across documents, and
        // that state once handed the document you left back to the one you
        // arrived at.
        let body_shows = |session: &mut DocumentSession, app: &mut App, text: &str| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| ui::draw(f, app, session)).unwrap();
            format!("{}", terminal.backend()).contains(text)
        };
        assert!(body_shows(&mut session, &mut app, "Prose."));

        stand_on(&mut session, &mut app, "part_of");
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(session.path().ends_with("README.md"), "followed the link");
        assert_eq!(app.name, "README.md", "and the host caught up");
        assert_eq!(app.nav.depth(), 1);
        assert!(
            body_shows(&mut session, &mut app, "The Vault")
                && !body_shows(&mut session, &mut app, "Prose."),
            "the body pane shows the document arrived at"
        );

        on_key(&mut session, &mut app, ctrl('o'));
        assert!(session.path().ends_with("note.md"), "and back again");
        assert_eq!(app.nav.depth(), 0);
        assert!(
            body_shows(&mut session, &mut app, "Prose."),
            "and the body pane came back with it"
        );

        // A back with nothing behind it says so rather than doing nothing.
        on_key(&mut session, &mut app, ctrl('o'));
        assert_eq!(app.status.as_deref(), Some("nothing to go back to"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same gesture from the other pane: a link written in the *prose*,
    /// followed from the caret inside it.
    ///
    /// Worth its own test rather than a second assertion on the metadata one,
    /// because the two reach the workspace by different routes — a metadata
    /// path versus a byte span — and meet only at `WorkspaceView::resolve`.
    /// That they land in the same place is the claim `AnyLink` makes.
    #[test]
    fn follows_the_body_link_under_the_caret_and_comes_back() {
        let (dir, note) = vault("follow_body");
        let nav = nav::Nav::none();
        let mut session = nav.open(&note).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);
        assert_eq!(app.focus, Focus::Body, "the prose has the keyboard");

        // Inside the label, not on the bracket: where a reader who has just
        // clicked the words of a link actually is.
        caret_at(&mut session, "the vault](README.md)");
        on_key(&mut session, &mut app, ctrl('g'));
        assert!(
            session.path().ends_with("README.md"),
            "followed the prose link"
        );
        assert_eq!(app.name, "README.md");
        assert_eq!(app.nav.depth(), 1);

        on_key(&mut session, &mut app, ctrl('o'));
        assert!(session.path().ends_with("note.md"), "and back again");
        assert_eq!(app.nav.depth(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reference chord: the link text that points at where the caret is,
    /// in the status line, because there is nowhere else for it to go.
    #[test]
    fn the_reference_chord_names_the_heading_the_caret_is_under() {
        let path = scratch(
            "provui_tui_reference.md",
            "---\ntitle: A Note\n---\n# A Note\n\n## Crash Safety\n\nWhy the journal is written first.\n",
        );
        let nav = nav::Nav::none();
        let mut session = nav.open(&path).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);

        caret_at(&mut session, "Why the journal");
        on_key(&mut session, &mut app, ctrl('r'));
        assert_eq!(
            app.status.as_deref(),
            Some("link to here: [Crash Safety](#crash-safety)"),
            "the heading above the caret, slugged the way prov slugs one"
        );

        // Above the first heading there is no place to name, so what comes back
        // is a reference to the document — which from the document itself is a
        // relative link to its own file.
        session.body_mut().caret = 0;
        on_key(&mut session, &mut app, ctrl('r'));
        assert_eq!(
            app.status.as_deref(),
            Some("link to here: [A Note](#a-note)"),
            "the caret sits on the first heading itself"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The findings channel, end to end in the host: run on open, counted in
    /// the status line, and — because the session now hands the metadata half
    /// to flower as annotations — spelled out by the *widget* when the metadata
    /// cursor reaches the row the finding is about. The assertion is on what is
    /// on the screen rather than on which layer drew it, which is what makes it
    /// the test that the host may stop drawing it.
    ///
    /// `Nav::discover` rather than `Nav::none`, because there are no findings
    /// without a workspace to check against — which is the one thing this test
    /// needs the real filesystem for.
    #[test]
    fn findings_are_run_on_open_and_reported_where_they_belong() {
        let dir = std::env::temp_dir().join("provui_tui_findings");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "---\ntitle: The Vault\ncontents:\n- '[A Note](note.md)'\n---\n# The Vault\n",
        )
        .unwrap();
        let note = dir.join("note.md");
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[Nowhere](/nowhere.md)'\n---\n# A Note\n\nSee [the missing one](gone.md).\n",
        )
        .unwrap();

        let nav = nav::Nav::discover(&note);
        assert!(nav.has_workspace(), "the vault was found");
        let mut session = nav.open(&note).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);
        refresh_findings(&mut session, &mut app);

        assert_eq!(session.findings().len(), 2, "{:#?}", session.findings());
        // The body half is leaf's to draw, and it has it.
        assert_eq!(session.body().highlights().len(), 1);

        let drawn = |session: &mut DocumentSession, app: &mut App| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| ui::draw(f, app, session)).unwrap();
            format!("{}", terminal.backend())
        };
        app.status = None;
        assert!(
            drawn(&mut session, &mut app).contains("2 findings"),
            "the count is on the line"
        );

        // Standing on the row the finding is about replaces flower's own hints
        // with what is wrong with it — in the widget's footer, one line above
        // the host's status line.
        stand_on(&mut session, &mut app, "part_of");
        app.status = None;
        let line = drawn(&mut session, &mut app);
        assert!(line.contains("broken part_of link"), "{line}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `e` on a relation's row opens flower's picker over the workspace's own
    /// documents — and this host binds nothing for it.
    ///
    /// The key is flower's (`begin_choose`, which falls back to the text line
    /// where there is nothing to pick from), the list is the backend's, and the
    /// backend got it from `Nav::open`. What is being tested here is the wiring:
    /// a document opened through this host has candidates, so the one key does
    /// the right thing on a link row and the old thing everywhere else.
    #[test]
    fn a_relation_row_picks_from_the_workspace_and_nothing_here_binds_it() {
        let dir = std::env::temp_dir().join("provui_tui_picker");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "---\ntitle: The Vault\ncontents:\n- '[A Note](note.md)'\n- '[Another](other.md)'\n---\n# The Vault\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("other.md"),
            "---\ntitle: Another\npart_of: '[The Vault](README.md)'\n---\n# Another\n",
        )
        .unwrap();
        let note = dir.join("note.md");
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[The Vault](README.md)'\n---\n# A Note\n",
        )
        .unwrap();

        let nav = nav::Nav::discover(&note);
        assert!(nav.has_workspace());
        let mut session = nav.open(&note).unwrap();
        let mut app = App::new(&session, nav);
        begin(&mut session, &app, SCREEN);

        stand_on(&mut session, &mut app, "part_of");
        on_key(&mut session, &mut app, key(KeyCode::Char('e')));
        let offered: Vec<String> = session
            .metadata()
            .visible_choices()
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(
            offered,
            ["The Vault", "Another"],
            "the workspace's other documents, this one left out"
        );

        // Esc closes it and writes nothing.
        on_key(&mut session, &mut app, key(KeyCode::Esc));
        assert!(!session.dirty(), "cancelling a pick is not an edit");

        // And on a row with neither a vocabulary nor a relation, the same key
        // is still the text line it always was.
        stand_on(&mut session, &mut app, "title");
        on_key(&mut session, &mut app, key(KeyCode::Char('e')));
        assert!(matches!(session.metadata().mode, Mode::Editing { .. }));
        on_key(&mut session, &mut app, key(KeyCode::Esc));

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

        // From the body, with the caret in the heading: the pane has a cursor
        // and it is not on a link, which is the same answer a metadata row
        // that is not a link gets.
        assert_eq!(app.focus, Focus::Body);
        caret_at(&mut session, "# A Note");
        on_key(&mut session, &mut app, ctrl('g'));
        assert_eq!(app.status.as_deref(), Some("no link under the caret"));

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
}
