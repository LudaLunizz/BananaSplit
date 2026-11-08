# The shared GUI layer

The control surface every BananaSplit app serves on `127.0.0.1:8790`: the HTTP
server, the state JSON, the `POST /api/cmd` commands, the port file and the
window process. It is **source files, not a crate**: each app compiles it into
its own binary,

```rust
#[path = "../../gui/mod.rs"]
mod gui;
mod gui_platform;
```

so there is one copy to fix and review instead of three.

| file | |
|---|---|
| `mod.rs` | the server, the state, `command()`, the client helpers (`probe`, `request_show`, `request_quit`, ...). All three apps. |
| `cmd.rs` | `POST /api/cmd` as a typed `Cmd`: `Cmd::parse` reads and checks the form (it needs no mixer, so its rules are tested alone). |
| `apply.rs` | `apply(hub, Cmd)`: doing it on the mixer; `command(hub, body)` is parse, then apply. |
| `api.rs` | `ApiError { status, msg }`: every refusal the server makes, and its one encoding, `{"error":"..."}` (`application/json`). A request that works answers as before: plain text for a command, JSON for a state or a list. |
| `peer.rs`, `portdir.rs` | the per-system readers and choices that need no system call (see below). All three apps. |
| `winmsg.rs` | what the mixer and its window say to each other: `ToWindow` and `FromWindow`, with the line each is on the wire. All three apps: the window processes read and write the lines, macOS passes the same messages through its event loop. |
| `window.rs` | the window process (`bananasplit --window`) as seen from the mixer. Linux and Windows; macOS has its window in its own process. |

## The platform contract

The shared files reach the operating system only through `crate::gui_platform`,
a module each app provides with the same items (`linux/src/gui_platform.rs`,
`windows/src/gui_platform.rs`, `macos/src/gui_platform.rs`). The list, with what
each item is, is the table at the top of `mod.rs`. If an app misses one, or gives
it another signature, that app does not compile, and each app's CI compiles it
on its own system.

Rules:

* No `cfg(target_os)` in the server code. A difference between the systems is a
  function in `gui_platform` (the one place that is allowed to be different),
  with the shared code calling it. (`window.rs` is declared for the apps with a
  window process; that is the only `cfg` here.)
* Behaviour stays the same on every system unless the platform really differs:
  the HTTP responses, the `/api/state` JSON and the window protocol are one
  thing, tested once in the shared tests.
* The system calls stay at the leaves. Anything that decides something (who may
  connect, where a file goes) is a plain function that can be tested without the
  system. Those that differ per system live in `peer.rs` (reading each system's
  table of connections) and `portdir.rs` (the folder of the port note), compiled
  as `#[cfg(any(target_os = "...", test))]`: an app only contains its own, and
  `cargo test` on any system, Linux CI included, tests all three.
* Do not touch the localhost checks (Host, Origin, custom header, same user) or
  the window process's isolation and reaping without a test that shows why.

The page (`gui.html`) is one file too. It holds the parts that only one system
shows (the "Virtual cables" window of Windows and macOS, a few texts): the server
sets `<html data-platform="linux|windows|macos">` and the page hides what is not
its system's, with `data-only="windows macos"` in the markup and `PLATFORM`/`MAC`
in the script. A change to one system's part must not move the others: the
tests check that every `data-only` names a system and that no element id is
used twice (three systems' markup now share one document).

## Adding a command

A variant of `Cmd` (what it carries, already checked), an arm of `Cmd::parse` (what it takes, in
the order the faults should be said), an arm of `apply` (what it does), and a case in the tests of
`cmd.rs`. What can only be known from the mixer's state (an updater, a loop in the routing, a take
that is gone) is `apply`'s to refuse; everything else is `parse`'s. Refuse with `ApiError` (a `String`
or `&str` is a 400): the page shows its message, `bananasplit --quit` prints it.
