//! What the tests in every module drive the host with: a scratch document,
//! opened the way [`run`](crate::run) opens one, and keys to press at it.

use std::path::PathBuf;

use flower_core::{Mode, Seg};
use provui_core::DocumentSession;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::{App, Flow, app, input, nav};

/// The 80×24 terminal every test in here drives.
pub const SCREEN: Rect = Rect {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

/// [`input::on_key`] with this crate's one terminal size filled in — the
/// argument only the navigation verbs use, and only to lay out the document
/// they arrive at.
pub fn on_key(session: &mut DocumentSession, app: &mut App, key: KeyEvent) -> Flow {
    input::on_key(session, app, key, SCREEN)
}

pub const DOC: &str = "\
---
# a comment nobody should lose
title: Old Title
draft: true
---
# Heading

Original body.
";

pub fn scratch(name: &str, doc: &str) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, doc).unwrap();
    path
}

/// An 80×24 terminal's worth of [`DOC`], started exactly the way [`run`](crate::run)
/// starts one.
pub fn open(name: &str) -> (PathBuf, DocumentSession, App) {
    open_with(name, DOC, SCREEN)
}

/// `doc`, opened for a terminal of `screen`'s size.
pub fn open_with(name: &str, doc: &str, screen: Rect) -> (PathBuf, DocumentSession, App) {
    let path = scratch(name, doc);
    // `Nav::none`, not `Nav::discover`: what these tests are about is this
    // host, and discovery walks the real filesystem to the root, so what it
    // finds is a property of the machine running them.
    let nav = nav::Nav::none();
    let mut session = nav.open(&path).unwrap();
    let app = App::new(&session, nav);
    app::begin(&mut session, &app, screen);
    (path, session, app)
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

pub fn typed(session: &mut DocumentSession, app: &mut App, text: &str) {
    for c in text.chars() {
        assert_eq!(on_key(session, app, key(KeyCode::Char(c))), Flow::Continue);
    }
}

/// Drive the metadata pane's keys the way a person would: walk the page to
/// the row, open it, clear it, type, commit.
pub fn retype_metadata(session: &mut DocumentSession, app: &mut App, target: &str, value: &str) {
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
