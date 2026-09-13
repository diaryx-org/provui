//! Following a link — the one step that needs a workspace to take it in.
//!
//! [`crate::links`] says a metadata row *is* a link and what it says; this
//! module says where that link lands. The two are separate because the second
//! costs something the first does not: a root to resolve `/`-absolute targets
//! from, a registry to turn `id:ajp7eq` into a path, and the filesystem to say
//! whether the answer is actually there. A frontend that only draws links
//! differently should not pay for any of it.
//!
//! [`WorkspaceView`] is prov's read surface with the pieces an editor needs
//! resolved once and kept: the effective config, the vocabularies its controlled
//! fields point at, the [`Facets`] its vocabulary implies, and the
//! [`flower_core::Schema`] a content document is edited under. It is a
//! *view* — the name is the promise. Nothing here writes.
//!
//! ## Read-only, and why that is not a temporary state
//!
//! Following a link reads. **Editing** one does not: a relation field is half of
//! a pair prov maintains bidirectionally, so writing `contents` in one document
//! means writing `part_of` in another, and that is prov's `mutate` layer rather
//! than its `edit` layer. This crate's metadata backend edits one document's
//! bytes and has no way to touch a second, which is exactly why the scope line
//! is where it is. So a frontend may follow a link with what is here and must
//! not conclude it can retarget one.
//!
//! prov's read surface is async over a filesystem port. Nothing here is, because
//! an editor's "open the document under the cursor" is a foreground action with
//! nothing to overlap: each entry point blocks with prov's own
//! [`prov::block_on`], which is the same executor prov's CLI uses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use flower_core::Schema;
use prov::index::FileIndex;
use prov::{
    Backlink, Discovery, Settings, StdFs, Target, Vocabulary, Workspace, WorkspaceConfig, block_on,
    discover,
};

use crate::facets::Facets;
use crate::links::MetaLink;
use crate::session::{DocumentSession, SessionError};

fn we(e: impl std::fmt::Display) -> SessionError {
    SessionError(e.to_string())
}

/// Where a link lands.
///
/// prov's own [`Target`] is the resolution; this adds the two things a frontend
/// about to open a file needs and `Target` deliberately does not carry — an
/// absolute path rather than a workspace-relative one, and whether the file is
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A document in this workspace, at an absolute path.
    ///
    /// `exists` is `false` for a broken link. It is still a `Document` and not
    /// an error case: the target is well-formed and names a place in this
    /// workspace, and an editor's right answer to a broken link is usually to
    /// say so and offer to create it, not to refuse to describe it.
    Document {
        /// Absolute, ready to open.
        path: PathBuf,
        /// Whether a file is actually there.
        exists: bool,
    },
    /// A `#locator` alone — a place inside the document the link is written in.
    /// prov does not read a document's internal address space, so where that
    /// place is, is the frontend's question to answer.
    SameDocument,
    /// A URL or mail address. Off-workspace by construction: prov recognizes it
    /// by syntax and never resolves it.
    External(String),
    /// `id:<workspace>/<id>` — a document named in another workspace. prov holds
    /// no map from a workspace name to a location (that map is a property of the
    /// device, not of the archive), so this is as far as resolution goes.
    Foreign {
        /// The workspace qualifier, as written.
        workspace: String,
        /// The id within it, as written — never check-verified, since that
        /// workspace owns its id space.
        id: String,
    },
    /// An `id:` target with no live registry entry: unknown, tombstoned, or a
    /// workspace with no registry at all.
    UnresolvedId(String),
    /// A nominal (`[[My File]]`) target several documents claim, so it names no
    /// one of them.
    AmbiguousAlias(String),
    /// The target is well-formed but cannot be resolved from here, with the
    /// reason — a `/`-absolute path asked about outside any workspace, say.
    Unresolvable {
        /// The target as written.
        target: String,
        /// Why, in a sentence a status line can show.
        why: String,
    },
}

impl Destination {
    /// The file to open, when there is one that exists.
    pub fn openable(&self) -> Option<&Path> {
        match self {
            Destination::Document { path, exists: true } => Some(path),
            _ => None,
        }
    }

    /// A one-line description, for a status line that has to say what happened
    /// when nothing opened.
    pub fn describe(&self) -> String {
        match self {
            Destination::Document { path, exists: true } => path.display().to_string(),
            Destination::Document {
                path,
                exists: false,
            } => format!("{} — not on disk", path.display()),
            Destination::SameDocument => "a place inside this document".to_string(),
            Destination::External(url) => format!("{url} — outside the workspace"),
            Destination::Foreign { workspace, id } => {
                format!("{id} in workspace `{workspace}` — not locatable from here")
            }
            Destination::UnresolvedId(id) => format!("id {id} — no registry entry"),
            Destination::AmbiguousAlias(name) => {
                format!("`{name}` — several documents claim that name")
            }
            Destination::Unresolvable { target, why } => format!("{target} — {why}"),
        }
    }
}

/// One workspace, opened for reading, with everything an editor resolves once.
pub struct WorkspaceView {
    ws: Workspace<StdFs, prov::identity::NoIdentity, FileIndex>,
    /// The root document, workspace-relative — what every pointer resolves from.
    root_doc: PathBuf,
    /// The config document the root points at, workspace-relative, when it has
    /// one. Kept because a document that *is* the config is edited under a
    /// different schema than the content around it.
    config_doc: Option<PathBuf>,
    config: WorkspaceConfig,
    vocabularies: BTreeMap<String, Vocabulary>,
    facets: Facets,
}

impl WorkspaceView {
    /// Find the workspace `from` belongs to and open it for reading.
    ///
    /// `from` may be a file or a directory; a file's directory is where the walk
    /// up starts. `Ok(None)` when no ancestor holds a root document, which is
    /// the ordinary state of a markdown file that is simply a markdown file —
    /// not an error, and a frontend should carry on without a workspace rather
    /// than refuse to open it.
    ///
    /// A directory holding two root candidates and no `index`/`readme` to break
    /// the tie *is* an error: prov will not guess which is the root, and neither
    /// should this.
    pub fn discover(from: &Path) -> Result<Option<Self>, SessionError> {
        let start = starting_dir(from)?;
        match block_on(discover(&StdFs, &start)).map_err(we)? {
            Discovery::Found(found) => Ok(Some(Self::open(
                found.root_dir,
                found.root_doc,
                found.config,
            )?)),
            Discovery::NotFound => Ok(None),
            Discovery::Ambiguous { dir, candidates } => Err(SessionError(format!(
                "{} holds {} root candidates and no index/readme to choose between them: {}",
                dir.display(),
                candidates.len(),
                candidates.join(", ")
            ))),
        }
    }

    /// Open a workspace whose root and config are already known — the path a
    /// caller that did its own discovery takes, and what
    /// [`discover`](Self::discover) calls.
    pub fn open(
        root_dir: impl Into<PathBuf>,
        root_doc: impl Into<PathBuf>,
        config: WorkspaceConfig,
    ) -> Result<Self, SessionError> {
        let root_dir = root_dir.into();
        let root_doc = prov::link::normalize(root_doc.into());

        // Every policy knob at once: the relation vocabulary, the reference
        // style, the embedding pair, fixity, id storage, what the workspace calls
        // itself. Threading them one at a time is what `Settings` exists to stop.
        let probe: Workspace<StdFs> = Workspace::builder(StdFs).root(&root_dir).build();
        let index = load_registry(&probe, &root_doc, &config)?;
        let ws = Workspace::builder(StdFs)
            .root(&root_dir)
            .settings(Settings::from(&config))
            .index(index)
            .build();

        let config_doc = block_on(ws.config_path(&root_doc)).map_err(we)?;
        let vocabularies = load_vocabularies(&ws, &root_doc, &config);
        let facets = Facets::from_config(&config);
        Ok(Self {
            ws,
            root_doc,
            config_doc,
            config,
            vocabularies,
            facets,
        })
    }

    /// The workspace root directory — the absolute path every relative one here
    /// is joined to.
    pub fn root_dir(&self) -> &Path {
        self.ws.root()
    }

    /// The root document, absolute.
    pub fn root_document(&self) -> PathBuf {
        self.ws.fs_path(&self.root_doc)
    }

    /// The config document the root points at, absolute, when there is one. A
    /// workspace that keeps all its policy in the root's `prov:` block has none.
    pub fn config_document(&self) -> Option<PathBuf> {
        self.config_doc.as_ref().map(|rel| self.ws.fs_path(rel))
    }

    /// The effective config — defaults, overlaid by the root's `prov:` block,
    /// overlaid by the config document.
    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
    }

    /// The classification this workspace's vocabulary implies. See
    /// [`crate::facets`] for why nothing acts on it.
    pub fn facets(&self) -> &Facets {
        &self.facets
    }

    /// The vocabularies the controlled fields point at, keyed by field name. A
    /// vocabulary that failed to load is simply absent.
    pub fn vocabularies(&self) -> &BTreeMap<String, Vocabulary> {
        &self.vocabularies
    }

    /// prov's read surface, for a frontend that wants more of it than this view
    /// exposes — a tree, a census, a title index.
    pub fn prov(&self) -> &Workspace<StdFs, prov::identity::NoIdentity, FileIndex> {
        &self.ws
    }

    /// The schema a **content** document in this workspace is edited under.
    pub fn content_schema(&self) -> Schema {
        crate::schema_from_config(&self.config, &self.vocabularies)
    }

    /// The schema `path` is edited under: the config-document schema for the
    /// document that *is* this workspace's config, and the content schema for
    /// everything else.
    ///
    /// A fact about which document this is, not a preference. `prov.yaml` is a
    /// document whose keys are policy, and editing it under the content schema
    /// would offer term pickers for fields it does not have and none for the
    /// ones it does.
    pub fn schema_for(&self, path: &Path) -> Schema {
        match self.config_document() {
            Some(config_doc) if same_file(&config_doc, path) => crate::config_schema(&self.config),
            _ => self.content_schema(),
        }
    }

    /// Open a document in this workspace as a [`DocumentSession`], under the
    /// schema [`schema_for`](Self::schema_for) picks.
    ///
    /// The schema and **nothing else** — no keys are made read-only and no rows
    /// are sunk, because both of those are the frontend's to decide (see
    /// [`crate::facets`]). Most editors want at least the first, which is three
    /// lines rather than one:
    ///
    /// ```ignore
    /// let facets = view.facets();
    /// let schema = Some(view.schema_for(&path));
    /// let mut session = DocumentSession::open_managed(&path, schema, facets.managed_key_names())?;
    /// session.metadata_mut().set_demoted(facets.structural_keys(session.meta()));
    /// ```
    pub fn open_document(&self, path: impl AsRef<Path>) -> Result<DocumentSession, SessionError> {
        let path = self.absolute(path.as_ref());
        let schema = self.schema_for(&path);
        DocumentSession::open_with_schema(path, schema)
    }

    /// Resolve a link written in the document at `doc` (absolute or
    /// workspace-relative).
    ///
    /// Path targets and `id:` handles resolve; a nominal (`[[My File]]`) target
    /// does not, because resolving one needs a title index over the whole
    /// workspace and that is a scan an editor should not do behind a keystroke.
    /// Use [`resolve_nominal`](Self::resolve_nominal) to pay for it deliberately.
    pub fn resolve(&self, doc: &Path, link: &MetaLink) -> Destination {
        self.destination(self.ws.resolve_link(&self.relative(doc), &link.link), link)
    }

    /// [`resolve`](Self::resolve), also resolving nominal targets against a
    /// title index built by walking the workspace.
    ///
    /// Separate because of what it costs: the index is a scan from the root, and
    /// a `[[My File]]` link is the only kind that needs one. A frontend that
    /// follows links from the keyboard should call [`resolve`](Self::resolve)
    /// first and fall back to this only when it comes back
    /// [`Unresolvable`](Destination::Unresolvable).
    pub fn resolve_nominal(
        &self,
        doc: &Path,
        link: &MetaLink,
    ) -> Result<Destination, SessionError> {
        let index = block_on(self.ws.title_index()).map_err(we)?;
        let target = self
            .ws
            .resolve_link_with(&self.relative(doc), &link.link, Some(&index));
        Ok(self.destination(target, link))
    }

    /// Every inbound reference to `target`, walked from the workspace root.
    ///
    /// prov keeps no stored backlink index — this is the census inverted, so it
    /// is always fresh and always a walk. Worth it on demand ("what points at
    /// this?"), not on every frame.
    pub fn backlinks_to(&self, target: impl AsRef<Path>) -> Result<Vec<Backlink>, SessionError> {
        let target = self.relative(target.as_ref());
        block_on(self.ws.backlinks_to(&self.root_doc, &target)).map_err(we)
    }

    /// Turn prov's resolution into a destination: absolute, checked against the
    /// filesystem, and — for the cases prov answers by *kind* rather than by
    /// value — carrying the target the link was written with.
    ///
    /// `Target::External` is the one that needs the link: prov recognizes a URL
    /// by syntax and has nothing further to say about it, so its answer is the
    /// bare fact of externality. A status line that then has to tell a reader
    /// their link went nowhere has nothing to name.
    fn destination(&self, target: Target, link: &MetaLink) -> Destination {
        match target {
            Target::Path(rel) => {
                let path = self.ws.fs_path(&rel);
                let exists = path.is_file();
                Destination::Document { path, exists }
            }
            Target::SameDocument => Destination::SameDocument,
            Target::External => Destination::External(link.target().to_string()),
            Target::Foreign { workspace, id } => Destination::Foreign {
                workspace,
                id: id.to_string(),
            },
            Target::UnresolvedId(id) => Destination::UnresolvedId(id.to_string()),
            Target::AmbiguousAlias(name) => Destination::AmbiguousAlias(name),
        }
    }

    /// `path` as this workspace sees it: relative to the root, normalized.
    fn relative(&self, path: &Path) -> PathBuf {
        let rel = path.strip_prefix(self.ws.root()).unwrap_or(path);
        prov::link::normalize(rel)
    }

    fn absolute(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.ws.fs_path(path)
        }
    }
}

/// Where a link points with **no workspace to resolve it in** — the lexical
/// floor, for a document opened on its own.
///
/// A relative target resolves against the document's own directory, which needs
/// nothing but the two strings. A `/`-absolute target does not: it is relative
/// to a workspace root, and there is no root, so this says so rather than
/// guessing at the filesystem root or at the document's directory — both of
/// which would sometimes open the wrong file, silently. An `id:` target is the
/// same story with a registry in place of a root.
pub fn resolve_without_workspace(doc: &Path, link: &MetaLink) -> Destination {
    use crate::links::TargetKind;

    match &link.kind {
        TargetKind::SameDocument => Destination::SameDocument,
        TargetKind::External => Destination::External(link.target().to_string()),
        TargetKind::Foreign { workspace } => Destination::Foreign {
            workspace: workspace.clone(),
            id: link.target().to_string(),
        },
        TargetKind::Id => Destination::Unresolvable {
            target: link.target().to_string(),
            why: "an id needs the workspace's registry to resolve".to_string(),
        },
        TargetKind::MalformedId => Destination::Unresolvable {
            target: link.target().to_string(),
            why: "an `id:` target with no id in it".to_string(),
        },
        TargetKind::Path if link.target().starts_with('/') => Destination::Unresolvable {
            target: link.target().to_string(),
            why: "a workspace-absolute path needs a workspace root".to_string(),
        },
        TargetKind::Path => {
            let path = prov::link::resolve(doc, link.target());
            let exists = path.is_file();
            Destination::Document { path, exists }
        }
    }
}

/// The directory a discovery walk starts from: the file's own for a file, the
/// directory itself for a directory, and absolute either way — the walk goes up
/// through `ancestors()`, which a relative path exhausts in one step.
fn starting_dir(from: &Path) -> Result<PathBuf, SessionError> {
    let absolute = if from.is_absolute() {
        from.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| SessionError(format!("resolving {}: {e}", from.display())))?
            .join(from)
    };
    Ok(if absolute.is_dir() {
        absolute
    } else {
        absolute.parent().map(Path::to_path_buf).unwrap_or(absolute)
    })
}

/// The registry the root declares, parsed, or an empty one in the workspace's
/// metadata format.
///
/// An empty index is not a failure: a workspace that stores ids in frontmatter
/// alone keeps no registry document at all, and one that has not bootstrapped
/// its registry yet is an ordinary new workspace. What it costs is that `id:`
/// targets come back [`UnresolvedId`](Destination::UnresolvedId), which is the
/// truth about them.
fn load_registry(
    probe: &Workspace<StdFs>,
    root_doc: &Path,
    config: &WorkspaceConfig,
) -> Result<FileIndex, SessionError> {
    let empty = || FileIndex::new(config.default_embed_format);
    let Some(rel) = block_on(probe.registry_path(root_doc)).map_err(we)? else {
        return Ok(empty());
    };
    match block_on(probe.read_text(&rel)) {
        Ok(text) => FileIndex::parse(&rel, &text).map_err(we),
        // A declared registry that is not there yet is a workspace mid-setup,
        // not a workspace that cannot be opened. Nothing an editor does depends
        // on it beyond `id:` resolution, which then honestly reports nothing.
        Err(_) => Ok(empty()),
    }
}

/// Load every controlled field's vocabulary, keyed by field name.
///
/// A vocabulary that does not load is left out rather than raised: it means the
/// pointer is broken or the store is malformed, which is `prov check`'s finding
/// to report and not a reason an editor cannot open a file. The field then
/// reaches the schema as an enum with no offered terms — which, for a closed
/// field, rejects everything, and that is the honest signal that its vocabulary
/// is missing.
fn load_vocabularies(
    ws: &Workspace<StdFs, prov::identity::NoIdentity, FileIndex>,
    root_doc: &Path,
    config: &WorkspaceConfig,
) -> BTreeMap<String, Vocabulary> {
    let mut loaded = BTreeMap::new();
    for (field, spec) in crate::schema::workspace_fields(config) {
        let Some(pointer) = spec.vocabulary.as_deref() else {
            continue;
        };
        // A reified vocabulary's terms are documents down the spanning tree, not
        // rows in a flat store, so it is read a different way. What makes it
        // reified is the declaration, not anything the target says about itself.
        let vocabulary = if spec.reify {
            block_on(ws.load_reified_vocabulary(root_doc, field, spec))
        } else {
            block_on(ws.load_vocabulary(root_doc, pointer))
        };
        if let Ok(Some(vocabulary)) = vocabulary {
            loaded.insert(field.clone(), vocabulary);
        }
    }
    loaded
}

/// Whether two absolute paths name the same file, comparing normalized paths and
/// falling back to the filesystem's own answer where it can give one.
fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::links_in;
    use flower_core::Seg;

    /// A three-document workspace on disk: a root, a child it contains, and a
    /// config document it points at.
    struct Vault(PathBuf);

    impl Vault {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("provui_workspace_{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("notes")).unwrap();
            std::fs::write(
                dir.join("README.md"),
                "---\ntitle: The Vault\nconfig: prov.yaml\ncontents:\n- '[A Note](notes/note.md)'\n---\n# The Vault\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("prov.yaml"),
                "title: vault config\nfields:\n  audience:\n    vocabulary: audiences.yaml\n    values: closed\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("audiences.yaml"),
                "title: Audiences\nvocabulary:\n  field: audience\n  values: closed\nterms:\n  public:\n    means: Anyone\n  private: {}\n",
            )
            .unwrap();
            std::fs::write(
                dir.join("notes/note.md"),
                "---\ntitle: A Note\npart_of: '[The Vault](/README.md)'\nlinks:\n- '[Missing](gone.md)'\n- 'https://example.com/'\naudience: public\n---\n# A Note\n",
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

    fn link_at(view: &WorkspaceView, doc: &Path, path: &[Seg]) -> MetaLink {
        let text = std::fs::read_to_string(doc).unwrap();
        let parsed = prov::Document::parse(doc, &text).unwrap();
        let meta = fig::Value::from(&parsed.meta);
        links_in(&meta, view.facets())
            .into_iter()
            .find(|l| l.path == path)
            .unwrap_or_else(|| panic!("no link at {path:?}"))
    }

    #[test]
    fn discovers_the_workspace_a_document_sits_in() {
        let vault = Vault::new("discover");
        let view = WorkspaceView::discover(&vault.path("notes/note.md"))
            .expect("discovery")
            .expect("a workspace");

        assert!(same_file(&view.root_document(), &vault.path("README.md")));
        assert_eq!(
            view.config_document()
                .map(|p| p.file_name().unwrap().to_owned()),
            Some("prov.yaml".into()),
            "the root's config pointer resolved"
        );
        // The config document's `fields` reached the effective config, so the
        // vocabulary it points at was loaded.
        assert!(view.config().fields.contains_key("audience"));
        let vocab = view.vocabularies().get("audience").expect("audiences.yaml");
        assert!(vocab.terms.contains_key("public"));
    }

    /// The three answers a follow actually has: a document that is there, a
    /// well-formed link to one that is not, and a target that was never going to
    /// be a file.
    #[test]
    fn follows_a_link_to_a_document_and_says_so_when_it_is_broken() {
        let vault = Vault::new("follow");
        let note = vault.path("notes/note.md");
        let view = WorkspaceView::discover(&note).unwrap().unwrap();

        // `/README.md` is workspace-absolute — resolved from the root, which is
        // the thing a bare document cannot do.
        let up = link_at(&view, &note, &[Seg::Key("part_of".into())]);
        let landed = view.resolve(&note, &up);
        let opened = landed.openable().expect("the root is on disk");
        assert!(same_file(opened, &vault.path("README.md")));

        let broken = link_at(&view, &note, &[Seg::Key("links".into()), Seg::Index(0)]);
        match view.resolve(&note, &broken) {
            Destination::Document { path, exists } => {
                assert!(!exists, "gone.md is not there");
                assert!(path.ends_with("notes/gone.md"), "resolved beside the note");
            }
            other => panic!("expected a broken document link, got {other:?}"),
        }

        // An external target keeps the URL: prov's own answer is the bare fact
        // of externality, and a status line has to be able to name what it was.
        let external = link_at(&view, &note, &[Seg::Key("links".into()), Seg::Index(1)]);
        match view.resolve(&note, &external) {
            Destination::External(url) => assert_eq!(url, "https://example.com/"),
            other => panic!("expected an external target, got {other:?}"),
        }
    }

    /// Opening through the workspace is what gets a document its schema — and
    /// the config document gets the *other* one.
    #[test]
    fn a_document_opens_under_the_schema_its_kind_calls_for() {
        let vault = Vault::new("schema");
        let view = WorkspaceView::discover(&vault.path("README.md"))
            .unwrap()
            .unwrap();

        let note = view.open_document("notes/note.md").expect("open the note");
        let schema = note.metadata().schema().expect("a content schema");
        assert!(
            schema
                .rule_for(&[Seg::Key("audience".into())])
                .is_some_and(|r| r.constraint.is_some()),
            "the workspace's controlled field reached the editor"
        );

        let config = view.open_document("prov.yaml").expect("open the config");
        let schema = config.metadata().schema().expect("a config schema");
        assert!(
            schema.rule_for(&[Seg::Key("fixity".into())]).is_some(),
            "the config document is edited under the config schema"
        );
    }

    /// The walk goes **up**: a document in a subdirectory finds the root above
    /// it, which is what makes "open any file in the vault" work.
    #[test]
    fn discovery_walks_up_from_a_subdirectory() {
        let vault = Vault::new("walk_up");
        let view = WorkspaceView::discover(&vault.path("notes"))
            .expect("discovery")
            .expect("a workspace");
        assert!(same_file(&view.root_document(), &vault.path("README.md")));
    }

    /// Without a workspace a relative target still resolves; the two that need a
    /// root or a registry say what is missing instead of guessing.
    #[test]
    fn the_lexical_floor_resolves_what_it_can_and_names_what_it_cannot() {
        let dir = std::env::temp_dir().join("provui_workspace_floor");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = dir.join("note.md");
        std::fs::write(
            &doc,
            "---\nlinks:\n- 'sibling.md'\n- '/root.md'\n- 'id:ajp7eq'\n---\n# n\n",
        )
        .unwrap();
        std::fs::write(dir.join("sibling.md"), "# sibling\n").unwrap();

        let text = std::fs::read_to_string(&doc).unwrap();
        let parsed = prov::Document::parse(&doc, &text).unwrap();
        let meta = fig::Value::from(&parsed.meta);
        let links = links_in(&meta, &Facets::default());

        match resolve_without_workspace(&doc, &links[0]) {
            Destination::Document { exists, .. } => assert!(exists, "sibling.md is there"),
            other => panic!("expected a document, got {other:?}"),
        }
        assert!(matches!(
            resolve_without_workspace(&doc, &links[1]),
            Destination::Unresolvable { .. }
        ));
        assert!(matches!(
            resolve_without_workspace(&doc, &links[2]),
            Destination::Unresolvable { .. }
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
