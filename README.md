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

**First: a TUI**, embedding [`leaf-ratatui`](https://github.com/diaryx-org/leaf)
and [`flower-ratatui`](https://github.com/diaryx-org/flower) — the two widget
crates that already exist for exactly these two editors. No FFI: it is one Rust
binary linking one copy of each library, which makes it the cheapest possible
test of whether the composition holds up under a real event loop, and the
fastest thing to iterate the core against.

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
cargo test
cargo clippy --all-targets -- -D warnings
```

## License

MIT or Apache-2.0, at your option.
