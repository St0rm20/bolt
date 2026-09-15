# Launcher

A Raycast-like application launcher for Fedora Linux running Hyprland,
written in Rust with GTK4 (`gtk4-rs`).

**Status:** a resident daemon with a Unix-socket control interface and a
borderless, translucent GTK4 launcher window. `launcherctl` sends
`TOGGLE`/`SHOW`/`HIDE`/`QUIT` to a daemon that forwards them to the window;
an application indexer in `launcher-core` reads `.desktop` entries from the
system and user application directories into an in-memory index that the
search box filters live with Sublime-Text-style fuzzy matching. A base plugin
system is live: plugins (a `Plugin` trait, a central `PluginRegistry`, a
unified result list) with action support — the `echo:` example, a prefix-free
`calculator` that evaluates `2 + 2` inline and copies the value on Enter, and a
`clipboard` plugin (`clip:`) whose background monitor keeps a bounded,
persistent history of copied text. More plugins and a full plugin runtime are next.

## Purpose

The goal is a launcher that can grow without entangling the daemon (core),
the graphical interface (GTK), and the plugin system. Each concern lives in
its own crate so that, for example, plugins can be added without touching UI
code and vice versa.

The UI is split into a GTK-free **model** (`LauncherState` in `launcher-core`)
that owns the index, the query and the highlighted row, and a thin GTK
**view** (`launcher-ui`) that renders it. Everything the window shows is
derived from that one state, so search/selection logic is testable without a
display.

Search itself lives in `crates/core/src/search.rs`: the `fuzzy-matcher` crate's
Sublime-Text-style matcher (`SkimMatcherV2`) scores each application (preferring
prefix, word-boundary and close-sequence matches), sub-threshold results are
dropped, and the rest are sorted by descending relevance. An empty query
short-circuits to the full list in the index's natural order.

Each keystroke first consults the plugin registry: the first plugin whose
activation rule matches the query contributes its rows *above* the applications
(a prefix such as `echo:`, or a triggerless rule like the calculator's
"query is a valid expression"). Up to ten fuzzy-ranked app matches follow
beneath; a matching plugin that yields no rows shows a greyed-out hint instead
of silence. That merge happens in `LauncherState`
(`crates/core/src/launcher_state.rs`) and produces a unified `ListRow` list
(`App` | `Plugin` | `Hint`) the GTK view just renders — see
[Plugin system](#plugin-system).

## Architecture

The workspace is split into five crates under `crates/`:

| Crate | Library/Binary | Responsibility |
| --- | --- | --- |
| `core` (`launcher-core`) | Library | Configuration loading (`serde` + `toml`), the IPC protocol (a `Command` enum), the Unix socket plumbing, the fuzzy search/ranking module (`fuzzy-matcher`), and the result model (`LauncherState`/`ListRow`) that merges app search with plugin results. **No GTK, no tokio.** |
| `ui` (`launcher-ui`) | Library | GTK4 user interface only. `launch()` runs the application; `CommandHandle::dispatch` forwards socket commands to the GTK main thread via `glib::MainContext::invoke`. Renders application and plugin rows alike. |
| `daemon` (`launcher-daemon`) | Binary | Resident daemon. Owns the tokio runtime, binds the control socket, loads config, registers the plugin registry (built-ins filtered by `enabled_plugins`), drives the GTK UI, forwards SIGINT/SIGTERM as `QUIT`. |
| `launcherctl` | Binary | CLI client: `launcherctl [toggle|show|hide|quit]` talks to the running daemon over the socket. |
| `plugins` (`launcher-plugins`) | Library | Plugin system: the `Plugin` trait (activation + results), `PluginResult` with an optional `PluginAction` (e.g. clipboard copy), the central `PluginRegistry` (dispatch + enable/disable), and the built-in `echo`, `calculator` (meval-based expression evaluation) and `clipboard` (bounded, persistent text history fed by a background monitor) plugins. GTK-free; GTK performs the actions. |

Dependency direction is one-way: `plugins` is a leaf (it has no workspace
dependencies, only external ones like `meval`); `core` and `ui` depend on
`plugins`; `ui`/`daemon`/`launcherctl` depend on `core`; `daemon` depends on
`ui`. Nothing except `ui` pulls in GTK.

Shared dependency versions are declared once at the workspace root
(`[workspace.dependencies]`).

## Required Fedora system packages

Developed against **Fedora 42**. Install the GTK4 development files:

```bash
sudo dnf install gtk4-devel
```

`gtk4-devel` pulls in the other libraries `gtk4-rs` needs at link time
(`glib2-devel`, `cairo-gobject-devel`, `pango-devel`, etc.). If no compiler
toolchain is present, install it as well:

```bash
sudo dnf groupinstall "Development Tools"
```

> **Assumption about the GTK version:** `gtk4-rs` is compiled against your
> system GTK4 via `pkg-config`, and the `v4_*` feature flag in
> `crates/ui/Cargo.toml` must match the installed major.minor. Fedora 42
> currently ships GTK **4.18.x**, so the workspace enables `v4_18`. If your
> release ships a different series, change that flag.

## Installing Rust

If `rustc`/`cargo` are not installed, the recommended way is `rustup`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version
```

Alternatively use the Fedora-packaged toolchain (may lag the upstream
release):

```bash
sudo dnf install rust cargo
```

## Building

From the workspace root:

```bash
cargo check --workspace
cargo build --workspace
```

- `cargo check --workspace` verifies that every crate type-checks without
  producing binaries; it is the fastest way to validate the code.
- `cargo build --workspace` produces the binaries. The daemon ends up in
  `target/debug/launcher-daemon` and the client in `target/debug/launcherctl`.

## Running

Start the daemon from the workspace root so it finds `config/config.toml`
(`LAUNCHER_CONFIG` overrides the path):

```bash
cargo run -p launcher-daemon
```

Expected output when started from the workspace root:

```
launcher-daemon: listening on /run/user/1000/raycast-launcher.sock
launcher-daemon: theme = dark, shortcut = SUPER+SPACE
```

The daemon creates the GTK window, hidden until a command shows it. Drive it
from another terminal:

```bash
target/debug/launcherctl toggle   # show/hide the window
target/debug/launcherctl show
target/debug/launcherctl hide
target/debug/launcherctl quit     # shut the daemon down
```

The exit code is 0 for `OK` responses, 1 otherwise. If the daemon is not
running, `launcherctl` reports a connection error (`Connection refused`).

## Configuration

The configuration file lives at the workspace root:

```
config/config.toml
```

```toml
shortcut = "SUPER+SPACE"
theme = "dark"

# Allow-list of plugin ids. Registered plugins not listed here are skipped at
# startup; an empty list keeps every registered plugin active.
enabled_plugins = [
    "applications",
    "echo",
    "calculator",
    "clipboard",
]

[appearance]
theme = "auto"
accent_color = "#0a84ff"
blur_enabled = true
corner_radius = 16
window_width = 640
clipboard_persistence = true
```

| Key | Type | Meaning |
| --- | --- | --- |
| `shortcut` | string | Keyboard shortcut that will open the launcher. `SUPER+SPACE` mirrors the Raycast convention. |
| `theme` | string | UI theme applied to the launcher window (e.g. `dark`, `light`). Used only when `appearance.theme` is empty. |
| `enabled_plugins` | array of strings | Allow-list of plugin ids. Registered plugins not listed are skipped at startup; an empty list keeps every registered plugin active. |

### `[appearance]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `theme` | string | (falls back to top-level `theme`) | `auto` follows the system colour scheme (XDG portal), `light`/`dark` force a theme. |
| `accent_color` | string (`#rrggbb`) | `#0a84ff` | Accent colour used for the caret and the selected-row highlight. |
| `blur_enabled` | bool | `true` | Keep the background translucent (the compositor applies blur). When `false` the background is opaque. |
| `corner_radius` | integer | `16` | Window corner radius in pixels. |
| `window_width` | integer | `640` | Window width in pixels. |
| `clipboard_persistence` | bool | `true` | Remember copied text between sessions. Disable for a memory-only history. |

Parsing is implemented in `crates/core/src/config.rs` with `serde` + `toml`
and is deliberately kept out of the GTK UI crate.

## Plugin system

A plugin extends the launcher with domain-specific results (a calculator, a
clipboard manager, ...). Activation is *prefix-based* by default, but a plugin
may override the rule — the calculator, for one, is active exactly while the
query is a valid arithmetic expression. Plugins live in the `launcher-plugins`
crate, which stays GTK-free.

The interface (`crates/plugins/src/lib.rs`):

- `PluginResult` — one list row a plugin contributes (`title` + optional
  `subtitle`).
- `PluginAction` — what the UI runs when a row is activated (Enter): `Copy`
  puts text on the system clipboard. Plugins only declare the action; the GTK
  layer performs it.
- `Plugin` — the trait every plugin implements:
  - `id()` / `name()` — stable identifier and human-readable name.
  - `prefix()` — optional trigger prefix such as `calc:` or `clip:`.
  - `matches(&str) -> bool` — whether the plugin should activate for the
    current query. The default is an ASCII-case-insensitive prefix check;
    override it for non-prefix activation rules.
  - `query(&str) -> Vec<PluginResult>` — the rows to display while active.
- `PluginRegistry` — the central, order-preserving collection the daemon
  fills at startup (`register`). `dispatch(query)` activates the *first*
  registered plugin that matches and returns its results; registration order
  decides when several claim the same query.

How queries are dispatched: every keystroke first asks the registry. A
matching plugin's rows come **first**, followed by up to ten fuzzy-ranked
application matches (`ListRow::App`) — apps never fuzzy-match `echo: …` or
`2 + 2`, so in practice only the plugin shows. A matching plugin that produced
no rows (e.g. an invalid expression) shows a single greyed-out `ListRow::Hint`
instead of an empty list. Without a matching plugin the plain fuzzy ranking is
shown. All row kinds share one result list in `LauncherState`, so
selection/navigation and Enter-activation work identically for each — Enter on
a plugin row runs its `PluginAction` rather than launching an app.

The daemon registers the built-ins and filters them by `enabled_plugins`
(`registry.retain`). The shipped plugins:

- `echo` (`crates/plugins/src/echo.rs`): type `echo: hello` in the launcher
  and the list shows an `echo: hello` row.
- `calculator` (`crates/plugins/src/calculator.rs`): type `2 + 2` and the list
  shows `4` above any app matches; Enter copies the value to the system
  clipboard. Expressions are parsed by `meval` in a bare context (only
  `+ - * / ^` and parentheses; no constants, no function calls), so ordinary
  searches like `firefox` never activate it.
- `clipboard` (`crates/plugins/src/clipboard/`): a `clip:`-prefixed history of
  copied text. A monitor thread owned by the daemon polls the X11/Wayland
  clipboard (`arboard`, using Hyprland's `wlr-data-control` protocol) twice a
  second; each *new* piece of text is pushed to the top of a bounded history
  (newest first, capped at 50 entries, real changes only) and atomically saved
  to `$XDG_DATA_HOME/launcher/clipboard_history.json` (fallback
  `~/.local/share/launcher`). Type `clip:` for the whole history, or `clip:
  sql` to filter it; Enter copies the chosen entry back to the clipboard. When
  compositor clipboard capture is unavailable the monitor logs a warning and
  stops — the launcher keeps working. Persistence is off for single-session
  use with `[appearance] clipboard_persistence = false`.

```txt
launcher-daemon: plugins: echo, calculator, clipboard
```

To validate end to end: start the daemon from the workspace root
(`cargo run -p launcher-daemon`), type `echo: hello` into the launcher's
search box and confirm the row appears; type `2 + 2` and confirm `4` shows
and Enter copies it; copy some text elsewhere, type `clip:` and confirm the
history appears and filters; plain text still only shows apps, and an empty
query still restores the full application list.

## GTK4 and Hyprland considerations

- GTK4 automatically selects the Wayland backend on Hyprland. If a backend
  issue occurs, force it explicitly: `GDK_BACKEND=wayland`.
- To try a theme without editing GTK settings, `GTK_THEME=Adwaita:dark`
  works for the standard widgets; the launcher's own palette follows the
  `theme` key in `config/config.toml`.
- The shortcut is not handled inside the launcher: it is delegated to the
  compositor. Add to `~/.config/hypr/hyprland.conf` (see
  `config/hyprland.conf.example`):

  ```
  bind = SUPER, SPACE, exec, launcherctl toggle
  ```

  `launcherctl` must be on Hyprland's `PATH` when it spawns the binding (an
  absolute path also works). Hyprland only spawns the client; the daemon does
  the actual show/hide and must already be running.
- The launcher window is a regular borderless toplevel that is **floated and
  centered by the compositor** — it does not use layer-shell. Add these rules
  (see `config/hyprland.conf.example`) so it floats, sits centered over the
  work area, drops its border and gets native frosted-glass blur:

  ```
  windowrulev2 = float,      class:^(dev.launcher.gtk)$
  windowrulev2 = noborder,   class:^(dev.launcher.gtk)$
  windowrulev2 = blur,       class:^(dev.launcher.gtk)$
  windowrulev2 = center,     class:^(dev.launcher.gtk)$
  ```

  The `class` pattern matches the GTK application id (`dev.launcher.gtk`,
  `DEFAULT_APP_ID` in `launcher-ui`). The blur is owned by Hyprland; the app
  only paints a semi-transparent background (see `appearance.blur_enabled`)
  and never does any blurring itself. Under X11, native compositor blur is
  not available the same way — the window is simply translucent.
- Operating without a desktop portal (common on minimal Hyprland setups),
  `appearance.theme = "auto"` falls back to `dark`.

## What's next (not implemented here)

- Extra ranking signals on top of the fuzzy score: launch frequency, recently
  used apps, favourites, desktop categories/keywords (the seam is
  `SearchRanker::score` in `crates/core/src/search.rs`).
- More concrete plugins and a real plugin runtime on top of
  `launcher-plugins`: more built-ins and actions (open a URL, run a command,
  ...), plugin-side input/output hops and dynamic loading.
- File indexing with an in-memory index (extend `AppIndexer` with a file-mode).
- Live tracking of the system colour-scheme changes via the XDG portal, so
  the theme switches without a restart.
- Overlay positioning (center-top) with layer-shell instead of a floated
  application window.