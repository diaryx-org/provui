//! Layout and drawing: where the two panes go, and the one status line the host
//! owns.
//!
//! Both widgets render into a `Rect` and neither knows the other exists, so
//! everything shared between them — the split, the focus cue, the file name, the
//! terminal's cursor — is decided here. So is the one thing a widget cannot do
//! for a mouse it never sees: [`metadata_hit`] says which of flower's rows a
//! point in its pane is standing on.

use flower_core::{Backend, Mode, Model, Page};
use provui_core::DocumentSession;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};

use crate::{App, FOCUS_CHORD, FOLLOW_CHORD, Focus};

/// The two host chords, written once so the hint list is a list of `&str`.
static PANE_HINT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("{FOCUS_CHORD} pane"));
static FOLLOW_HINT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("{FOLLOW_CHORD} follow"));

/// The narrowest metadata pane worth drawing.
///
/// A page row is an indent, a key, a gap, and the value flushed right, and
/// flower truncates the value before it squeezes the key. Below this the value
/// is an ellipsis after every key longer than a word, which is a pane showing
/// what the keys are called and not what they say.
const METADATA_MIN_COLS: u16 = 30;

/// The widest metadata pane worth drawing.
///
/// Frontmatter is keys and short values; past this the pane is drawing pad
/// between the two columns. A wide terminal spends the rest on the prose.
/// Comfortably above the 64 columns at which flower splits its own page view
/// in two, so a wide terminal gets that view and a narrow one gets the
/// single-pane layout it would have had on a phone.
const METADATA_MAX_COLS: u16 = 80;

/// A body pane narrower than this is not an editor, it is a slot. Below the
/// point where both panes clear their minimum, the split is abandoned rather
/// than shrunk (see [`layout`]).
const BODY_MIN_COLS: u16 = 40;

/// The two-pane threshold flower-ratatui applies to the pane it is given.
/// Restated here because [`metadata_hit`] has to draw the same line the widget
/// draws, and the widget does not export it.
const FLOWER_TWO_PANE_MIN_WIDTH: u16 = 64;

/// Where each piece of the screen went this frame.
///
/// `None` means "not drawn": the metadata pane is absent when a narrow terminal
/// gave the screen to the body, and the body is absent both when the document
/// has no prose region at all and when the metadata pane took the screen. The
/// divider is there exactly when both panes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panes {
    pub body_label: Option<Rect>,
    pub body: Option<Rect>,
    pub divider: Option<Rect>,
    pub metadata: Option<Rect>,
    pub status: Rect,
}

/// Split `area` into the body pane on the left, the metadata pane on the
/// right, and the status line.
///
/// Side by side, with the prose leading: the body is the document and reads
/// left to right, and the frontmatter is what is true about it, which is what a
/// sidebar is for. The metadata pane takes a third of the width, bounded to
/// keep a row readable on a narrow terminal and to stop a wide one drawing pad
/// between keys and values; the body gets everything the pane and the divider
/// do not. Both get the full height, which is the dimension a page of metadata
/// actually spends — flower's inline budget is refit to the pane's height every
/// frame, so a tall terminal draws the whole document with nothing to drill
/// into.
///
/// The cost is known: flower's own two-pane page view wants 64 columns, and a
/// third of an ordinary terminal is not that. It gets the single-pane layout
/// instead, which is the same interaction in one column, and the split view
/// back from about 190 columns. When the terminal is too narrow for both
/// minimums the split is abandoned rather than shrunk, and the pane holding the
/// keyboard takes the screen — which is why `focus` is an argument to a layout
/// function.
pub fn layout(area: Rect, focus: Focus, has_body: bool) -> Panes {
    let [rest, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    let nothing = Panes {
        body_label: None,
        body: None,
        divider: None,
        metadata: None,
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

    let cols = (rest.width / 3).clamp(METADATA_MIN_COLS, METADATA_MAX_COLS);
    if rest.width < BODY_MIN_COLS + 1 + cols {
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

    let [left, divider, metadata] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(cols),
    ])
    .areas(rest);
    let (label, body) = body_pane(left);
    Panes {
        body_label: Some(label),
        body: Some(body),
        divider: Some(divider),
        metadata: Some(metadata),
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

    if let Some(divider) = panes.divider {
        f.render_widget(
            Block::new().borders(Borders::LEFT).border_style(dim()),
            divider,
        );
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
        // The picker puts what has been typed on its own box title and draws a
        // `›` against the row it is on, so there is no caret of the widget's to
        // sit on and no box position the host is told. Park it where a normal
        // page parks it.
        Mode::Choosing { .. } | Mode::Normal => 0,
    };
    let x = metadata.x + x.min(metadata.width - 1);
    f.set_cursor_position(Position::new(x, metadata.bottom() - 1));
}

// ── the metadata pane, from the outside ──────────────────────────────────────

/// What a point in the metadata pane is standing on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataHit {
    /// Row `i` of the page the cursor is on.
    Row(usize),
    /// Row `i` of the page one level out — the left pane, while the cursor's
    /// page is on the right.
    ParentRow(usize),
    /// Row `i` of the page the cursor would open — the right pane, while the
    /// cursor's page leads the split and the right one is a preview.
    PeekRow(usize),
}

/// Which of flower's rows is drawn under `at`, in a metadata pane drawn into
/// `metadata`. `None` for the chrome — header, breadcrumb, footer — and for
/// the empty space under a short list.
///
/// flower-ratatui draws from a `Rect` and remembers nothing about where its
/// rows went, and it takes no mouse events, so the host has to map a click the
/// way `draw_in` laid the pane out: a header row and a footer row, and between
/// them one page pane or two — each a breadcrumb over a list that scrolls only
/// as far as it must to keep its selection on screen. Those are the widget's
/// constants (one row of chrome at each end, two even panes from 64 columns,
/// a list that starts at the top every frame), restated here. A widget that
/// moves them moves this too; the draw tests in `main.rs` are where that
/// would show.
pub fn metadata_hit<B: Backend>(
    metadata: Rect,
    model: &Model<B>,
    at: Position,
) -> Option<MetadataHit> {
    // The same three bands `draw_in` cuts: header, pages, footer.
    let [_header, pages, _footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(metadata);
    if !pages.contains(at) {
        return None;
    }

    let current = |pane: Rect| {
        row_in(pane, model.page(), Some(model.page_selected()), at).map(MetadataHit::Row)
    };

    if pages.width < FLOWER_TWO_PANE_MIN_WIDTH || model.pages_would_degenerate() {
        return current(pages);
    }

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(pages);

    if model.page_leads_the_split() {
        if left.contains(at) {
            return current(left);
        }
        let peek = model.peek_page()?;
        return row_in(right, &peek, None, at).map(MetadataHit::PeekRow);
    }

    if left.contains(at) {
        let parent = model.parent_page();
        let came_from = parent.position_of(model.focus());
        return row_in(left, parent, came_from, at).map(MetadataHit::ParentRow);
    }
    current(right)
}

/// Which item of `page` is drawn at `at`, in a pane that is a breadcrumb row
/// over a list highlighting `selected`.
fn row_in(pane: Rect, page: &Page, selected: Option<usize>, at: Position) -> Option<usize> {
    let [_crumb, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(pane);
    if !list.contains(at) || page.is_empty() {
        return None;
    }
    // ratatui's `List` is given a fresh state every frame, so it starts at the
    // top and scrolls forward exactly as far as it must to bring the selection
    // on screen. Every item is one row, so that is the selection less the last
    // visible row — and nothing at all while the page fits, which a page sized
    // by `fit_to_room` almost always does.
    let height = list.height as usize;
    let offset = selected.map_or(0, |s| s.saturating_sub(height - 1));
    let index = offset + (at.y - list.y) as usize;
    (index < page.items.len()).then_some(index)
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The key hints, trimmed to what is actually left of the line.
///
/// A `Line` clips silently on the right, so a hint set that overflows loses its
/// tail without saying so — and the tail is where `^Q quit` is, which is the one
/// hint a reader who is stuck actually needs. Dropping from the **front**
/// instead gives up the most guessable chords first: everyone knows `^S` saves,
/// and nobody guesses that `^G` follows a link.
///
/// The list is the same in both panes, because the keys now are: `^G` follows
/// the metadata row in one and the link under the caret in the other, so there
/// is no pane it means nothing in. `focus` stays an argument because the list
/// being per pane is a property of *this* host worth keeping cheap to restore.
fn hints(focus: Focus, room: usize) -> Option<String> {
    let _ = focus;
    // Least worth keeping first — the order this drops in.
    let mut hints = vec!["^S save"];
    hints.push(&FOLLOW_HINT);
    hints.push(&PANE_HINT);
    hints.push("^Q quit");

    while !hints.is_empty() {
        let line = hints.join(" · ");
        if line.chars().count() <= room {
            return Some(line);
        }
        hints.remove(0);
    }
    None
}

/// The one line the host owns: which file, which pane has the keyboard, whether
/// there is anything unsaved, and how to change the first two of those.
fn status_line(f: &mut Frame, area: Rect, app: &App, session: &DocumentSession) {
    let mut spans = Vec::new();

    // Two characters for the fact that changes the most: in a workspace there is
    // a schema behind the pickers and `id:` links resolve, and outside one
    // neither is true. It goes here rather than in a message because it is
    // constant for the session, and a message that never changes is a message a
    // reader stops seeing.
    if app.nav.has_workspace() {
        spans.push(Span::styled(" ⌂", Style::default().fg(Color::Cyan)));
    }
    spans.push(Span::styled(
        format!(" {} ", app.name),
        Style::default().add_modifier(Modifier::BOLD),
    ));

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

    // A count, not a list: the findings themselves are already *placed* — the
    // body's are washed under the prose by leaf, the metadata's are marked and
    // spelled out by flower — so what the line owes a reader here is only that
    // there is something to go and look at.
    let findings = session.findings().len();
    if findings > 0 {
        spans.push(Span::styled(
            format!(
                " ⚠ {findings} finding{}",
                if findings == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Red),
        ));
    }

    // The hints and a message are alternatives rather than neighbours: together
    // they overflow 80 columns, and a `Line` clips on the right — so keeping
    // both would mean losing the end of whichever came last, silently. When
    // there is something to say, saying it is worth more than repeating keys
    // that have not moved.
    //
    // Two things want the tail of the line, not three: what is wrong with the
    // metadata row under the cursor used to be drawn here, and is now flower's
    // own — the widget marks the row and puts the message in its footer. What
    // is left is what just happened, and the standing hints, which are the only
    // one a reader can get back by pressing nothing.
    match &app.status {
        Some(status) => {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!(" {status} "),
                Style::default().fg(Color::Black).bg(Color::Green),
            ));
        }
        None => {
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            let room = (area.width as usize).saturating_sub(used + 2);
            if let Some(hints) = hints(app.focus, room) {
                spans.push(Span::styled(format!("  {hints}"), dim()));
            }
        }
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
    fn a_roomy_terminal_puts_the_body_left_and_the_metadata_right() {
        let panes = layout(area(120, 40), Focus::Body, true);
        let metadata = panes.metadata.expect("metadata pane");
        let body = panes.body.expect("body pane");
        let label = panes.body_label.expect("body label");
        let divider = panes.divider.expect("divider");

        assert_eq!(metadata.width, 40, "a third of the width");
        assert_eq!(divider.width, 1);
        assert_eq!(body.width, 79, "the body gets the rest");
        assert!(body.right() <= divider.x && divider.right() <= metadata.x);
        assert_eq!(panes.status.height, 1);
        // Every column and every row is spoken for.
        assert_eq!(body.width + divider.width + metadata.width, 120);
        assert_eq!(label.height + body.height + panes.status.height, 40);
        assert_eq!(metadata.height + panes.status.height, 40);
        // The body is the primary surface, and on a roomy terminal it says so.
        assert!(body.width > metadata.width, "{body:?} vs {metadata:?}");
    }

    #[test]
    fn the_metadata_pane_stays_between_its_bounds() {
        // A third of 80 is 26, which is not enough for a row, and is raised.
        assert_eq!(
            layout(area(80, 24), Focus::Body, true)
                .metadata
                .unwrap()
                .width,
            METADATA_MIN_COLS
        );
        // A third of 150 is 50, which is between the bounds and is taken as-is.
        assert_eq!(
            layout(area(150, 24), Focus::Body, true)
                .metadata
                .unwrap()
                .width,
            50
        );
        // And a very wide terminal spends the extra columns on the prose.
        assert_eq!(
            layout(area(300, 24), Focus::Body, true)
                .metadata
                .unwrap()
                .width,
            METADATA_MAX_COLS
        );
    }

    #[test]
    fn a_narrow_terminal_gives_the_screen_to_whichever_pane_has_the_keyboard() {
        let narrow = area(60, 24);

        let on_body = layout(narrow, Focus::Body, true);
        assert!(on_body.metadata.is_none() && on_body.divider.is_none());
        assert_eq!(on_body.body.expect("body").width, 60);
        assert_eq!(on_body.body.unwrap().height, 22, "label + body + status");

        let on_metadata = layout(narrow, Focus::Metadata, true);
        assert!(on_metadata.body.is_none() && on_metadata.divider.is_none());
        let metadata = on_metadata.metadata.expect("metadata");
        assert_eq!((metadata.width, metadata.height), (60, 23));
    }

    /// The hint list gives up its most guessable entries first, so the way out
    /// is the last thing to go rather than the first — which is what silent
    /// right-edge clipping would have done instead.
    #[test]
    fn the_hints_drop_the_guessable_chords_before_the_one_nobody_guesses() {
        let full = hints(Focus::Metadata, 80).expect("a roomy line");
        assert!(full.starts_with("^S save"), "everything, in order: {full}");
        assert!(full.contains(FOLLOW_CHORD) && full.ends_with("^Q quit"));

        // Room for three of the four: the save hint goes, because everybody
        // already knows it.
        let room = full.chars().count() - 1;
        let trimmed = hints(Focus::Metadata, room).expect("still something");
        assert!(!trimmed.contains("^S save"), "{trimmed}");
        assert!(trimmed.contains(FOLLOW_CHORD), "{trimmed}");
        assert!(trimmed.ends_with("^Q quit"), "{trimmed}");

        // Whatever the room, what is shown fits it — that is the whole promise.
        for room in 0..=full.chars().count() {
            for focus in [Focus::Body, Focus::Metadata] {
                if let Some(line) = hints(focus, room) {
                    assert!(line.chars().count() <= room, "{room}: {line}");
                }
            }
        }
        assert!(hints(Focus::Body, 3).is_none(), "no room, nothing shown");
    }

    #[test]
    fn a_document_with_no_prose_region_is_all_metadata() {
        let panes = layout(area(80, 40), Focus::Body, false);
        let metadata = panes.metadata.expect("metadata");
        assert_eq!((metadata.width, metadata.height), (80, 39));
        assert!(panes.body.is_none() && panes.body_label.is_none());
        assert!(panes.divider.is_none());
    }

    /// A terminal can be one row tall or one column wide, and a layout that
    /// panics there is a layout that panics on a window drag.
    #[test]
    fn a_degenerate_terminal_draws_nothing_and_does_not_panic() {
        for (width, height) in [(80, 0), (80, 1), (80, 2), (0, 24), (1, 24), (0, 0)] {
            for focus in [Focus::Body, Focus::Metadata] {
                let panes = layout(area(width, height), focus, true);
                if height <= 1 {
                    assert!(panes.metadata.is_none() && panes.body.is_none());
                }
                if width <= 1 {
                    assert!(panes.divider.is_none(), "no room for a split at {width}");
                }
            }
        }
    }
}
