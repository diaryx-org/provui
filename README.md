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
  `fixity` is one of three words. With it, `id_storage` becomes a picker instead
  of free text, and a typo like `fixity: alll` — which prov silently ignores,
  keeping the default — stops being reachable.

Every spelling in `config_schema` is prov's own. The term lists mirror
`prov::diagnose`'s accepted values, and the tests assert exactly that: each
offered term is round-tripped through prov's linter, so a spelling prov renames
fails the build here rather than drifting into a picker that writes values prov
ignores.

## Scope

The single-document metadata surface — prov's `edit` layer. Relation fields that
maintain inverse links *across* documents belong to prov's `mutate` layer, which
wants a later, relationship-aware backend rather than a wider version of this
one.

Saving writes bytes directly. A frontend that wants fixity and `updated`
restamping maintained routes the write through prov's `Storage`/`mutate` layer
instead; this is the floor it builds on, not a policy it inherits.

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

It opens the file through `DocumentSession` and draws the document's two regions
with the two widgets that exist for them — `leaf-ratatui` over the prose,
`flower-ratatui` over the frontmatter. Everything about the document belongs to
the session; the binary owns the terminal, the split, the focus and one status
line, and nothing else.

### The panes

```
 flower — ▶ document.md ●            ┐
 ‹document›                          │  metadata band: a third of the height,
  title      New Title               │  bounded to 9…14 rows
  draft      true                    │
  j/k · l/h in/out · e edit · x del  ┘
   leaf — body                       ┐  body label (▶ marks the focused pane)
 # Heading                           │
                                     │  the body gets every row the band and
 Original body.                      │  the status line do not
                                     ┘
 document.md ○ saved  focus: body   ^W pane · ^S save · ^Q quit
```

**A horizontal band, not a side-by-side split.** That is the widgets' decision
rather than a taste: flower collapses its own two-pane page view below 64
columns, and half of an 80-column terminal is 40 — so a vertical split would
silently degrade the metadata view on the most ordinary terminal there is. Prose
wants the width too. Stacking gives both panes the full width and spends the one
scarce dimension, height, on the surface that is the point.

The band is sized against `flower_ratatui::page_room`, which says how many item
rows survive the widget's own three rows of chrome, and the model's inline budget
is refit to the **pane's** height rather than the terminal's on every frame. The
floor of 9 rows is where flower's budget stops using extra room anyway; the
ceiling of 14 is where a band of mostly-empty list starts costing the prose. When
the terminal is too short for both minimums the split is **abandoned rather than
shrunk**, and whichever pane holds the keyboard takes the screen. A whole-file
config document has no prose region at all, and is all metadata.

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
| `^Q` | quit; refused once while there are unsaved changes |
| body pane | leaf's keys (`leaf --help`) |
| metadata pane | `j`/`k` move · `l`/`h` in/out · `e` edit · `x` delete |

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

**`provui-tui` additionally needs the two widget crates, which are not on
crates.io yet.** They are consumed the prepublication way — by the version each
repo declares, with a `[patch.crates-io]` supplying it — so that no manifest here
carries a path across a repository boundary and nothing has to be undone to
publish. In this working tree that patch is `~/diaryx/.cargo/config.toml`, copied
from `~/diaryx/.cargo/patches.toml` with four entries uncommented:

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
show exactly one.

`provui-core` alone needs none of this: it depends only on published crates.

## License

MIT or Apache-2.0, at your option.
