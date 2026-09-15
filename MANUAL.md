# Bolt — Launcher Manual

A Raycast-style application launcher for Fedora Linux/Hyprland, written in
Rust with GTK4. A resident **daemon** keeps a borderless, translucent search
window off-screen; a small control client (`launcherctl`) shows, hides and
quits it; plugins extend the search box with a calculator and a persistent
clipboard history.

---

## Contents

1. [Building](#building)
2. [Running](#running)
3. [The launcher window](#the-launcher-window)
4. [Configuration](#configuration)
5. [Plugins](#plugins)
   - [echo](#echo)
   - [calculator](#calculator)
   - [clipboard](#clipboard)
6. [Hyprland integration](#hyprland-integration)
7. [Data and IPC internals](#data-and-ipc-internals)
8. [Troubleshooting](#troubleshooting)

---

## Building

Requires the GTK4 development files (Fedora 42 ships GTK **4.18.x**; the
`v4_*` feature in `crates/ui/Cargo.toml` must match your installed series):

```bash
sudo dnf install gtk4-devel                 # pulls in glib2-devel, pango-devel, ...
sudo dnf groupinstall "Development Tools"   # if no C/Rust toolchain is present
```

Install Rust if needed (`curl https://sh.rustup.rs | sh`, or `sudo dnf install
rust cargo`), then build from the workspace root:

```bash
cargo check --workspace    # type-check, no binaries (fastest)
cargo build --workspace    # produce the binaries
```

Binaries land in `target/debug/`:

| Binary | Role |
| --- | --- |
| `launcher-daemon` | The resident daemon (GTK window + IPC + plugins). |
| `launcherctl` | Command-line client that controls a running daemon. |

---

## Running

Start the daemon **from the workspace root** so it finds `config/config.toml`:

```bash
cargo run -p launcher-daemon
```

or, for the built binary:

```bash
target/debug/launcher-daemon
```

You should see startup lines like:

```
launcher-daemon: listening on /run/user/1000/raycast-launcher.sock
launcher-daemon: theme = auto, shortcut = SUPER+SPACE
launcher-daemon: plugins: echo, calculator, clipboard
launcher-daemon: indexed 156 applications (3 skipped)
```

The window is created hidden. Control it from another terminal:

```bash
target/debug/launcherctl toggle    # show or hide the window
target/debug/launcherctl show
target/debug/launcherctl hide
target/debug/launcherctl quit      # shut the daemon down
```

With no argument, `launcherctl` defaults to `toggle`. The exit code is `0`
for `OK` responses and `1` otherwise; if the daemon is not running you get a
`Connection refused` error.

### Environment variables

| Variable | Purpose |
| --- | --- |
| `LAUNCHER_CONFIG` | Path to the config file instead of `config/config.toml`. |
| `LAUNCHER_SOCKET` | Override the `launcherctl` socket path. |
| `XDG_RUNTIME_DIR` | Where the control socket lives (`raycast-launcher.sock`). |
| `XDG_DATA_HOME` / `HOME` | Where clipboard history is persisted. |

The daemon exits cleanly on `SIGINT`/`SIGTERM` and stops the clipboard monitor
(taking any pending saves with it).

---

## The launcher window

The window is a borderless, centered, semi-transparent search list.

| Key | Action |
| --- | --- |
| `Type` | Search applications by fuzzy (Sublime-Text-style) match; plugin results appear above apps. |
| `↑` / `↓` | Move the highlighted row (wraps around). |
| `Enter` (or KP enter) | Activate the row: run the plugin action, or launch the app. |
| `Esc` | Hide the window and clear the query. |

An empty query shows the full application list in the index's natural order.
Closing the window with the compositor (e.g. `Alt+F4`) only hides it; the
daemon keeps running.

Enter on a plugin row:

- **calculator** — copies the computed value to the system clipboard;
- **clipboard** — copies the full stored text back to the system clipboard;
- **echo** — purely informational, nothing runs.

Enter on an application row launches it (the `Exec` line never touches a
shell; `%` field codes are expanded directly).

---

## Configuration

The configuration file is `config/config.toml` (overridable with
`LAUNCHER_CONFIG`). A minimal example:

```toml
shortcut = "SUPER+SPACE"
theme = "dark"

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

[clipboard]
retention = "1_week"
```

### Top-level keys

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `shortcut` | string | `SUPER+SPACE` | The shortcut shown in startup output; the actual binding is handled by the compositor (see [Hyprland integration](#hyprland-integration)). |
| `theme` | string | `dark` | Theme used only when `[appearance] theme` is empty. |
| `enabled_plugins` | array of strings | `[]` (all) | Allow-list of plugin ids (`applications`, `echo`, `calculator`, `clipboard`). An empty list keeps every registered plugin active. |

### `[appearance]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `theme` | string | *(top-level `theme`)* | `auto` follows the system colour scheme via the XDG portal; `light`/`dark` force a palette. |
| `accent_color` | string (`#rrggbb`) | `#0a84ff` | Accent colour for the caret and selected-row highlight. |
| `blur_enabled` | bool | `true` | Translucent background, blurred by the compositor. `false` = opaque. |
| `corner_radius` | integer | `16` | Window corner radius in pixels. |
| `window_width` | integer | `640` | Window width in pixels. |
| `clipboard_persistence` | bool | `true` | Whether copied text is remembered between sessions. `false` keeps the history memory-only. |

### `[clipboard]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `retention` | string | `1_week` | How long copied entries are kept. `session` disables persistence entirely; see [clipboard](#clipboard). |

`retention` accepts (case-insensitively):

| Value | Meaning |
| --- | --- |
| `session` | Memory-only: no file is written, entries live for the current daemon run. |
| `1_day` | Entries older than 24 h are pruned; history is persisted. |
| `1_week` | Entries older than 7 days are pruned; history is persisted. |
| `1_month` | Entries older than ~30 days are pruned; history is persisted. |

Parsing is `serde` + `toml` in `crates/core/src/config.rs`. Any section you
omit falls back to its defaults, so older config files keep working.

---

## Plugins

The launcher consults plugins on every keystroke. A matching plugin's rows
appear **above** up to ten fuzzy-ranked app matches; a plugin that matches
but produces no rows shows a greyed-out hint instead of an empty list. With
no matching plugin, only applications are shown.

### echo

`echo: <text>` echoes exactly what you type after the prefix (`echo: hello`
shows an `echo: hello` row). The example plugin; it cannot be missed.

### calculator

The calculator has **no prefix**: it activates exactly while the query is a
valid arithmetic expression, so ordinary searches such as `firefox` never
trigger it. Type `2 + 2` and the list shows `4`; Enter copies the value to
the system clipboard.

Supported syntax:

- Operators: `+ - * / ^` and parentheses. Evaluation is `f64` (no integer
  division), parsed by `meval` in a bare context — **no constants, no
  function calls, no variables**.
- **Superscript exponents**: `2²` = 4, `2³` = 8, `2¹⁰` = 1024, `2⁻²` =
  0.25. The unicode superscripts `⁰¹²³⁴⁵⁶⁷⁸⁹⁻` are normalised into
  `^(...)`.
- **Root binder**: `a//b` means the b-th root of `a` (`a^(1/b)`).
  `16//4` = 2, `27//3` = 3, `81//4` = 3. `//` binds tightest (same as `^`)
  and is right-associative:

  | Expression | Result |
  | --- | --- |
  | `16//4` | `2` |
  | `2^8//4` | `4` — `2^8`, then `//4` |
  | `8//2^2` | `1.6817928305` — the square root of `2^2`, i.e. `8^(1/4)` |
  | `2//3+1` | `2.2599210499` — `2//3`, then `+1` |
  | `(1+3)//2` | `2` — parentheses make the left operand explicit |

  Superscripts work alongside it: `2²//2` = 2.

### clipboard

The `clip:` prefix searches your clipboard history.

| Query | Shows |
| --- | --- |
| `clip:` | The whole history, newest first. |
| `clip:rust` | Only entries containing `rust` (case-insensitive). |
| `CLIP:repo` | Same — the prefix is case-insensitive too. |

Every row previews the stored text and reports its length and age
(`42 characters · 3h`). Enter copies the **complete** original text back to
the system clipboard.

How it works:

- A background **monitor thread** polls the system clipboard every 500 ms
  through `arboard` (Hyprland's `wlr-data-control` protocol). Only *new*
  text is recorded — unchanged clips are skipped, and an empty clipboard is
  not a change.
- The history is a bounded list, newest first, capped at **50** entries.
  Re-copying older text moves it to the front instead of duplicating it.
- With a retention of `1_day`/`1_week`/`1_month` the history is persisted
  **atomically** (temp file + rename) after each real change to
  `$XDG_DATA_HOME/launcher/clipboard_history.json` (fallback
  `~/.local/share/launcher/`). Persisted entries carry a capture timestamp,
  and stale entries are pruned by age on access.
- `session` retention (or `clipboard_persistence = false`) keeps the history
  **in memory only** — nothing is ever written to disk, protecting your
  clipboard privacy across restarts.
- If capture is unavailable (no Wayland data-control backend), the daemon
  logs a warning and runs without live capture; previously persisted history
  is still loaded and searchable.

The JSON file stores `[{"text": "...", "timestamp": 1710000000}, ...]`.
Legacy plain string arrays (`["...", ...]`) are still read and treated as
freshly copied.

---

## Hyprland integration

The shortcut is **not** handled by the daemon — the compositor owns the key,
spawns `launcherctl toggle`, and the daemon does the rest. Add to
`~/.config/hypr/hyprland.conf` (see `config/hyprland.conf.example`):

```
bind = SUPER, SPACE, exec, launcherctl toggle
```

`launcherctl` must be on the PATH Hyprland uses to exec (an absolute path
works too). The daemon must already be running.

Float and style the window (it is a regular borderless toplevel, not a
layer-shell surface):

```
windowrulev2 = float,      class:^(io.github.bolt)$
windowrulev2 = noborder,   class:^(io.github.bolt)$
windowrulev2 = blur,       class:^(io.github.bolt)$
windowrulev2 = center,     class:^(io.github.bolt)$
```

- The `class` pattern matches the GTK application id `io.github.bolt`
  (`DEFAULT_APP_ID` in `launcher-ui`); Hyprland derives the Wayland app id
  from it.
- Frosted glass is Hyprland's job: the compositor blurs whatever sits behind
  the translucent window. To strengthen it raise `blur { size }`/`passes`
  (see the example file). Under X11 there is no compositor blur — the window
  is simply translucent.
- If a backend issue arises, force GTK to Wayland: `GDK_BACKEND=wayland`.

---

## Data and IPC internals

- **Control socket**: `$XDG_RUNTIME_DIR/raycast-launcher.sock` (mode
  `0600`), else a private `/tmp/launcher-<user>/` directory (mode `0700`).
  A stale socket left by a crashed daemon is detected and reused.
- **Protocol**: line-based. `launcherctl` writes one command (`TOGGLE`,
  `SHOW`, `HIDE`, `QUIT` — case-insensitive) and reads one response line.
- **Clipboard history**: `$XDG_DATA_HOME/launcher/clipboard_history.json`.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `launcherctl: is the daemon running?` | Start the daemon first (`cargo run -p launcher-daemon`, from the workspace root). |
| `another instance is running on ...` | A live daemon already owns the socket; use `launcherctl toggle` instead. |
| Startup complains about the clipboard backend | The compositor lacks `wlr-data-control`; live capture is off but `clip:` still searches persisted history. |
| History does not survive a restart | `retention = "session"` or `clipboard_persistence = false` are set — add a retention to enable persistence. |
| Confusing GTK backend behaviour on Hyprland | Prefix the daemon with `GDK_BACKEND=wayland`. |
| Window not floating/centred | Add the `windowrulev2` lines from [Hyprland integration](#hyprland-integration). |