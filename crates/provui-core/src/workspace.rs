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
//! fields point at, which region of the tree each scoped field declaration
//! governs, the [`Facets`] its vocabulary implies, and the
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use flower_core::{Choice, Schema};
use prov::index::FileIndex;
use prov::workspace::FieldScopes;
use prov::{
    Backlink, Discovery, IdIndex, Settings, StdFs, Target, Workspace, WorkspaceConfig, block_on,
    discover,
};

use crate::facets::Facets;
use crate::findings::Finding;
use crate::links::AnyLink;
use crate::schema::Vocabularies;
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
    /// Which region of the tree each scoped field declaration governs. Resolved
    /// once: it is a walk per scoped declaration, and a title-index scan when
    /// an anchor is a `[[Title]]`, which is a cost to pay at open and not on
    /// every document.
    scopes: FieldScopes,
    vocabularies: Vocabularies,
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
        // A scoped declaration whose anchor resolves to nothing governs no
        // document, and `FieldScopes` says so per declaration rather than
        // failing. That is the right policy for an editor for the same reason
        // as a vocabulary that does not load: it is `prov check`'s finding.
        let scopes = block_on(ws.field_scopes_of(&root_doc, &config)).map_err(we)?;
        let vocabularies = load_vocabularies(&ws, &root_doc, &config);
        let facets = Facets::from_config(&config);
        Ok(Self {
            ws,
            root_doc,
            config_doc,
            config,
            scopes,
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

    /// The vocabularies the controlled fields point at, one per declaration. A
    /// vocabulary that failed to load is simply absent.
    pub fn vocabularies(&self) -> &Vocabularies {
        &self.vocabularies
    }

    /// Which region of the tree each scoped field declaration governs — what
    /// [`schema_for`](Self::schema_for) asks to know which declaration of a
    /// field reaches a document. A workspace with no scoped declarations has
    /// an empty one that always falls back to the workspace-wide declaration.
    pub fn field_scopes(&self) -> &FieldScopes {
        &self.scopes
    }

    /// prov's read surface, for a frontend that wants more of it than this view
    /// exposes — a tree, a census, a title index.
    pub fn prov(&self) -> &Workspace<StdFs, prov::identity::NoIdentity, FileIndex> {
        &self.ws
    }

    /// The schema a **content** document in this workspace is edited under
    /// when no particular document is in hand: the workspace-wide declaration
    /// of each field, and nothing for a field declared only under indexes.
    ///
    /// For a document you have, [`schema_for`](Self::schema_for) is the real
    /// answer — it is this, with each scoped field resolved to the declaration
    /// that governs *that* document.
    pub fn content_schema(&self) -> Schema {
        crate::schema_from_config(&self.config, &self.vocabularies.unscoped(&self.config))
    }

    /// The schema `path` is edited under: the config-document schema for the
    /// document that *is* this workspace's config, and for everything else the
    /// content schema with each field governed by whichever of its
    /// declarations reaches this document — `status` under `Tasks` gets the
    /// task terms, under `Proposals` the proposal terms, and a document under
    /// neither gets no `status` rule at all.
    ///
    /// A fact about which document this is, not a preference. `prov.yaml` is a
    /// document whose keys are policy, and editing it under the content schema
    /// would offer term pickers for fields it does not have and none for the
    /// ones it does.
    pub fn schema_for(&self, path: &Path) -> Schema {
        match self.config_document() {
            Some(config_doc) if same_file(&config_doc, path) => crate::config_schema(&self.config),
            _ => crate::schema::schema_for_document(
                &self.config,
                &self.scopes,
                &self.vocabularies,
                &self.relative(path),
            ),
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
        let mut session = DocumentSession::open_with_schema(&path, schema)?;
        // The walk is paid here, once, and the backend answers every picker
        // from the map afterwards — see `ProvBackend::set_candidates`. A
        // workspace that cannot be walked is a workspace with no candidates,
        // not a document that cannot be opened: the field falls back to free
        // text, which is what it was before there was a picker at all.
        if let Ok(map) = self.candidates_map(&path) {
            session.set_candidates(map);
        }
        Ok(session)
    }

    /// Every relation's candidate list for a document being edited at `doc`, in
    /// the shape [`ProvBackend::set_candidates`](crate::ProvBackend::set_candidates)
    /// takes.
    ///
    /// **One walk, whatever the relation.** Every content document in the
    /// workspace is a candidate for every relation — prov's relations are not
    /// typed by what they may point at — so the list is built once and shared
    /// across the entries. The map is keyed by relation so that the backend can
    /// answer without knowing any of this, and so that a narrowing added later
    /// (a relation that may only point at an index, say) changes one function
    /// and nothing downstream.
    pub fn candidates_map(&self, doc: &Path) -> Result<HashMap<String, Vec<Choice>>, SessionError> {
        let choices = self.candidates_for(doc, "")?;
        Ok(self
            .config
            .relation_set()
            .relations()
            .iter()
            .map(|rel| (rel.name.clone(), choices.clone()))
            .collect())
    }

    /// What a picker on `relation`, in the document at `doc`, should offer:
    /// every other content document in the workspace, spelled as a link this
    /// workspace would write.
    ///
    /// Each [`Choice`] carries the three things a picker draws and commits:
    ///
    /// - **`value`** is exactly what [`reference_to`](Self::reference_to) would
    ///   write for that target — markdown or wikilink, by path or by id,
    ///   root-relative or document-relative, labelled with the target's own
    ///   title. So choosing a candidate writes a link in the workspace's own
    ///   style, and a document that chooses one is indistinguishable from a
    ///   document whose link was typed by hand correctly.
    /// - **`label`** is the target's title, which is what a reader is looking
    ///   for and what flower's filter matches against.
    /// - **`detail`** is the workspace-relative path, which is what tells two
    ///   documents with the same title apart.
    ///
    /// `doc` itself is left out: a document contains or is contained by other
    /// documents, and prov's check has a finding for the one that points at
    /// itself.
    ///
    /// `relation` is read for nothing today and is in the signature anyway,
    /// because *which* relation is asking is the only axis this could ever be
    /// narrowed along, and a caller that has already written the argument does
    /// not have to be found again when it is.
    ///
    /// ## What it costs
    ///
    /// A spanning walk from the workspace root
    /// ([`reachable_documents_from`](prov::Workspace::reachable_documents_from))
    /// — the same population `prov check` counts — plus one read per document
    /// for its title. Proportional to the workspace, not to the document. This
    /// is a per-open cost and must not be put behind a keystroke; see
    /// [`ProvBackend::set_candidates`](crate::ProvBackend::set_candidates) for
    /// where the answer is cached.
    pub fn candidates_for(&self, doc: &Path, relation: &str) -> Result<Vec<Choice>, SessionError> {
        let _ = relation;
        let doc_rel = self.relative(doc);
        let reachable = block_on(self.ws.reachable_documents_from(&self.root_doc)).map_err(we)?;
        let mut choices = Vec::new();
        for target in reachable {
            if target == doc_rel {
                continue;
            }
            let reference = self.reference_to(&doc_rel, &target, None)?;
            choices.push(
                Choice::new(fig::Value::Str(reference), self.title_of(&target))
                    .detail(target.display().to_string()),
            );
        }
        Ok(choices)
    }

    /// Resolve a link written in the document at `doc` (absolute or
    /// workspace-relative).
    ///
    /// Path targets and `id:` handles resolve; a nominal (`[[My File]]`) target
    /// does not, because resolving one needs a title index over the whole
    /// workspace and that is a scan an editor should not do behind a keystroke.
    /// Use [`resolve_nominal`](Self::resolve_nominal) to pay for it deliberately.
    ///
    /// Generic over [`AnyLink`], which is what lets a link written in the prose
    /// body resolve through exactly this code: where a link *sits* is the one
    /// thing resolution never asks about.
    pub fn resolve(&self, doc: &Path, link: &impl AnyLink) -> Destination {
        self.destination(self.ws.resolve_link(&self.relative(doc), link.link()), link)
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
        link: &impl AnyLink,
    ) -> Result<Destination, SessionError> {
        let index = block_on(self.ws.title_index()).map_err(we)?;
        let target = self
            .ws
            .resolve_link_with(&self.relative(doc), link.link(), Some(&index));
        Ok(self.destination(target, link))
    }

    /// The link text to **write** from the document at `from` to the one at
    /// `to`, optionally landing on `locator` inside it — "a link to there", in
    /// this workspace's own spelling.
    ///
    /// The inverse of [`resolve`](Self::resolve), and the only thing in this
    /// crate that produces link syntax rather than consuming it. It is still a
    /// read: nothing is written, nothing is registered, and the caller decides
    /// what to do with the string. Retargeting an existing link is prov's
    /// `mutate` layer and is still out of scope (see the module docs); handing
    /// a reader the text of a link is not.
    ///
    /// Everything about the spelling is prov's
    /// [`reference_style`](prov::Workspace::reference_style) — markdown or
    /// wikilink, by path or by id, root-relative or document-relative, labelled
    /// or bare. A workspace that addresses by id gets one **only if the target
    /// is already registered**: minting an id would be a write, so an
    /// unregistered target degrades to a path link, which is exactly what
    /// [`format_reference`](prov::link::format_reference) does with `None`.
    ///
    /// The label is the target's own `title`, falling back to prov's
    /// [`path_to_title`](prov::link::path_to_title) — the same two steps every
    /// prov verb that authors a link takes. A target that cannot be read falls
    /// back with it rather than failing: a link to a document that is not there
    /// yet is a reasonable thing to want to write.
    pub fn reference_to(
        &self,
        from: &Path,
        to: &Path,
        locator: Option<&str>,
    ) -> Result<String, SessionError> {
        let from_rel = self.relative(from);
        let to_rel = self.relative(to);
        let style = self.ws.reference_style();
        let id = style
            .registers()
            .then(|| self.ws.index().id_for_path(&to_rel))
            .flatten();
        let title = self.title_of(&to_rel);
        let reference =
            prov::link::format_reference(style, &from_rel, &to_rel, id.as_ref(), &title);
        Ok(with_locator(&reference, locator))
    }

    /// A document's `title`, or prov's title-from-path fallback.
    fn title_of(&self, rel: &Path) -> String {
        block_on(self.ws.read_text(rel))
            .ok()
            .and_then(|text| prov::Document::parse(rel, &text).ok())
            .and_then(|doc| {
                fig::Value::from(&doc.meta)
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| prov::link::path_to_title(rel))
    }

    /// What prov's integrity check says about `doc`, placed where an editor can
    /// draw it.
    ///
    /// ## What it costs
    ///
    /// [`Workspace::check`](prov::Workspace::check) is **reachability-bounded**:
    /// it walks from the document it is given and reports on what that walk
    /// reaches. Starting it at the document itself is therefore the cheap
    /// per-document check — for a leaf note with no `contents` it loads one
    /// document and censuses its links, which is the price of a save. It is not
    /// free for every document: run on an index, it walks the subtree under it,
    /// and run on the workspace root it walks the workspace. A frontend that
    /// wants this after every keystroke should not have it; after a save, which
    /// is what `provui-tui` does, it is proportional to what the document
    /// contains.
    ///
    /// The bound is also why the answer is narrower than `prov check` on the
    /// whole workspace: a finding lodged against *this* document by a walk that
    /// started somewhere else — a parent reporting that this document does not
    /// link back — is not reachable from here and does not appear. What does
    /// appear is everything this document declares.
    ///
    /// Findings about other documents the walk reached are filtered out:
    /// [`subject`](prov::Finding::subject) is prov's own answer to "which file
    /// would a repair open", and a broken link in `a.md` pointing at `b.md`
    /// belongs to `a.md`.
    pub fn findings_for(&self, doc: &Path) -> Result<Vec<Finding>, SessionError> {
        let rel = self.relative(doc);
        let found = block_on(self.ws.check(&rel)).map_err(we)?;
        // Read once, for the relation-name → list-index refinement, and only if
        // there is something to refine.
        let links = if found.iter().any(|f| f.subject() == rel) {
            self.links_of(&rel)
        } else {
            Vec::new()
        };
        Ok(found
            .iter()
            .filter(|f| f.subject() == rel)
            .map(|f| crate::findings::place(f, &rel, &links))
            .collect())
    }

    /// The metadata links a document declares, for placing a finding. An
    /// unreadable document has none, which is the state a finding about it is
    /// already reporting.
    fn links_of(&self, rel: &Path) -> Vec<crate::MetaLink> {
        block_on(self.ws.read_text(rel))
            .ok()
            .and_then(|text| prov::Document::parse(rel, &text).ok())
            .map(|doc| crate::links_in(&fig::Value::from(&doc.meta), &self.facets))
            .unwrap_or_default()
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
    fn destination(&self, target: Target, link: &impl AnyLink) -> Destination {
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
pub fn resolve_without_workspace(doc: &Path, link: &impl AnyLink) -> Destination {
    use crate::links::TargetKind;

    match link.kind() {
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

/// Attach a `#locator` to a reference that has already been rendered.
///
/// A locator belongs on the *target*, inside whatever wrapper the style chose —
/// `[Title](notes.md#a-heading)`, not `[Title](notes.md)#a-heading` — and
/// [`format_reference`](prov::link::format_reference) builds the target itself,
/// so the only seam left is to read the rendered reference back with
/// [`Link::parse`](prov::Link::parse), re-target it with
/// [`join_locator`](prov::link::join_locator), and render it again. Both halves
/// are prov's, and `render` reproduces the wrapper it parsed, so a wikilink
/// workspace stays a wikilink workspace.
fn with_locator(reference: &str, locator: Option<&str>) -> String {
    let Some(locator) = locator else {
        return reference.to_string();
    };
    let link = prov::Link::parse(reference);
    let target = prov::link::join_locator(link.target.clone(), Some(locator));
    link.with_target(target).render()
}

/// The link text to write from `from` to `to` with **no workspace to spell it**
/// — the lexical floor, matching [`resolve_without_workspace`].
///
/// A relative markdown link, which is the one spelling that needs nothing but
/// the two paths: no config to read a reference style out of, no registry to
/// address by id, no root to be absolute from. The label is prov's
/// title-from-path, since reading the target's frontmatter for a better one is
/// exactly the filesystem work this half of the module does not do.
pub fn reference_without_workspace(from: &Path, to: &Path, locator: Option<&str>) -> String {
    let title = prov::link::path_to_title(to);
    let reference = prov::format_link(prov::LinkStyle::MarkdownRelative, from, to, &title);
    with_locator(&reference, locator)
}

/// The link text to write from `from` to **where the caret is** in `session` —
/// "a link to here".
///
/// The composition of the two halves this module and [`DocumentSession`] each
/// own: the caret's heading becomes a locator
/// ([`DocumentSession::locator_at_caret`]), and the workspace spells the
/// reference to the document ([`WorkspaceView::reference_to`]).
///
/// `from` is the document the link would be *written in*, which is what decides
/// a relative path. Passing the session's own path — the default a frontend
/// with one document open has — asks for a link from this document to a place
/// in this document, and gets the same-document `#locator` form rather than a
/// path back to the file you are already in. With no heading above the caret
/// there is no place to name, and the answer is a reference to the document as
/// a whole.
pub fn reference_here(
    view: Option<&WorkspaceView>,
    session: &DocumentSession,
    from: &Path,
) -> String {
    let locator = session.locator_at_caret();
    let here = session.path();
    if let (Some(locator), true) = (locator.as_deref(), same_file(from, here)) {
        // A link into the document it is written in addresses no document at
        // all: `[The Hard Part](#the-hard-part)`. Written by hand rather than
        // through `format_reference`, which always addresses a document —
        // there is nothing for it to address, and a workspace's path style has
        // no say over a fragment.
        let label = session
            .heading_at_caret()
            .map(|h| h.text)
            .unwrap_or_else(|| locator.to_string());
        return prov::Link {
            label: Some(label),
            target: prov::link::join_locator(String::new(), Some(locator)),
            wikilink: false,
        }
        .render();
    }
    match view {
        Some(view) => view
            .reference_to(from, here, locator.as_deref())
            .unwrap_or_else(|_| reference_without_workspace(from, here, locator.as_deref())),
        None => reference_without_workspace(from, here, locator.as_deref()),
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

/// Load the vocabulary every controlled field declaration points at — one per
/// declaration, since a field declared under two indexes names two stores.
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
) -> Vocabularies {
    let mut loaded = Vocabularies::default();
    for (field, declarations) in &config.fields {
        for (index, spec) in declarations.iter().enumerate() {
            let Some(pointer) = spec.vocabulary.as_deref() else {
                continue;
            };
            // A reified vocabulary's terms are documents down the spanning
            // tree, not rows in a flat store, so it is read a different way.
            // What makes it reified is the declaration, not anything the target
            // says about itself.
            let vocabulary = if spec.reify {
                block_on(ws.load_reified_vocabulary(root_doc, field, spec))
            } else {
                block_on(ws.load_vocabulary(root_doc, pointer))
            };
            if let Ok(Some(vocabulary)) = vocabulary {
                loaded.insert(field.clone(), index, vocabulary);
            }
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
    use crate::findings::{Severity, Site};
    use crate::links::{MetaLink, links_in};
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
        let vocab = view
            .vocabularies()
            .get("audience", 0)
            .expect("audiences.yaml");
        assert!(vocab.terms.contains_key("public"));
    }

    /// The inverse of following: the link text to *write*, in the workspace's
    /// own spelling, with and without a locator.
    #[test]
    fn a_reference_is_written_in_the_workspaces_own_style() {
        let vault = Vault::new("reference");
        let note = vault.path("notes/note.md");
        let root = vault.path("README.md");
        let view = WorkspaceView::discover(&note).unwrap().unwrap();

        // An unconfigured workspace is markdown, by path, workspace-absolute —
        // prov's default reference style, and the label is the target's own
        // `title` rather than its filename.
        assert_eq!(
            view.reference_to(&note, &root, None).unwrap(),
            "[The Vault](/README.md)"
        );
        assert_eq!(
            view.reference_to(&root, &note, None).unwrap(),
            "[A Note](/notes/note.md)"
        );

        // The locator goes on the target, inside the wrapper.
        assert_eq!(
            view.reference_to(&root, &note, Some("crash-safety"))
                .unwrap(),
            "[A Note](/notes/note.md#crash-safety)"
        );

        // A target with no document to read a title off falls back to prov's
        // title-from-path rather than failing.
        assert_eq!(
            view.reference_to(&root, &vault.path("notes/not_here.md"), None)
                .unwrap(),
            "[Not Here](/notes/not_here.md)"
        );
    }

    /// "A link to here" — the caret's heading becomes the locator, and the two
    /// `from`s a frontend actually has produce the two forms.
    #[test]
    fn a_link_to_here_names_the_heading_the_caret_is_under() {
        let vault = Vault::new("link_to_here");
        let note = vault.path("notes/note.md");
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[The Vault](/README.md)'\n---\nPreamble, above every heading.\n\n# A Note\n\n## Crash Safety\n\nWhy the journal is written first.\n",
        )
        .unwrap();
        let view = WorkspaceView::discover(&note).unwrap().unwrap();
        let mut session = DocumentSession::open(&note).unwrap();
        let at = session.body().source.find("Why the journal").unwrap();
        session.body_mut().caret = at;

        // From this document: a same-document fragment, labelled with the
        // heading's own words. There is no path to write — you are already
        // here.
        assert_eq!(
            reference_here(Some(&view), &session, &note),
            "[Crash Safety](#crash-safety)"
        );

        // From another document: the workspace's reference to this one, with
        // the same fragment on the end.
        assert_eq!(
            reference_here(Some(&view), &session, &vault.path("README.md")),
            "[A Note](/notes/note.md#crash-safety)"
        );

        // With no workspace at all: a relative markdown link, titled from the
        // path, because there is no config to read a style out of.
        assert_eq!(
            reference_here(None, &session, &vault.path("README.md")),
            "[Note](notes/note.md#crash-safety)"
        );

        // Above the first heading there is no place to name, so the answer is
        // the document itself.
        session.body_mut().caret = 0;
        assert_eq!(session.locator_at_caret(), None);
        assert_eq!(
            reference_here(Some(&view), &session, &vault.path("README.md")),
            "[A Note](/notes/note.md)"
        );
    }

    /// prov's check, placed: a broken link in the prose lands on a byte range,
    /// a broken `part_of` lands on the metadata row that declares it.
    #[test]
    fn a_findings_run_places_each_one_in_the_region_it_was_written_in() {
        let vault = Vault::new("findings");
        let note = vault.path("notes/note.md");
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[Nowhere](/nowhere.md)'\n---\n# A Note\n\nSee [the missing one](gone.md).\n",
        )
        .unwrap();
        let view = WorkspaceView::discover(&note).unwrap().unwrap();
        let findings = view.findings_for(&note).expect("check");

        let body: Vec<&Finding> = findings
            .iter()
            .filter(|f| matches!(f.site, Site::Body(_)))
            .collect();
        assert_eq!(body.len(), 1, "one body finding: {findings:#?}");
        assert_eq!(body[0].kind, "broken_link");
        assert_eq!(body[0].severity, Severity::Error);
        let Site::Body(span) = &body[0].site else {
            unreachable!()
        };
        // The span is a range in the *body*, not in the file: it slices the
        // link back out of the prose leaf is holding.
        let session = DocumentSession::open(&note).unwrap();
        assert_eq!(
            &session.body().source[span.clone()],
            "[the missing one](gone.md)"
        );

        let meta: Vec<&Finding> = findings
            .iter()
            .filter(|f| matches!(f.site, Site::Meta(_)))
            .collect();
        assert_eq!(meta.len(), 1, "one metadata finding: {findings:#?}");
        assert_eq!(meta[0].kind, "broken_link");
        assert_eq!(
            meta[0].site,
            Site::Meta(vec![Seg::Key("part_of".into())]),
            "on the row that declares it"
        );
        // prov's own sentence, without the path a per-document panel already
        // knows.
        assert!(
            meta[0].message.starts_with("broken part_of link:"),
            "{:?}",
            meta[0].message
        );
    }

    /// Applying findings washes the body ones under the text, and leaves the
    /// metadata ones for the host to draw.
    #[test]
    fn applying_findings_highlights_the_body_and_holds_the_rest() {
        let vault = Vault::new("apply_findings");
        let note = vault.path("notes/note.md");
        std::fs::write(
            &note,
            "---\ntitle: A Note\npart_of: '[Nowhere](/nowhere.md)'\n---\n# A Note\n\nSee [the missing one](gone.md).\n",
        )
        .unwrap();
        let view = WorkspaceView::discover(&note).unwrap().unwrap();
        let findings = view.findings_for(&note).unwrap();

        let mut session = DocumentSession::open(&note).unwrap();
        assert!(session.body().highlights().is_empty());
        session.apply_findings(&findings);

        let highlights = session.body().highlights();
        assert_eq!(highlights.len(), 1, "the body half, and only it");
        assert_eq!(highlights[0].id, "broken_link", "the kind is the id");
        assert_eq!(highlights[0].marker.as_deref(), Some("finding"));
        assert_eq!(
            &session.body().source[highlights[0].start..highlights[0].end],
            "[the missing one](gone.md)"
        );

        // The metadata half is readable by path, which is how a host puts it
        // beside the row it belongs to.
        assert!(
            session
                .meta_finding_at(&[Seg::Key("part_of".into())])
                .is_some()
        );
        assert!(
            session
                .meta_finding_at(&[Seg::Key("title".into())])
                .is_none()
        );

        // And it reached the *rows*: flower carries the same finding as an
        // annotation at the same path, so the widget draws the marker and the
        // message without the host drawing either.
        session
            .metadata_mut()
            .set_view(flower_core::ViewMode::Pages);
        let items = &session.metadata().page().items;
        let part_of = items
            .iter()
            .find(|i| i.path == [Seg::Key("part_of".into())])
            .expect("a row for part_of");
        let annotation = part_of.annotation.as_ref().expect("marked");
        assert_eq!(annotation.severity, flower_core::annotate::Severity::Error);
        assert!(
            annotation.message.starts_with("broken part_of link:"),
            "{:?}",
            annotation.message
        );
        let title = items
            .iter()
            .find(|i| i.path == [Seg::Key("title".into())])
            .expect("a row for title");
        assert!(title.annotation.is_none(), "only the row that is wrong");

        // A fresh list replaces the old one, rows and all — the same ownership
        // the body's highlights have.
        session.apply_findings(&[]);
        assert!(session.metadata().annotations().is_empty());
    }

    /// A picker on a reference field offers the workspace's other documents,
    /// spelled the way this workspace spells a link — and choosing one writes
    /// exactly that.
    ///
    /// The whole point of routing it through `reference_to` rather than
    /// formatting a path here: a document that chose a candidate is
    /// indistinguishable from one whose link was typed by hand correctly.
    #[test]
    fn a_reference_field_offers_the_workspaces_other_documents() {
        let vault = Vault::new("candidates");
        let note = vault.path("notes/note.md");
        let view = WorkspaceView::discover(&note).unwrap().unwrap();

        let offered = view.candidates_for(&note, "part_of").expect("walk");
        // Every document the workspace *reaches* — which is prov's own
        // population, so the config document the root points at is one of them
        // and the vocabulary store, which is loaded rather than contained, is
        // not. The document being edited is left out: a link from a document to
        // itself is one of prov's findings.
        let labels: Vec<&str> = offered.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["The Vault", "vault config"], "{offered:#?}");

        // The value is the reference text, not the path: this vault writes
        // labelled markdown links by workspace-absolute path.
        let root = offered
            .iter()
            .find(|c| c.label == "The Vault")
            .expect("the root");
        assert_eq!(
            root.value,
            fig::Value::Str("[The Vault](/README.md)".into()),
            "exactly what `reference_to` would write"
        );
        assert_eq!(
            root.detail.as_deref(),
            Some("README.md"),
            "the path, to tell two titles apart"
        );

        // And it reaches the model through the backend, at the relation's row
        // and at an item of it alike.
        let mut session = view.open_document(&note).unwrap();
        let part_of = [Seg::Key("part_of".into())];
        let choices = session
            .metadata()
            .choices_at(&part_of)
            .expect("the workspace answered");
        assert_eq!(
            choices.iter().map(|c| &c.value).collect::<Vec<_>>(),
            offered.iter().map(|c| &c.value).collect::<Vec<_>>()
        );

        // Choosing one commits the reference text into the document.
        session.metadata_mut().focus_on(&part_of);
        session.metadata_mut().begin_choose();
        while session
            .metadata()
            .choice_selected()
            .is_some_and(|c| c.label != "The Vault")
        {
            session.metadata_mut().choose_next();
        }
        session.metadata_mut().choose_commit();
        let out = session.reassemble().unwrap();
        assert!(
            out.contains("part_of: '[The Vault](/README.md)'")
                || out.contains("part_of: \"[The Vault](/README.md)\""),
            "the chosen reference was written:\n{out}"
        );

        // A field with a vocabulary is answered by the schema and never reaches
        // the backend: `audience` is a closed prov vocabulary here, and its
        // terms are what the picker shows.
        let audience = [Seg::Key("audience".into()), Seg::Index(0)];
        let terms: Vec<String> = session
            .metadata()
            .choices_at(&audience)
            .expect("the vocabulary answered")
            .iter()
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(
            terms,
            ["private", "public"],
            "the vocabulary, not the documents"
        );

        // A field that is neither has nothing to pick from at all.
        assert!(
            session
                .metadata()
                .choices_at(&[Seg::Key("title".into())])
                .is_none()
        );
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

    /// The shape of the org's `tasks` preset: `status` declared twice, each
    /// `under:` an index named by title, with a different closed vocabulary
    /// each time, and no workspace-wide declaration at all.
    fn scoped_vault(name: &str) -> Vault {
        let vault = Vault::new(name);
        let dir = &vault.0;
        std::fs::create_dir_all(dir.join("tasks")).unwrap();
        std::fs::create_dir_all(dir.join("proposals")).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "---\ntitle: The Vault\nconfig: prov.yaml\ncontents:\n- '[A Note](notes/note.md)'\n- '[Tasks](tasks.md)'\n- '[Proposals](proposals.md)'\n---\n# The Vault\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("prov.yaml"),
            "title: vault config\nfields:\n  status:\n  - under: '[[Tasks]]'\n    values: closed\n    vocabulary: task-statuses.yaml\n  - under: '[[Proposals]]'\n    values: closed\n    vocabulary: proposal-statuses.yaml\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("task-statuses.yaml"),
            "title: Task statuses\nvocabulary:\n  field: status\n  values: closed\nterms:\n  open: {}\n  done: {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("proposal-statuses.yaml"),
            "title: Proposal statuses\nvocabulary:\n  field: status\n  values: closed\nterms:\n  draft: {}\n  accepted: {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tasks.md"),
            "---\ntitle: Tasks\npart_of: '[The Vault](/README.md)'\ncontents:\n- '[Fix the thing](tasks/fix.md)'\n---\n# Tasks\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tasks/fix.md"),
            "---\ntitle: Fix the thing\npart_of: '[Tasks](/tasks.md)'\nstatus: open\n---\n# Fix the thing\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("proposals.md"),
            "---\ntitle: Proposals\npart_of: '[The Vault](/README.md)'\ncontents:\n- '[Do it differently](proposals/differently.md)'\n---\n# Proposals\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("proposals/differently.md"),
            "---\ntitle: Do it differently\npart_of: '[Proposals](/proposals.md)'\nstatus: draft\n---\n# Do it differently\n",
        )
        .unwrap();
        vault
    }

    fn status_terms(view: &WorkspaceView, rel: &str) -> Option<Vec<String>> {
        use flower_core::FieldRuleExt;
        let schema = view.schema_for(&view.absolute(Path::new(rel)));
        schema.rule_for(&[Seg::Key("status".into())]).map(|rule| {
            let (terms, closed) = rule.enum_constraint().expect("a closed enum");
            assert!(closed, "{rel}: the declaration says `values: closed`");
            terms.iter().map(|t| t.value.clone()).collect()
        })
    }

    /// Which `status` a document gets is a fact about where it sits: the task
    /// terms under `Tasks`, the proposal terms under `Proposals`, and nothing
    /// anywhere else — the index included, which is not one of its own records.
    #[test]
    fn a_scoped_field_is_governed_by_the_declaration_that_reaches_the_document() {
        let vault = scoped_vault("scoped");
        let view = WorkspaceView::discover(&vault.path("README.md"))
            .unwrap()
            .unwrap();

        // Both declarations' vocabularies loaded, each under its own index.
        assert!(view.vocabularies().get("status", 0).is_some());
        assert!(view.vocabularies().get("status", 1).is_some());
        assert!(view.field_scopes().unresolved().is_empty());

        assert_eq!(
            status_terms(&view, "tasks/fix.md").as_deref(),
            Some(&["done".to_string(), "open".to_string()][..]),
            "a task draws the task terms"
        );
        assert_eq!(
            status_terms(&view, "proposals/differently.md").as_deref(),
            Some(&["accepted".to_string(), "draft".to_string()][..]),
            "a proposal draws the proposal terms"
        );
        assert!(
            status_terms(&view, "notes/note.md").is_none(),
            "a document under neither index has no status declaration"
        );
        assert!(
            status_terms(&view, "tasks.md").is_none(),
            "the index is not in its own scope"
        );

        // With no document in hand there is no workspace-wide declaration to
        // read, so the field is absent — the answer there was before prov could
        // scope a field, and still the right one for a caller with no document.
        assert!(
            view.content_schema()
                .rule_for(&[Seg::Key("status".into())])
                .is_none()
        );
        assert!(view.vocabularies().unscoped(view.config()).is_empty());

        // Opening through the view is the same answer, so an editor gets the
        // picker without asking.
        let task = view.open_document("tasks/fix.md").unwrap();
        let schema = task.metadata().schema().expect("a content schema");
        assert!(
            schema
                .rule_for(&[Seg::Key("status".into())])
                .is_some_and(|r| r.constraint.is_some())
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
