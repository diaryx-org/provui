# provui

An unopinionated UI composition layer over [`prov`](https://github.com/diaryx-org/prov) —
a structural editor for prov documents (embedded metadata + prose body) and for
the workspace config that governs them.

provui is the composition, not the app. It deliberately lacks any particular
product's style and user-friendliness; it exposes prov's structure directly, so
that the foundation can be validated on its own and reused by more than one
frontend.

## The shape

Everything under the UI is Rust, and each layer already exists and is tested:

```
prov        (workspace / document library) ┐
flower-core (structural metadata editor)   ├─ provui-core ─ a frontend
leaf-core   (rich-text body editor)        ┘
```

`provui-core` is where the three meet. It owns the composition and the
translation between them — flower-core never learns the word "prov", and prov
knows nothing about editors — so a frontend is left with drawing and input
handling and nothing else.

- **`ProvBackend`** — the flower↔prov bridge: a `flower_core::Backend` over
  prov's carrier-aware `MetaEditor`. Lossless, so comments, key order, the
  metadata carrier and the prose body all survive an edit. It is the second
  implementation of that trait, and it is checked against flower's own
  conformance suite rather than against a restatement of it, so a guarantee
  added upstream arrives here as a failing test. It also answers flower's two
  document-shaped questions: what a picker on a **reference field** should
  offer (`candidates`, injected by `set_candidates` because one document has no
  way to enumerate a workspace) and what a **list item is**, across a reorder
  (`item_key` — a link's target, which survives a relabel).
- **`DocumentSession`** — one open prov document, edited through a flower
  metadata model *and* a leaf body editor, reconciled on save. The two regions
  share no byte offsets, so they edit independently and meet only at `save`,
  which splices the body back in and writes the reassembled document. A disk
  round-trip test (`open_edit_save_reopen_round_trip_on_disk`) proves open →
  edit both → save → reopen with comments, fences, untouched keys and untouched
  body all preserved.
- **`schema_from_config`** — the adapter turning a resolved prov
  `WorkspaceConfig` (plus the vocabularies its controlled fields point at) into
  a generic `flower_core::Schema` for the workspace's *content* documents. This
  is where prov's controlled vocabularies and its spanning relation reach the
  UI, so a frontend renders term pickers and link widgets instead of text boxes.
  **`schema_for_document`** is the same adapter for one document in particular:
  prov 0.12 lets a field be declared several times, each `under:` an index, so
  which `status` a document gets — a task's terms, a proposal's, or none — is a
  fact about where it sits, and `WorkspaceView::schema_for` answers it per
  document.
- **`config_schema`** — the same trick turned on the config document itself. A
  prov config is a document, and flower can already render, type-direct and
  validate any prov document; the only thing missing was a schema saying that
  `fixity` is one of two words. With it, `id_storage` becomes a picker instead
  of free text, and a typo like `fixity: alll` — which prov silently ignores,
  keeping the default — stops being reachable.
- **`facets`** — what each frontmatter key *is* to prov: a relation, a one-way
  pointer at machinery, identity, the policy block, a declared field, or a value
  prov only carries. Read off the workspace's own vocabulary rather than a list
  kept here, so a workspace that retracts `link_of` gets an ordinary field and
  one that declares `see_also` gets a followable link, without a line changing.
- **`links`** — the links a document's frontmatter declares, each carrying the
  metadata **path** it sits at. prov already extracts a document's edges; what an
  editor additionally needs is *where* each one is, so that "the row under the
  cursor — is that a link?" is a question with an answer. Lexical throughout: no
  filesystem, no registry, no claim anything exists.
- **`body_links`** — the same question asked of the *prose*, answered with a
  byte range into the body instead of a metadata path. The scan is prov's own
  (`scan_body_links`, the seam its census, check and rename all use), so it is
  code-aware, reaches into footnote definitions, and never mistakes bracket
  prose for a link — three bugs a second implementation here would have had to
  find again. The two halves share a target and nothing else, and `AnyLink` is
  what that sharing is called: `WorkspaceView::resolve` takes either, so a
  `[[a.md]]` in a paragraph and an `[[a.md]]` in `contents` land in the same
  place by construction rather than by agreement.
- **`findings`** — what prov's integrity check says about one document, placed
  where an editor can draw it, and then *given* to that editor —
  `DocumentSession::apply_findings` washes the body half under the prose as leaf
  highlights and hangs the metadata half on the rows as flower annotations, so a
  frontend gets markers and messages without drawing either. prov's `Finding` names the document and, for a
  link, the *site* — a relation's name, or a byte span in the body. A relation's
  name is one step short of a place: `contents` is a list, and the broken item is
  the third one. So this recovers the index by matching the finding's target
  against the links that key actually holds, which is exact unless the list
  repeats a target — and where it does, the key alone is the honest answer rather
  than a guess. prov has no severity of its own; the error/warning split is this
  crate's, it is a rendering hint that suppresses nothing, and it lives in one
  list so that a frontend does not grow its own.
- **`WorkspaceView`** — the step that needs a workspace to take it in. It finds
  the workspace a document belongs to, resolves the effective config and the
  vocabularies it points at, and turns a link into a document you can open —
  absolute, and checked against the disk. Read-only, and that is not temporary:
  see [Scope](#scope).

Every spelling in `config_schema` is prov's own. The term lists mirror
`prov::diagnose`'s accepted values, and the tests assert exactly that: each
offered term is round-tripped through prov's linter, so a spelling prov renames
fails the build here rather than drifting into a picker that writes values prov
ignores.

## Scope

The single-document metadata surface — prov's `edit` layer — plus **read-only**
navigation across documents.

Following a link reads. *Retargeting* one does not: a relation field is half of a
pair prov maintains bidirectionally, so writing `contents` in one document means
writing `part_of` in another, and that is prov's `mutate` layer. The metadata
backend here edits one document's bytes and has no way to touch a second, which
is exactly why the line is where it is. A frontend may follow a link with what is
here and must not conclude it can retarget one.

Producing link *text* is still reading. `WorkspaceView::reference_to` says what
a link to a document would be spelled like in this workspace, and writes
nothing — not even the id an id-addressing style would need, since minting one
is a write: an unregistered target degrades to a path link, which is what prov's
`format_reference` does when handed no id. What the caller does with the string
is the caller's business.

Saving writes bytes directly. A frontend that wants fixity and `updated`
restamping maintained routes the write through prov's `Storage`/`mutate` layer
instead; this is the floor it builds on, not a policy it inherits.

## Structure, values, and what this crate refuses to decide

A prov document's frontmatter holds two kinds of thing side by side, and they
look identical: keys prov reads to build the workspace (`contents` is an edge,
`id` is identity, `prov:` is policy) and keys prov merely carries (`mood:
rainy`). A schema-free editor draws `id` and `mood` as the same row and offers to
let you type into both.

`facets` is the answer to which is which, and it is **only** the answer.
Nothing in this crate hides a row, sinks one, reorders them, or makes one
read-only — even where it plainly knows enough to. `Facets` will tell you that
`id` is minted by the workspace and that `contents` is structure, and hand you
those lists already shaped for flower's `derived` and `demoted` sets, and then
stop.

That is a deliberate answer to a real question. An application over prov usually
*does* separate the two halves — diaryx puts prov's structure in a sidebar and
gives the form to the user-defined values — and it is a good design. It is not a
general one. A mobile inspector, a 30-column terminal sidebar and a settings sheet do
not want the same split, and a core that picked one would be a core each frontend
had to work around. So the classification is general and lives here once; the
arrangement is local and lives in the frontend.

What that buys is measured in lines. `provui-tui`'s whole policy — the keys the
workspace maintains decline edits, and prov's structure sinks below the
document's own values — is two:

```rust
let mut session = DocumentSession::open_managed(path, schema, facets.managed_key_names())?;
session.metadata_mut().set_demoted(facets.structural_keys(session.meta()));
```

A frontend that wants a flat list writes neither.

The same principle is why `document_rules` and `config_rules` are public and
first-match-wins, and why `rules` is public at all: see
[Composing over it](#composing-over-it).

## Frontends

The core is frontend-neutral, and the plan is to prove that by using it twice.

**First: a TUI** — `provui-tui`, which exists (see [Usage](#usage) below). It
embeds [`leaf-ratatui`](https://github.com/diaryx-org/leaf) and
[`flower-ratatui`](https://github.com/diaryx-org/flower) — the two widget crates
that already exist for exactly these two editors. No FFI: it is one Rust binary
linking one copy of each library, which makes it the cheapest possible test of
whether the composition holds up under a real event loop, and the fastest thing
to iterate the core against.

**Later: SwiftUI over UniFFI**, against the same core. The hardest binding
already exists — `leaf` ships `leaf-ffi` + `leaf-swift` (LeafUI), a full
rich-text body editor for Apple, and `flower` ships `flower-ffi`. What that
milestone adds is a single `provui-ffi` wrapping `DocumentSession`, so the whole
editing composition stays in Rust and `fig`/`twig` are linked once. Orchestrating
several FFI stacks from Swift instead would risk duplicate native libraries in
one binary, which is the failure this arrangement exists to avoid.

Both frontends drive the same `DocumentSession`. If the TUI needs something the
core does not expose, that is the core's gap, and fixing it there is what makes
the second frontend cheap.

## Usage

`provui-tui`'s binary is `provui`, following the family: leaf-tui's is `leaf`
and flower-tui's is `flower`.

```sh
cargo run -p provui-tui -- path/to/document.md
```

It finds the workspace the file belongs to, opens the file through
`DocumentSession` under whatever schema that workspace implies, and draws the
document's two regions with the two widgets that exist for them —
`leaf-ratatui` over the prose, `flower-ratatui` over the frontmatter. Everything
about the document belongs to the session; the binary owns the terminal, the
split, the focus, the navigation and one status line, and nothing else.

There is a smaller door for looking rather than editing: `cargo run --example
inspect -p provui-core -- <file>` prints what each frontmatter key is to prov and
where each of its links lands, which is the whole of `facets` + `links` +
`WorkspaceView` in forty lines of caller.

A file that belongs to no workspace still opens — that is the ordinary state of a
markdown file — with no schema and with links resolved by path alone. A `⌂` at
the head of the status line is how it says which of the two you are in, because
that is the fact that decides whether there are pickers and whether `id:` links
resolve.

### The panes

```
  ▶ leaf — body                          │ flower —   document.md ●
 # Heading                               │ ‹document›
                                         │  title      New Title
 Original body.                          │  draft      true
                                         │  part_of    ↑ The Vault
                                         │  id         ajp7eq
                                         │
                                         │  j/k · l/h in/out · e edit · x del
 ⌂ document.md ○ saved  focus: body   ^W pane · ^S save · ^Q quit
```

**The body on the left, the metadata on the right.** The prose is the document
and reads left to right; the frontmatter is what is true about it, which is what
a sidebar is for. The metadata pane takes a third of the width, bounded to 30…80
columns — the floor is where a `key … value` row stops being readable, the
ceiling is where a wide terminal would be drawing pad between the two columns —
and the body gets every column the pane and the divider do not. Both get the
full height, which is the dimension a page of metadata actually spends: the
model's inline budget is refit to the **pane's** height on every frame
(`flower_ratatui::page_room` says how many item rows survive the widget's own
chrome), so on a tall terminal the whole frontmatter is drawn on one page with
nothing to drill into.

The cost is flower's own two-pane page view, which wants 64 columns and does not
get them from a third of an ordinary terminal. It falls back to its single-pane
layout — the same interaction in one column — and the split view comes back
from about 190 columns. When the terminal is too narrow for both minimums the
split is **abandoned rather than shrunk**, and whichever pane holds the keyboard
takes the screen. A whole-file config document has no prose region at all, and
is all metadata.

### Focus

Exactly one pane owns the keyboard. **`^W` switches it** — the window key, in a
host that has windows. The status line always names the pane that has it, and
the focused pane's label carries a `▶`.

`^W` is taken by the host *before* either widget sees the event, and it has to
be: leaf swallows every Ctrl and Alt chord it is handed, bound or not, so a host
cannot discover a free one from the return value; and flower reads `key.code`
while ignoring modifiers entirely, so an un-intercepted `^X` would arrive as `x`
and delete a key. `^W` is unbound in leaf's Ctrl table and is not a bare letter
for flower to navigate on, which is what makes it free to take.

A click also moves focus to the pane it lands in. A focus switch is refused
while the metadata pane has a value open for editing — leaving mid-edit would
strand a half-typed value in a pane no longer taking keys — and says so.

| Key | |
|---|---|
| `^W` | switch panes |
| `^S` | save the document — **both** regions, from either pane |
| `^G` | follow the link under the cursor — the metadata row, or the body link the caret is inside |
| `^O` | back to the document you followed from |
| `^R` | show the link text that points at the caret |
| `^Q` | quit; refused once while there are unsaved changes |
| body pane | leaf's keys (`leaf --help`) |
| metadata pane | `j`/`k` move · `l`/`h` in/out · `e` pick or edit · `E` type · `x` delete |

### Picking a link instead of typing one

A relation's row is a link, and a link to a document in this workspace is a
string a person should not have to spell. flower's `e` opens a **picker** where
the field has something to pick from and the text line where it does not, so
there is one key for both and this host binds nothing new for it.

What a reference field has to pick from is *other documents*, which flower-core
— one document, no filesystem — can never enumerate. So it asks the backend, and
`ProvBackend` answers from a map it was handed: `WorkspaceView::candidates_for`
walks the workspace's reachable documents (prov's own population, so the config
document the root points at is one of them), and each candidate's **value is
exactly what `WorkspaceView::reference_to` would write** — markdown or wikilink,
by path or by id, labelled with the target's own title, in this workspace's
style. A document whose `part_of` was chosen from the list is therefore
indistinguishable from one whose `part_of` was typed by hand correctly. The
label is the title, which is what the filter matches; the detail is the path,
which is what tells two documents with the same title apart.

**The walk is paid at open and never on a keystroke.** `Nav::open` builds the
map once and the backend answers every picker from it by relation name, so
`contents`, `contents[4]` and the append position are one entry and opening the
picker is a lookup. The staleness that buys is the staleness the per-document
check already has: a document created in another window is not on the list until
this one is reopened.

A field with a **controlled vocabulary** never reaches the backend at all —
flower asks its own schema first, and a prov vocabulary is already a
`Constraint::Enum` there. That includes a *reified* vocabulary, whose terms are
documents: `schema_from_config` emits the field's rule before the relation's and
a schema is first-match-wins, so the picker shows the vocabulary's terms rather
than every document in the workspace. `ProvBackend::relation_at` reads the same
rule, which is what keeps the two from ever disagreeing about which one wins.

Choosing writes a value through the ordinary commit funnel, so everything that
was already true of a typed value is still true of a chosen one: the schema
validates it, a workspace-maintained key refuses it, and the splice is lossless.

### Following links

A prov document carries links in both of its regions. Some frontmatter keys are
links, and so is a `[label](target)` or a `[[target]]` written in the prose; a
workspace is what makes either resolvable. **`^G` opens the document the focused
pane's cursor is on** and **`^O` returns**, which makes this a two-key browser
over the whole document graph: `^G` on `part_of` goes up, `^G` on a `contents`
item goes down, and `^G` with the caret inside a link in a paragraph goes
wherever the prose pointed.

Both are taken before the widgets for the same reason `^W` is, and both are free
in leaf's Ctrl table — `^G` for *go*, `^O` for the jump-back every vi has. The
back chord is advertised in the status line on arrival rather than in the
standing hints, which is exactly when there is something to go back to.

**Each pane follows its own cursor, and never the other's.** A body caret in the
middle of a paragraph is no evidence about which frontmatter row was last
selected, and following one from the other would be the host guessing. So the
metadata pane follows the row it is standing on, the body pane follows the link
the caret is *inside* — half-open, so a caret just past the closing bracket is
past the link — and a cursor on neither says `no link under the caret` or `not a
link` rather than picking something.

A link that lands somewhere that is not a file you can open says that too — a
URL, a `#locator` into this document, a reference into a workspace prov cannot
locate, or a target that is simply not on disk. Each of those is a real answer
rather than a failure, and the status line gives it.

Images are not followed. An `![alt](pic.png)` is one of prov's body links, but
it names a payload rather than a document, and opening a picture in a text
editor is not what the chord promises; prov's own census leaves them out for the
same reason.

### A link to here

Following a link is half of navigating a workspace; the other half is writing
one, and that starts with knowing what to write. **`^R` puts the link text that
would point at where the caret is into the status line** — `link to here:
[Crash Safety](#crash-safety)`.

The locator is the heading at or above the caret, slugged with prov's own
`link::slug`, which is the spelling that matters: it is the fragment `prov
check` resolves and the fragment leaf's `Doc::locate` lands, and for Markdown —
where there are no ids to name — a heading's own words are the only thing a
fragment can name. Above the first heading there is no place to point at, and
the answer is a reference to the document as a whole.

The rest of the spelling is the *workspace's*. `WorkspaceView::reference_to`
asks prov for the effective `reference_style` and writes markdown or wikilink,
by path or by id, root-relative or document-relative, labelled or bare,
accordingly — with the target's own `title` as the label, falling back to
`link::path_to_title`. A workspace that addresses by id gets one only if the
target is **already registered**: minting an id would be a write, and this stays
read-only, so an unregistered target degrades to a path link, which is what
prov's `format_reference` does when handed no id. With no workspace it is a
relative markdown link, which is the only form two paths alone can justify.

**Showing it is the whole deliverable.** This host has no clipboard — `^C` and
`^X` in the body already say so — so the status line is the honest maximum, and
it is the terminal that copies from there. A frontend with a clipboard calls the
same `provui_core::reference_here` and puts the string on it.

`^R` is free by the same two tests `^W` and `^G` passed: unbound in leaf's Ctrl
table, and `r` is not a bare letter flower navigates on, so an un-intercepted one
would reach flower as a plain `r` and do nothing. `^L` and `^K` were the other
candidates and both fail a test — `^K` is leaf's kill-to-end-of-line, and `l` is
how flower opens a row.

Leaving a document with **unsaved changes is refused**, with no second-press
escape hatch. Quitting has one because quitting twice discards work you were
told about and meant to discard; following a link is a *reading* gesture, and an
edit lost to one would be an edit lost to something nobody thinks of as
destructive.

### Findings

prov can say what is wrong with a document — a link that resolves to nothing, a
term outside a closed vocabulary, a child that does not link back — and
`WorkspaceView::findings_for` runs that check for one document and places each
answer. **The host runs it on open and after every save**, and nowhere else:
`Workspace::check` is reachability-bounded, so starting it at the document is
one document's worth of work for a leaf note and a subtree's worth for an index,
which is a price worth paying at the two moments the structure actually changed
and not on a keystroke.

That bound is also why the answer is narrower than `prov check` over the whole
workspace: a finding lodged against this document by a walk that started
somewhere else — a parent reporting that this document does not link back — is
not reachable from here and does not appear. What does appear is everything this
document declares.

**Each half goes to the editor that owns that half of the document, and the
host draws neither.** Body findings become leaf highlights, washed under the
link they are about — the spans prov reports are already body offsets, so
nothing converts. Metadata findings become flower `Annotation`s at the same
metadata path, so the row `contents[2]` was narrowed to gets the marker, and the
widget puts the message in its own footer when the cursor reaches it. The host's
**status line carries the count** and nothing else: that there is something to
go and look at is the one part of this no widget can say, because neither of
them can see the other's half.

`session.meta_findings()` and `meta_finding_at` are still there and still the
prov-side answer — the `kind` and the severity rather than the sentence — for a
frontend that wants to branch on a finding rather than draw it.
`provui_core::annotations_of` is the translation on its own.

A finding about the *file* rather than about anything written in it — an orphan,
a fixity mismatch — becomes an annotation at the **empty** path, which is
flower's spelling for the document. Nothing draws the root, so those stay the
host's to report from `session.findings()`; making one of them a row would be
inventing a row prov never named.

One consequence of both editors' design is worth stating: `Doc::set_highlights`
and `Model::set_annotations` each replace the whole set rather than adding to
it — deliberately, so the host and the document can never disagree about what is
on screen — so `apply_findings` **owns** both lists. A frontend that also wants
search hits in the body or annotations of its own on the rows composes the list
itself and calls leaf and flower directly.

### Saving, and what is not here

A save from *either* pane writes the whole document: `DocumentSession::save`
reconciles the body edits back into the metadata editor's document and writes the
reassembled bytes, so the unit that gets saved is the file, not the pane you were
standing in. Dirtiness is likewise the session's answer, covering both regions.
leaf's own `Doc::save` is deliberately unused — this body is a *region* of a
file rather than a file, and the `Doc` has no path.

leaf's `Outcome` is a full editor's surface, and `leaf-tui` is where all of it is
handled. This host implements the three outcomes that are about the document —
`Save`, `Quit`, `Continue` — and **degrades the rest to a status-line message**
rather than dropping them: `Copy`/`Cut`/`Paste`, `SaveAs`, `New`, the link,
language and media prompts, the command palette, `Find`/`Replace`, `Help`, and
the right-click context menu all say what they are and where they live. A key
that does nothing here is at least a key that admits it.

Two things do work without any of that: bracketed paste is enabled, so the
terminal's own paste arrives as one `Event::Paste` and goes into the body as a
single edit rather than as N keypresses; and mouse capture is on, so leaf gets
click-to-place-caret, drag-select and scrolling.

flower takes no mouse events, so the host maps a click in its pane back onto
the row it landed on and drives the model in the vocabulary flower's keys use.
A click stands on a row; a second click on the row the cursor is already on is
Enter — a container opens as a page, a value opens for editing; the wheel is
`j`/`k`, and works without taking the keyboard. In flower's two-pane view the
other half is one step along the lineage, and a click there takes it: a row of
the parent's page on the left backs out onto it, a row of the previewed page on
the right opens it there. The click that brings the keyboard to the pane only
ever stands, whatever row it landed on — focusing a pane is not Enter. A value
that is open for editing stays open, and its row stays put, until Enter or Esc.

## Composing over it

prov permits a config surface to carry keys it never reads, so an application
that keeps its own block (`myapp.default_view`, `myapp.publish`, …) supplies its
own rules for it. `config_schema` governs none of them — its vocabulary is
prov's, and an app's is the app's.

A schema resolves a path by **first match wins**, so an application *prepends*
its rules to `config_rules`:

```rust
let mut rules = my_app_rules();                       // `myapp.*`, and any narrowing
rules.extend(provui_core::config_schema::config_rules(&config));
let schema = flower_core::Schema::new(rules);
```

Prepending is what makes it an overlay rather than only an addition: an app that
wants a narrower vocabulary for a key this crate governs openly can shadow the
generic rule. Appending would leave the generic rule winning and the app's rule
silently dead — which is why there is a test asserting the order.

`document_rules` is the same door one document over, for a **content**
document's frontmatter, and the ordering inside it is the same argument made
twice. A workspace's `fields` declarations come first, prov's own kernel keys
(`title`, `id`, `content`/`manifest`/`attachment`/`content_hash`, and the root's
inline `prov:` block) come last — so a workspace that declares `fields.title`
shadows prov's rule for it rather than being shadowed by it, and an app that
prepends shadows both.

That the inline `prov:` block is governed at all falls out of stating the
vocabulary once. prov's spec says workspace policy has two homes and the same
keys in each — nested under `prov:` in the root, at top level in a config
document — so `kernel_rules` re-roots `config_rules` one key deeper rather than
keeping a second copy. A term added to the config schema reaches the inline block
in the same commit, because it is the same list.

The `provui_core::rules` module is public for the same reason: an overlay's rows
should come out looking like the ones beside them, with the same tints and the
same consequence vocabulary, without every frontend restating what a "costly"
field looks like.

## Related repos

- [`prov`](https://github.com/diaryx-org/prov) — the self-describing plaintext
  workspace library.
- [`flower`](https://github.com/diaryx-org/flower) — the generic structural
  config editor over `fig` (has `flower-ratatui`, `flower-ffi`).
- [`leaf`](https://github.com/diaryx-org/leaf) — the rich-text document editor
  over `twig` (has `leaf-ratatui`, `leaf-swift`).
- [`fig`](https://github.com/diaryx-org/fig) /
  [`twig`](https://github.com/diaryx-org/twig) — the Zig parsing/editing
  libraries underneath both.

## Building

The dependency chain reaches `fig` and `twig`, which are Zig-backed, so a build
needs `zig` on `PATH`. `nix develop` in `prov` or
[`nix`](https://github.com/diaryx-org/nix) provides one.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Everything here is consumed from crates.io by version, `leaf-ratatui` and
`flower-ratatui` included; no manifest carries a path across a repository
boundary, so nothing has to be undone to publish. To build against a *checkout*
of leaf or flower instead — for a widget change that is not on the registry
yet — turn on the `[patch.crates-io]` in `~/diaryx/.cargo/config.toml`, copied
from `~/diaryx/.cargo/patches.toml`, with four entries uncommented:

```toml
leaf-core      = { path = "leaf/crates/leaf-core" }
leaf-ratatui   = { path = "leaf/crates/leaf-ratatui" }
flower-core    = { path = "flower/crates/flower-core" }
flower-ratatui = { path = "flower/crates/flower-ratatui" }
```

The two **cores** have to be patched alongside the widgets, not just the widgets.
Each widget path-depends on its own core inside its own workspace, so patching
only the widget leaves the graph holding two copies of that core — a registry one
under `provui-core` and a path one under the widget — and `Model` and `Doc` stop
being the same type across the two. It surfaces as a baffling type error rather
than as anything mentioning duplicate crates. `cargo tree -i leaf-core` should
show exactly one. The patch must be off again before any publish: `cargo
publish` verifies by building, and would build against it.

### CI and releasing

`cargo xtask ci` runs what CI runs, in the same order — format, clippy, tests,
docs, `provui-core` checked on its own, and the MSRV — and `cargo xtask <job>`
runs one. `.github/workflows/ci.yml` reads the job list from `cargo xtask
ci-matrix`, so a job is added or renamed in `xtask/src/main.rs` and nowhere
else.

Releases are cut with the org's shared tooling — `dx release <spec>` bumps,
regenerates [`docs/CHANGELOG.md`](docs/CHANGELOG.md), commits and tags, and
pushing the tag runs `publish.yml`, which uploads `provui-core`. `provui-tui` is
`publish = false` and stays in the checkout; its manifest says why.

## License

MIT or Apache-2.0, at your option.
