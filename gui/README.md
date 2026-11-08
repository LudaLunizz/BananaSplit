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
| `peer.rs`, `portdir.rs` | the per-system readers and choices that need no system call (see below). All three apps. |
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

The page itself (`gui.html`) is still one file per app, next to its
`gui_platform.rs`.
