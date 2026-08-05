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

Each crate declares `kani-shared` as a git dependency on the server repository —
what a third-party author would use. It resolves without credentials now the
server is public.

It does not yet *compile*, because the dependency tracks `develop` and develop
still carries kani-shared v0.1.0, whose `ChapterInfo` predates `page_count`. The
workspace therefore carries a `[patch]` onto the sibling checkout, which is why
the two directories must sit side by side for now.

Delete the patch once the road-to-1.0 branch has landed on develop. Stage 4 then
publishes `kani-shared` and the git dependency becomes an ordinary version one,
at which point this repository builds standalone with no sibling checkout.

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
