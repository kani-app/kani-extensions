# Kani extensions

Source extensions for [Kani](https://github.com/kani-app/kani), built as WASM
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

## Publishing

`.github/workflows/publish.yml` builds and validates every extension on each
pull request with the newest Kani release's `kani-cli`, building against that
release's `kani-shared`. A push to `main` does the same, then lists the
extensions whose version is newer than the live repository. A maintainer
approves the `extension-repo` environment, and the deploy job then:

1. pulls the live repository and verifies it against the pinned
   `.github/maintainer.pub`;
2. signs each newer artifact into it, refusing to overwrite a version that
   already exists;
3. verifies it again, syncs it back, and confirms the server's `index.json`
   matches.

To release an extension, bump its version and merge. For a crate, the version,
`id` and `name` in `Cargo.toml` must match `metadata()` in its source; CI checks
this. A crate that should not be published sets `repo = false` under
`[package.metadata]`. Each published extension's `min_kani_version` is the
release it was built with, so an extension that needs unreleased host features
cannot be published until Kani ships them.

The run can be started by hand from the Actions tab, optionally naming a Kani
release, with `dry_run` to sign locally and show the sync without writing to the
server.

### Recorded responses

`tests/<id>/<endpoint>.har` is a recorded response for a YAML extension, and
`tests/<id>/<endpoint>.expected.json` is the rows the extension should extract
from it. CI replays every pair with `kani-cli repl replay`. That catches an edit
that breaks extraction, not a change on the site, and not request signing, since
hooks have already run on a recorded body.

To record one, run the solver image and point `kani-cli` at it (the URL needs
the `/v1`):

```bash
docker run -d -p 127.0.0.1:8191:8191 ghcr.io/kani-app/flaresolverr
KANI_SOLVER_URL=http://127.0.0.1:8191/v1 \
  kani-cli repl record comix.yaml popular page=1 -o tests/comix/popular.har
```

Trim what you commit: cut lists to a few entries and replace long text such as
synopses with a placeholder. The fixture only has to exercise the extraction, and
this repository should not carry the site's content. To write the expected
file, replay against `{"rows":[],"scalars":{}}` and save the "actual" block
after checking it.

### One-time setup

- An `extension-repo` environment with a required reviewer, and deployments
  limited to `main`.
- Environment secrets:
  - `KANI_AUTHOR_KEY` and `KANI_MAINTAINER_KEY`: the signing keys;
  - `KANI_REPO_SSH_KEY`: the SSH private key for the server.
- Environment variables: `KANI_REPO_HOST` (`user@host`) and
  `KANI_REPO_REMOTE_DIR`. Set `KANI_REPO_PORT` only when SSH does not use port 22.
- The server's public ED25519 host key is pinned in `.github/known_hosts`, under
  the `kani-extension-repo` alias. Its fingerprint is
  `SHA256:5YU6VlmZyUEBDhYVGOlAVCxHWDDHGT+hc6h+MB7mNYk`. Verify a replacement
  key against the server before changing this file.
- Optionally a repository secret `KANI_REPO_INDEX_URL`, the public `index.json`
  URL, so the summary before approval shows exactly what will be published.

## Provenance

Extracted from the Kani server repository, where these crates lived under
`kani-extensions/` until the split.
