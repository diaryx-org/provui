//! Layout and drawing: where the two panes go, and the one status line the host
//! owns.
//!
//! Both widgets render into a `Rect` and neither knows the other exists, so
//! everything shared between them — the split, the focus cue, the file name, the
//! terminal's cursor — is decided here.

use flower_core::Mode;
use provui_core::DocumentSession;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::{App, FOCUS_CHORD, Focus};

/// The smallest metadata band worth drawing.
///
/// `flower_ratatui::page_room` spends 3 rows on chrome, and flower's inline
/// budget floors at 6 item rows however little room it is given — so below 9
/// the band is only losing rows off the bottom of a page that was going to be 6
/// rows long anyway.
const METADATA_MIN_ROWS: u16 = 9;

/// The largest metadata band worth drawing.
///
/// Frontmatter is a handful of keys and the prose is the document; past this
/// the band is taking rows from the body to draw empty list. A document with
/// more metadata than fits navigates — that is what flower's pages are for.
const METADATA_MAX_ROWS: u16 = 14;

/// A body pane shorter than this is not an editor, it is a peephole. Below the
/// point where both panes clear their minimum, the split is abandoned rather
/// than shrunk (see [`layout`]).
const BODY_MIN_ROWS: u16 = 6;

/// Where each piece of the screen went this frame.
///
/// `None` means "not drawn": the metadata pane is absent when a short terminal
/// gave the screen to the body, and the body is absent both when the document
/// has no prose region at all and when the metadata pane took the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panes {
    pub metadata: Option<Rect>,
    pub body_label: Option<Rect>,
    pub body: Option<Rect>,
    pub status: Rect,
}

/// Split `area` into the metadata band, the body pane, and the status line.
///
/// A horizontal band, not a side-by-side split, for two reasons that both come
/// from the widgets: flower collapses its own two-pane page view below 64
/// columns, and half of an 80-column terminal is 40 — so a vertical split would
/// silently degrade the metadata view on the most ordinary terminal there is.
/// Prose wants the width too. Stacking gives both panes the full width and
/// spends the only scarce dimension, height, on the surface that is the point:
/// the body gets everything the band and the status line do not.
///
/// When the terminal is too short for both minimums the split is abandoned
/// rather than shrunk, and the pane holding the keyboard takes the screen —
/// which is why `focus` is an argument to a layout function.
pub fn layout(area: Rect, focus: Focus, has_body: bool) -> Panes {
    let [rest, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    let nothing = Panes {
        metadata: None,
        body_label: None,
        body: None,
        status,
    };
    if rest.height == 0 {
        return nothing;
    }

    // A whole-file config document has no prose region — `DocumentSession`'s
    // body editor stays empty and there is nothing for a second pane to hold.
    if !has_body {
        return Panes {
            metadata: Some(rest),
            ..nothing
        };
    }

    let body_pane = |rest: Rect| {
        let [label, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(rest);
        (label, body)
    };

    let band = (rest.height / 3).clamp(METADATA_MIN_ROWS, METADATA_MAX_ROWS);
    if rest.height < band + BODY_MIN_ROWS {
        return match focus {
            Focus::Metadata => Panes {
                metadata: Some(rest),
                ..nothing
            },
            Focus::Body => {
                let (label, body) = body_pane(rest);
                Panes {
                    body_label: Some(label),
                    body: Some(body),
                    ..nothing
                }
            }
        };
    }

    let [metadata, below] =
        Layout::vertical([Constraint::Length(band), Constraint::Min(0)]).areas(rest);
    let (label, body) = body_pane(below);
    Panes {
        metadata: Some(metadata),
        body_label: Some(label),
        body: Some(body),
        status,
    }
}

/// Draw the frame, and record where everything landed so a mouse click can be
/// routed to the pane it was in.
pub fn draw(f: &mut Frame, app: &mut App, session: &mut DocumentSession) {
    let panes = layout(f.area(), app.focus, session.has_body());

    // The body goes first: `leaf_ratatui::render` sets the terminal's cursor
    // wherever its caret is, whether or not this pane has the keyboard, and
    // ratatui keeps one cursor per frame. Drawing it first leaves the last word
    // to `park_cursor` below.
    if let (Some(label), Some(body)) = (panes.body_label, panes.body) {
        pane_label(f, label, "leaf — body", app.focus == Focus::Body);
        leaf_ratatui::render(f, body, session.body_mut(), &mut app.editor);
    }

    if let Some(metadata) = panes.metadata {
        let focused = app.focus == Focus::Metadata;
        // flower draws its own header bar and the host cannot restyle it, so the
        // focus cue has to travel in the one string it does take from us. The
        // body's label is drawn to match, marker and all.
        let header = format!("{}{}", marker(focused), app.name);
        flower_ratatui::draw_in(f, metadata, session.metadata(), &header);
        if focused {
            park_cursor(f, metadata, session);
        }
    }

    status_line(f, panes.status, app, session);
    app.panes = Some(panes);
}

fn marker(focused: bool) -> &'static str {
    if focused { "▶ " } else { "  " }
}

/// The body pane's title bar, drawn to match the one flower draws for itself so
/// the two panes read as one window.
fn pane_label(f: &mut Frame, area: Rect, text: &str, focused: bool) {
    let style = Style::default()
        .fg(Color::Black)
        .bg(Color::White)
        .add_modifier(Modifier::BOLD);
    let line = Line::from(Span::styled(format!(" {}{}", marker(focused), text), style));
    f.render_widget(line, area);
}

/// Keep the terminal's own cursor inside the pane that is taking input.
///
/// leaf puts it on its caret every frame it draws, focused or not, and there is
/// no way to ask ratatui to take it back — so when the metadata pane has the
/// keyboard, the last writer has to be us, or the blinking cursor sits in the
/// pane that is ignoring every key.
///
/// It lands on flower's edit line, which is where typing goes while a value is
/// open, and on top of the caret glyph flower draws there itself.
fn park_cursor(f: &mut Frame, metadata: Rect, session: &DocumentSession) {
    if metadata.height == 0 || metadata.width == 0 {
        return;
    }
    let x = match &session.metadata().mode {
        // flower's edit line is a ` edit ` badge, a space, then the buffer.
        Mode::Editing { buffer, .. } => 7 + buffer.chars().count() as u16,
        Mode::Normal => 0,
    };
    let x = metadata.x + x.min(metadata.width - 1);
    f.set_cursor_position(Position::new(x, metadata.bottom() - 1));
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The one line the host owns: which file, which pane has the keyboard, whether
/// there is anything unsaved, and how to change the first two of those.
fn status_line(f: &mut Frame, area: Rect, app: &App, session: &DocumentSession) {
    let mut spans = vec![Span::styled(
        format!(" {} ", app.name),
        Style::default().add_modifier(Modifier::BOLD),
    )];

    // Dirtiness is the *document's*, not either editor's: `DocumentSession`
    // answers for the metadata model and the body buffer together, which is the
    // only unit a save writes.
    if session.dirty() {
        spans.push(Span::styled(
            "● unsaved",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        spans.push(Span::styled("○ saved", dim()));
    }

    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!(" focus: {} ", app.focus.name()),
        Style::default().fg(Color::Black).bg(Color::Cyan),
    ));

    // The hints and a message are alternatives rather than neighbours: together
    // they overflow 80 columns, and a `Line` clips on the right — so keeping
    // both would mean losing the end of whichever came last, silently. When
    // there is something to say, saying it is worth more than repeating keys
    // that have not moved.
    match &app.status {
        Some(status) => {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!(" {status} "),
                Style::default().fg(Color::Black).bg(Color::Green),
            ));
        }
        None => spans.push(Span::styled(
            format!("  {FOCUS_CHORD} pane · ^S save · ^Q quit"),
            dim(),
        )),
    }

    f.render_widget(Line::from(spans), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn a_roomy_terminal_gets_both_panes_and_the_body_gets_the_rest() {
        let panes = layout(area(80, 40), Focus::Body, true);
        let metadata = panes.metadata.expect("metadata band");
        let body = panes.body.expect("body pane");

        assert_eq!(
            metadata.height, 13,
            "a third of the 39 rows below the status"
        );
        assert_eq!(metadata.width, 80, "both panes get the full width");
        assert_eq!(body.width, 80);
        assert_eq!(panes.status.height, 1);
        // Every row is spoken for: band + label + body + status.
        assert_eq!(metadata.height + 1 + body.height + 1, 40);
        // The body is the primary surface, and on a roomy terminal it says so.
        assert!(body.height > metadata.height, "{body:?} vs {metadata:?}");
    }

    #[test]
    fn the_band_never_shrinks_past_the_point_flower_stops_using_it() {
        // A third of 30 is 10, which is between the two bounds and so is taken
        // as-is; a third of 21 is 7, which is not, and is raised to the floor.
        assert_eq!(
            layout(area(80, 31), Focus::Body, true)
                .metadata
                .unwrap()
                .height,
            10
        );
        assert_eq!(
            layout(area(80, 22), Focus::Body, true)
                .metadata
                .unwrap()
                .height,
            METADATA_MIN_ROWS
        );
        // And a tall terminal spends the extra rows on the prose, not the band.
        assert_eq!(
            layout(area(80, 60), Focus::Body, true)
                .metadata
                .unwrap()
                .height,
            METADATA_MAX_ROWS
        );
    }

    #[test]
    fn a_short_terminal_gives_the_screen_to_whichever_pane_has_the_keyboard() {
        let short = area(80, 12);

        let on_body = layout(short, Focus::Body, true);
        assert!(on_body.metadata.is_none(), "no band");
        assert_eq!(
            on_body.body.expect("body").height,
            10,
            "label + body + status"
        );

        let on_metadata = layout(short, Focus::Metadata, true);
        assert!(on_metadata.body.is_none(), "no body pane");
        assert_eq!(on_metadata.metadata.expect("band").height, 11);
    }

    #[test]
    fn a_document_with_no_prose_region_is_all_metadata() {
        let panes = layout(area(80, 40), Focus::Body, false);
        assert_eq!(panes.metadata.expect("band").height, 39);
        assert!(panes.body.is_none() && panes.body_label.is_none());
    }

    /// A terminal can be one row tall, and a layout that panics there is a
    /// layout that panics on a window drag.
    #[test]
    fn a_degenerate_terminal_draws_nothing_and_does_not_panic() {
        for height in 0..=2 {
            for focus in [Focus::Body, Focus::Metadata] {
                let panes = layout(area(80, height), focus, true);
                if height <= 1 {
                    assert!(panes.metadata.is_none() && panes.body.is_none());
                }
            }
        }
    }
}
