//! The mouse. leaf takes its own events and the host forwards them; flower
//! takes none, so a click on its pane is mapped back onto the row it landed on
//! and driven in the vocabulary flower's keys use.

use flower_core::Model;
use provui_core::{DocumentSession, ProvBackend};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::input::{open_in_metadata, switch_focus};
use crate::{App, Focus, ui};

pub fn on_mouse(session: &mut DocumentSession, app: &mut App, mouse: MouseEvent) {
    dispatch_mouse(session, app, mouse);
    session.sync_history();
}

pub fn dispatch_mouse(session: &mut DocumentSession, app: &mut App, mouse: MouseEvent) {
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
        if open_in_metadata(session) {
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
pub fn click_metadata(model: &mut Model<ProvBackend>, hit: ui::MetadataHit, arriving: bool) {
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
pub fn stand_on_row(model: &mut Model<ProvBackend>, i: usize) {
    let i = i.min(model.page().items.len().saturating_sub(1));
    while model.page_selected() < i {
        model.page_move_down();
    }
    while model.page_selected() > i {
        model.page_move_up();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::fit_metadata;
    use crate::testing::on_key;
    use crate::testing::*;
    use flower_core::{Mode, Seg};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::layout::Rect;

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
