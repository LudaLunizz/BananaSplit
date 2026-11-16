# Contributing

Issues and pull requests are welcome. This file is the setup and the exact
commands; [ARCHITECTURE.md](ARCHITECTURE.md) says where things live.

## Setup

You need a stable Rust toolchain, **1.88 or newer** (the `rust-version` of all
four manifests; CI checks it), with `clippy` and `rustfmt`.

The repository holds four Cargo projects with their own `Cargo.lock`, not one
workspace:

| Directory | What |
|---|---|
| `core/` | the shared mixer (library `bananasplit-core`, no dependencies) |
| `linux/` | the Linux app |
| `windows/` | the Windows app |
| `macos/` | the macOS app |

Work in the directory of the project you touch. A change in `core/` can break any
of the three apps, so check all that use what you changed.

On Linux the build needs only Rust: JACK, GTK and WebKitGTK are opened at run
time (`linux/src/dl.rs`), so none of them is a build dependency. The end-to-end
test also needs `gcc` (it builds a mock JACK) and `python3`; the MP3 test uses
libmpg123 when it is installed and is skipped with a note when it is not. The
window test's packages are listed in the "Linux window and tray" job in
`.github/workflows/ci.yml`.

## Before you open a pull request

Run these in the project you changed. They are what CI runs (`.github/workflows/ci.yml`):

```sh
cargo build --locked
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

Run `cargo fmt` in **every** project you touched (CI checks all four, and
`macos/` and `windows/` are easy to forget). Dependencies are audited with
`cargo deny --config ../deny.toml check` from each project directory.

### Linux

```sh
cd linux
cargo build --locked --release
sh tests/e2e/run_e2e.sh          # the real binary against a mock JACK
sh tests/window/run_window_test.sh   # the window and tray, with fakes
```

The end-to-end test starts a real BananaSplit and uses the control port (8790):
stop a running BananaSplit first, or run it in a sandbox. For the same reason
one unit test, `cables::tests::names_that_share_cables`, fails while a real
BananaSplit is running on the computer; it passes otherwise.

The browser tests (`tests/gui/ui_check.js`) need Node and Playwright; the CI job
"Linux GUI (Playwright)" shows the steps.

### Windows and macOS code from another system

CI runs both natively. To check that your change still compiles and passes
clippy when you are on Linux:

```sh
# Windows (needs: rustup target add x86_64-pc-windows-gnu)
cd windows && cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings

# macOS (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin);
# DOCS_RS=1 lets it type-check without Apple's SDK
cd macos && DOCS_RS=1 cargo clippy --target aarch64-apple-darwin --lib --bins -- -D warnings
```

These do not run any test. Platform code that CI cannot run for real (a real
sound card, a real Mac) is marked as untested in the release notes.

### Changes to the page

`gui/gui.html` is one page for all three systems. If you change it, see
`gui/tests/pages/README.md`: it renders the page the way each system serves it, so
you can see that a change meant for one system did not move the others.

## Conventions

- **Formatting** is `rustfmt` with `rustfmt.toml` (edition 2024 style, 120 columns).
- **Every `unsafe` block gets a `// SAFETY:` comment** saying why it is sound.
- **Every public item gets a doc comment** (`missing_docs` is on in `core/`). A
  function returning `Result` has an `# Errors` section; one that can panic on
  caller input has `# Panics`.
- **New public errors are types**, not `Result<_, String>`: see `core/src/error.rs`
  and `ConfigError`. `core/` has no dependencies, so errors implement `Display` and
  `Error` by hand.
- **`core/` stays free of dependencies and operating-system calls.**
- **The audio thread does not allocate.** If you touch the mixing path, run the
  `*_no_alloc` tests in `linux/tests/`.
- **The API is documented** in `docs/api.md`, and an end-to-end test checks the
  document against the server: a new operation or state field needs a row there.
- Say in the release note section (`linux/README.md`, "What's new") what a user
  sees; `CHANGELOG.md` is built from those sections.

## Issues and pull requests

Open an issue first for anything beyond a typo, saying what you want and why.
Work on a branch named `<issue number>-short-name`, and start the pull request
description with `Closes #<number>`. Wait for CI to be green; the maintainer
merges with a rebase, so the history stays linear.

Developed with help from Claude Code; every change is reviewed, tested in CI and
released by the maintainer.

## Releases

Releases are made by the maintainer: a version bump (the three `Cargo.toml`
and `Cargo.lock` files, the "What's new" section in `linux/README.md`, the
`metainfo.xml` release entry), a tag on the merge commit, packages built by
`.github/workflows/release.yml`, and a signature made with a key that is not on
GitHub (`linux/packaging/sign-release.sh`). The in-app updater installs a release
only if its `SHA256SUMS.txt` is signed; see the README.
