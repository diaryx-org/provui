//! The workspace this host is looking at, and moving around inside it.
//!
//! `provui-core` answers three questions and applies none of them: which
//! frontmatter keys are prov's own ([`Facets`]), which rows are links
//! ([`links`](provui_core::links)), and where a link lands
//! ([`WorkspaceView`]). This module is where one frontend turns those answers
//! into policy — and it is deliberately short, because that is the claim the
//! core is making.
//!
//! The policy, in full:
//!
//! - **Keys the workspace maintains decline edits.** `id` is minted,
//!   `content_hash` is computed, the `updated` stamp is written on save. Their
//!   rows are drawn — you should be able to *see* a document's id — and typing
//!   into one is refused. `Facets::managed_key_names` is the list; flower's
//!   `derived` set is where it goes.
//! - **prov's structure sinks below the document's own values.** `title`,
//!   `audience` and `mood` come first; `contents`, `part_of`, `id` and the
//!   `prov:` block come after. In a pane a third of the terminal wide that is
//!   the difference between seeing what the document says and scrolling past
//!   how it is wired. `Facets::structural_keys` is the list; flower's `demoted` set is
//!   where it goes.
//!
//! Both are one line each, and a frontend that wants neither writes neither.
//! That is what "less opinionated" bought: the classification is general and
//! lives once, and the arrangement is local and lives here.
//!
//! Discovery is best-effort by design. A markdown file that belongs to no
//! workspace is an ordinary markdown file, and a directory prov refuses to guess
//! a root for is a real answer about *that directory* rather than a reason this
//! editor cannot open a document. Either way the file opens, without the schema
//! and without id resolution, and the status line says which.

use std::path::{Path, PathBuf};

use provui_core::facets::Facets;
use provui_core::links::MetaLink;
use provui_core::workspace::{Destination, WorkspaceView, resolve_without_workspace};
use provui_core::{DocumentSession, SessionError, link_at};

/// What is under the cursor, when the follow key is pressed.
pub enum Follow {
    /// The metadata pane has no cursor — an empty document.
    NoCursor,
    /// There is a row, and it is not a link.
    NotALink,
    /// A link, and where it lands.
    Lands(Box<MetaLink>, Destination),
}

/// The workspace, the way back, and this host's arrangement policy.
pub struct Nav {
    workspace: Option<WorkspaceView>,
    /// The classification: the workspace's vocabulary when there is one, prov's
    /// built-in preset when there is not. A lone document is still a prov
    /// document, so `contents` still means `contents`.
    facets: Facets,
    /// Why there is no workspace, when there is none and it was not simply
    /// absent — an ambiguous root directory has something worth saying.
    complaint: Option<String>,
    /// Documents we came from, most recent last.
    back: Vec<PathBuf>,
}

impl Nav {
    /// Find the workspace `path` belongs to, or carry on without one.
    pub fn discover(path: &Path) -> Self {
        let (workspace, complaint) = match WorkspaceView::discover(path) {
            Ok(found) => (found, None),
            // Not fatal, and not silent: prov declining to guess which of two
            // candidates is the root is a fact about that directory, and a
            // reader who is missing their schema deserves to know why.
            Err(e) => (None, Some(e.to_string())),
        };
        let facets = workspace
            .as_ref()
            .map(|ws| ws.facets().clone())
            .unwrap_or_default();
        Self {
            workspace,
            facets,
            complaint,
            back: Vec::new(),
        }
    }

    /// A navigator with no workspace: prov's built-in vocabulary, no schema, and
    /// links resolved by path alone.
    ///
    /// The state a document opened outside any workspace is genuinely in, and
    /// the one a test wants when its subject is this host rather than discovery
    /// — [`discover`](Self::discover) reads the real filesystem all the way up
    /// to the root, so what it finds is a property of the machine.
    pub fn none() -> Self {
        Self {
            workspace: None,
            facets: Facets::default(),
            complaint: None,
            back: Vec::new(),
        }
    }

    /// Whether a workspace was found — which is to say whether there is a
    /// schema, and whether `id:` links can resolve.
    pub fn has_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    /// The one thing about discovery worth interrupting for: prov refusing to
    /// guess which of a directory's candidates is the root.
    ///
    /// A workspace that is simply *absent* is not in here. That is the ordinary
    /// state of a markdown file, it costs the reader nothing they did not
    /// already know, and the status line has key hints to show that a new reader
    /// needs more — [`has_workspace`](Self::has_workspace) is how the status
    /// line says it in two characters instead.
    pub fn note(&self) -> Option<String> {
        self.complaint.clone()
    }

    /// How deep the way back goes.
    pub fn depth(&self) -> usize {
        self.back.len()
    }

    /// Open a document with this host's policy applied — the whole of it.
    pub fn open(&self, path: &Path) -> Result<DocumentSession, SessionError> {
        let schema = self.workspace.as_ref().map(|ws| ws.schema_for(path));
        let mut session =
            DocumentSession::open_managed(path, schema, self.facets.managed_key_names())?;
        // After the open, not before: which structural keys this *document*
        // carries is a fact about the document, and demoting `contents` in a
        // document that has none says nothing to anybody.
        let structural = self.facets.structural_keys(session.meta());
        session.metadata_mut().set_demoted(structural);
        Ok(session)
    }

    /// Where the link under the metadata cursor would take you.
    ///
    /// Resolution only — nothing is opened and nothing is remembered, so a host
    /// can describe a link (a preview, a footer, a tooltip) with the same call
    /// that follows one.
    pub fn follow(&self, session: &DocumentSession) -> Follow {
        let Some(cursor) = session.cursor_path() else {
            return Follow::NoCursor;
        };
        let Some(link) = link_at(session.meta(), &self.facets, &cursor) else {
            return Follow::NotALink;
        };
        let landing = match &self.workspace {
            Some(ws) => ws.resolve(session.path(), &link),
            None => resolve_without_workspace(session.path(), &link),
        };
        Follow::Lands(Box::new(link), landing)
    }

    /// Open `to`, remembering `from` so [`back`](Self::back) can return.
    pub fn go(&mut self, from: &Path, to: &Path) -> Result<DocumentSession, SessionError> {
        let session = self.open(to)?;
        // Pushed only once the open succeeded: a way back to a document you are
        // still standing in is a way back to nowhere.
        self.back.push(from.to_path_buf());
        Ok(session)
    }

    /// Return to the document we came from, if there is one.
    pub fn back(&mut self) -> Option<Result<DocumentSession, SessionError>> {
        let previous = self.back.pop()?;
        match self.open(&previous) {
            Ok(session) => Some(Ok(session)),
            // The document we came from is gone or has stopped parsing. Put it
            // back on the stack: the failure is about that document, and
            // silently eating the entry would make the next press skip a step
            // with no explanation.
            Err(e) => {
                self.back.push(previous);
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flower_core::{Seg, ViewMode};

    /// The vault the follow tests walk: a root, a child, and a link that is
    /// broken on purpose.
    struct Vault(PathBuf);

    impl Vault {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("provui_tui_nav_{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("notes")).unwrap();
            std::fs::write(
                dir.join("README.md"),
                "---\ntitle: The Vault\ncontents:\n- '[A Note](notes/note.md)'\n---\n# The Vault\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("notes/note.md"),
                "---\ntitle: A Note\nid: ajp7eq\nmood: rainy\npart_of: '[The Vault](/README.md)'\nlinks:\n- '[Gone](gone.md)'\n---\n# A Note\n\nProse.\n",
            )
            .unwrap();
            Self(dir)
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    impl Drop for Vault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Put the metadata model in the shape the widget drives it in, and stand on
    /// a given row. The host does the first of these in `begin`; the tests need
    /// both.
    fn stand_on(session: &mut DocumentSession, path: &[Seg]) {
        session.metadata_mut().set_view(ViewMode::Pages);
        session.metadata_mut().focus_on(path);
        assert_eq!(
            session.cursor_path().as_deref(),
            Some(path),
            "the cursor is where the test put it"
        );
    }

    #[test]
    fn follows_the_link_under_the_cursor_and_comes_back() {
        let vault = Vault::new("follow");
        let note = vault.path("notes/note.md");
        let mut nav = Nav::discover(&note);
        assert!(nav.note().is_none(), "the vault was found");

        let mut session = nav.open(&note).expect("open the note");
        stand_on(&mut session, &[Seg::Key("part_of".into())]);

        let landed = match nav.follow(&session) {
            Follow::Lands(_, landing) => landing,
            _ => panic!("part_of is a link"),
        };
        let target = landed
            .openable()
            .expect("the root is on disk")
            .to_path_buf();

        let root = nav.go(&note, &target).expect("follow to the root");
        assert!(root.path().ends_with("README.md"));
        assert_eq!(nav.depth(), 1);

        let returned = nav.back().expect("a way back").expect("reopened");
        assert!(returned.path().ends_with("note.md"));
        assert_eq!(nav.depth(), 0);
        assert!(nav.back().is_none(), "and no further");
    }

    /// The two answers that are not "a document opened": a row that is not a
    /// link, and a link whose target is not there.
    #[test]
    fn says_what_happened_when_nothing_opens() {
        let vault = Vault::new("nothing");
        let note = vault.path("notes/note.md");
        let nav = Nav::discover(&note);
        let mut session = nav.open(&note).expect("open");

        stand_on(&mut session, &[Seg::Key("mood".into())]);
        assert!(matches!(nav.follow(&session), Follow::NotALink));

        stand_on(&mut session, &[Seg::Key("links".into()), Seg::Index(0)]);
        match nav.follow(&session) {
            Follow::Lands(_, landing) => {
                assert!(landing.openable().is_none(), "gone.md is not there");
                assert!(landing.describe().contains("not on disk"), "{landing:?}");
            }
            _ => panic!("a broken link is still a link"),
        }
    }

    /// This host's whole arrangement policy, checked where it lands: `id`
    /// declines edits, and prov's structure sits below the document's own
    /// values.
    #[test]
    fn a_document_opens_with_this_hosts_policy_applied() {
        let vault = Vault::new("policy");
        let note = vault.path("notes/note.md");
        let nav = Nav::discover(&note);
        let mut session = nav.open(&note).expect("open");

        assert!(
            session.metadata().is_derived(&[Seg::Key("id".into())]),
            "the workspace maintains `id`"
        );
        assert!(
            !session.metadata().is_derived(&[Seg::Key("mood".into())]),
            "and nothing else"
        );
        assert!(
            session.metadata().is_demoted(&[Seg::Key("part_of".into())]),
            "prov's structure sinks"
        );
        assert!(
            !session.metadata().is_demoted(&[Seg::Key("title".into())]),
            "what the document says does not"
        );

        // The row is still there to read, and still refuses to be typed into.
        session
            .metadata_mut()
            .set_scalar_text(&[Seg::Key("id".into())], "typed");
        assert!(!session.dirty());
    }

    /// A file that belongs to no workspace still opens, and the status line says
    /// what is missing rather than the editor refusing.
    #[test]
    fn a_lone_document_opens_without_a_workspace() {
        let dir = std::env::temp_dir().join("provui_tui_nav_lone");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lone = dir.join("lone.md");
        std::fs::write(
            &lone,
            "---\ntitle: Lone\nlinks:\n- 'beside.md'\n---\n# Lone\n",
        )
        .unwrap();
        std::fs::write(dir.join("beside.md"), "# Beside\n").unwrap();

        // Whatever discovery made of the ancestors, the document opens and
        // prov's own vocabulary still applies to it.
        let nav = Nav::discover(&lone);
        let mut session = nav.open(&lone).expect("a lone file still opens");
        stand_on(&mut session, &[Seg::Key("links".into()), Seg::Index(0)]);
        assert!(
            matches!(nav.follow(&session), Follow::Lands(..)),
            "`links` is a relation even with nothing configured"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
