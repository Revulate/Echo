# Plugins and scripting

Echo has no plugin or scripting API, and embedding a scripting runtime is out
of scope. This page explains why and lists the built-in features that cover
the common reasons for wanting one.

## Why there is no scripting runtime

- Message parsing, layout and rendering are Echo's hot path. A scripting VM
  with per-message hooks would run third-party code inside exactly that path,
  making frame pacing and memory use depend on arbitrary user scripts.
- A stable plugin API would have to expose internals (message construction,
  commands, HTTP, rendering) that change as Echo evolves. Keeping that surface
  compatible would be a permanent maintenance cost.

## Built-in alternatives

The usual plugin use cases are native features:

- custom commands with placeholders, import/export, and enable/disable state
  (`src/backend/commands/`)
- expression-based filters with message, user, channel, moderation and
  system-event variables (`src/backend/filters.rs`)
- highlight and ignore rules with sounds and visual priority
- per-event desktop notifications and sound events
- a configurable image uploader with custom headers and response parsing
- link handling with incognito variants, and link previews
- JSON theme files and theme export

## Requesting a feature

If a workflow needs behavior these features can't express, open an issue that
describes the workflow rather than a plugin mechanism. The intended path is to
cover the need natively. A bounded, Rust-native extension point can be
considered for a concrete need; a scripting VM in the message hot path cannot.
