---
title: 'The metadata axes say they rewrite every document, and nothing does'
part_of: '[Tasks](/docs/tasks/tasks.md)'
status: open
author: adammharris
created: 2026-09-20
updated: 2026-09-20
---

# The metadata axes say they rewrite every document, and nothing does

## The problem

`config_schema.rs` marks `metadata.format` and `metadata.embed` as `costly`,
with the sentence a frontend shows before Apply:

> Rewrites the metadata of every document in the workspace.

It does not. Setting either axis is a one-line edit to the config document, and
prov's rule for a config axis is the opposite of the sentence
(`prov/docs/config-vocab.md`, "Restating existing documents"): the setting
governs what prov writes *next*; a document keeps the spelling it has; a mixed
workspace is valid and `check`-clean; `prov convert <root> <axis> <value> -r` is
the whole-workspace restatement, and it is a separate, deliberate command.
Nothing in prov, provui-core or any frontend runs it on Apply.

The truth is also narrower than "what is written next": `prov new` gives a
child its **parent's** carrier and falls back to the default only when the
parent has none (`prov/src/mutate/create.rs`, `create_titled`). So in a
workspace whose root is YAML, moving the axis to TOML changes nothing a person
will see until they convert — the new pages under the old root are still YAML.

A warning that overstates is one a reader learns to click through, which is
exactly what the per-value `Consequence` design was built to avoid
(`a_field_costly_in_one_direction_warns_on_that_direction_only`). And the
sentence hides the cost that *is* real: a frontend that parses an existing
block in the configured language rather than the one its fences declare breaks
on every pre-existing document. Diaryx had that bug (`diaryx` commit
`1a3f3333`); the sentence gave no hint of it.

## What to do

Two choices, and either is fine; what is not fine is the sentence as it stands.

1. **Say what happens.** Reword both consequences to prov's own rule — along
   the lines of *"Applies to documents written from now on. Existing documents
   keep their format until converted (`prov convert … -r`)."* — and consider
   whether `Tint::Warning` + `Severity::Confirm` is still the right weight for a
   change that rewrites nothing. A `Notice` may be honest; the mixed workspace
   it produces is legal but surprising, and the surprise is the thing worth a
   sentence.
2. **Make it true.** Offer the conversion as part of the change: provui-core
   exposes "convert the reachable set to the new axis" beside the config write,
   so a frontend's Apply can do what the sentence promises. Then the sentence
   stays, and `Confirm` is earned. This is the larger change and belongs with
   whichever frontend wants it; provui-core's part is the primitive.

`references.notation`, `references.path_style`, `references.target`, and
`spanning` carry their own `costly` sentences from the same helper and should
be checked against the same rule while this is open — a config axis governs
what is written next in every case, and the test
`a_field_that_rewrites_the_workspace_carries_the_warning_and_the_sentence`
asserts the tint and the presence of a sentence, not its truth.

## Done when

The consequence text on `metadata.format` and `metadata.embed` describes what
applying the change does, and a test holds the sentence to that — or the
conversion the sentence describes runs, and the test holds that instead.
