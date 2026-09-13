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
  added upstream arrives here as a failing test.
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
| `^G` | follow the link under the metadata cursor |
| `^O` | back to the document you followed from |
| `^Q` | quit; refused once while there are unsaved changes |
| body pane | leaf's keys (`leaf --help`) |
| metadata pane | `j`/`k` move · `l`/`h` in/out · `e` edit · `x` delete |

### Following links

Some of a prov document's frontmatter keys are links, and a workspace is what
makes them resolvable. **`^G` opens the document the metadata cursor is standing
on** and **`^O` returns**, which makes this a two-key browser over the spanning
tree: `^G` on `part_of` goes up, `^G` on a `contents` item goes down.

Both are taken before the widgets for the same reason `^W` is, and both are free
in leaf's Ctrl table — `^G` for *go*, `^O` for the jump-back every vi has. The
back chord is advertised in the status line on arrival rather than in the
standing hints, which is exactly when there is something to go back to.

Following is the **metadata pane's** gesture: from the body there is no row to be
standing on, and following whatever the other pane was last left on would be a
guess, so the host says so instead. A row that is not a link says that too, and
so does a link that lands somewhere that is not a file you can open — a URL, a
`#locator` into this document, a reference into a workspace prov cannot locate,
or a target that is simply not on disk. Each of those is a real answer rather
than a failure, and the status line gives it.

Leaving a document with **unsaved changes is refused**, with no second-press
escape hatch. Quitting has one because quitting twice discards work you were
told about and meant to discard; following a link is a *reading* gesture, and an
edit lost to one would be an edit lost to something nobody thinks of as
destructive.

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
