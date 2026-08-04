# Kani extensions

Source extensions for [Kani](https://github.com/ArloB/kani), built as WASM
components against the `kani-extension` WIT world.

They live here rather than in the server repository so that the server is
neutral infrastructure: a takedown against any extension here does not touch
the server, its releases, or its images. The server ships no sources of its
own — only `kani-example`, which demonstrates the ABI and targets nothing.

## Layout

One crate per extension, each a WASM component exporting `manga-provider`.
`[package.metadata].id` is the extension id the server installs it under, which
is not always the crate name.

## Building

The toolchain lives in the server repository, and expects the two checkouts
side by side:

```
~/code/kani/              # the server, providing kani-cli and kani-shared
~/code/kani-extensions/   # this repository
```

From the server checkout:

```bash
cargo run -p kani-cli -- build kani-weebcentral --ext-dir ../kani-extensions
cargo run -p kani-cli -- build --all --ext-dir ../kani-extensions
```

Or set it once:

```bash
export KANI_EXT_DIR=../kani-extensions
export KANI_OUT_DIR=../kani-extensions/wasm_sources
cargo run -p kani-cli -- build --all
```

Output is a `.wasm` component per extension, ready to sign and publish to an
extension repository.

## The `kani-shared` dependency

Each crate depends on `kani-shared` by relative path (`../kani/kani-shared`),
which is why the sibling checkout matters. That is a stopgap: once
`kani-shared` is published, this becomes an ordinary version dependency and
this repository builds standalone, which is what a third-party extension author
needs. Until then, building requires the server checked out alongside.

## YAML extensions

Not every extension needs a crate. A YAML extension is a first-class artifact —
the server verifies and installs it exactly like a WASM component and then
interprets it at runtime, so it never becomes Rust at all. Prefer YAML for
sources the DSL can express; reach for a crate only when it cannot.

```bash
cargo run -p kani-cli -- validate my-source.yaml
```

## Provenance

Extracted from the Kani server repository, where these crates lived under
`kani-extensions/` until the split.
