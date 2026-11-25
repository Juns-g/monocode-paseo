# Monocode Paseo (experimental)

**English** | [简体中文](README.zh-CN.md)

A fork of the native GPUI client of [MonoCode](https://github.com/hardbeat920/monocode) (synced to `main` (`09d04436`, 2026-11-12)) that uses a local [Paseo](https://github.com/getpaseo/paseo) daemon as its backend. The UI is the original MonoCode native shell; conversations, execution and history stay in Paseo, so the Paseo apps and this client see the same conversations and can continue each other's work.

> ⚠️ **Personal experiment.** Only used on the author's own macOS (Apple Silicon) machine with Paseo 0.17.4. No guarantees on behavior, compatibility or stability, and no support. For the real thing, use upstream MonoCode. The upstream README is kept as [README.upstream.md](README.upstream.md).
<img width="2600" height="1784" alt="image" src="https://github.com/user-attachments/assets/af4ad010-1e00-481f-b3a8-eaf71ecc07fd" />

## Why this fork (Architecture)

Upstream MonoCode Native and official Paseo each solve half of the problem well. This fork combines **MonoCode's Rust + GPUI native shell** with **Paseo's daemon and orchestration runtime** via a lightweight bundled Node sidecar (`paseo-host.mjs` / `host-adapter.mjs`) that translates MonoCode Host RPC onto the official `@getpaseo/client` SDK:

| Dimension | Upstream MonoCode Native | Official Paseo | This Fork (`monocode-paseo`) |
| --- | --- | --- | --- |
| **UI & Rendering** | Rust + GPUI + Metal (low memory, recursive split panes, `alacritty_terminal`, `tree-sitter`) | Expo / React Native + Electron wrapper (cross-platform & UI plugins, heavier desktop shell) | **MonoCode GPUI native shell** + tailored multi-agent sidebar, diff review & quota UX |
| **Orchestration & Runtime** | Local single-process (`monocode-core` + SQLite); `/operator`, file-scoped plans & Monos are local-only | Client–daemon split (`~/.paseo`), detached `terminal-worker`, HTTP MCP sub-agent trees, durable finish wakeups & cross-host routing | **Paseo daemon backend** — inherits MCP sub-agent orchestration, multi-client sync & remote hosts |
| **Lifecycle Boundary** | Closing the desktop app stops local orchestration | Clients connect over WebSocket; closing UI never kills running agents | **Daemon stays external by design** — rebuilding/restarting this UI never interrupts active agents |

- **Why Paseo daemon is not bundled into this app:** embedding the daemon would break C/S decoupling (every UI rebuild/restart would disrupt running agents and couple upstream Paseo upgrades). To use only this app day-to-day, run the Paseo daemon headless/standalone instead of keeping the Electron `Paseo.app` window open, and open official Paseo or the CLI only for low-frequency admin (plugins, relay, permissions).

## Usage

Requirements: macOS; a Paseo daemon already running locally (default `127.0.0.1:6973`, loopback only — can run headless without keeping `Paseo.app` open); Rust stable; Node (tested with 24.22.0); a Paseo source checkout with its dependencies installed (used only at packaging time to bundle the official SDK; the installed app does not need it).

```sh
RUSTUP_TOOLCHAIN=stable cargo build --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui --release
PASEO_SOURCE=/path/to/paseo python3 crates/monocode-gpui/paseo-prototype/package.py /tmp/monocode-paseo
open "/tmp/monocode-paseo/Monocode Paseo Prototype.app"
```

- Separate bundle ID `dev.usemono.paseo-prototype`; data lives in `~/Library/Application Support/Monocode Paseo Prototype/` and never touches upstream MonoCode's data.
- Authentication uses Paseo's official local credential lookup; no credentials are written into the source or the app.
- Optional config at `~/Library/Application Support/Monocode Paseo Prototype/prototype.json` (or the `PASEO_PROTOTYPE_CONFIG` environment variable):

```json
{ "endpoint": "127.0.0.1:6973", "serverId": "your serverId (optional)", "defaultProjectNames": ["Project A", "Project B"], "nodePath": "/path/to/node" }
```

- The packaged app is ad-hoc signed and not notarized. The Node path is recorded at packaging time; override it with `nodePath` if Node moves.
- Adapter internals: [crates/monocode-gpui/paseo-prototype/README.md](crates/monocode-gpui/paseo-prototype/README.md).

## Differences from upstream MonoCode

**Available**
- The original window, sidebar, tabs, split panes and composer, plus upstream preview features such as project color themes. Paseo conversations sync live; all replies are visible, with tool calls and thinking collapsed. Opening a conversation shows a preview tab (VS Code style) until you interact with it.
- A "working" line with a timer appears as soon as a turn starts, before the first output arrives.
- New conversations are created in Paseo only on first send, which avoids duplicates. They can start in a new workspace or join one of the 20 most recent workspaces in the same project. The default model follows the project default, then the Settings default, then the current conversation, then your most recent choice.
- Model and effort switching, presets, and favorite models (star a model in the picker; presets also appear under Favorites).
- Sub-agents nest under their parent conversation (including same-workspace sibling sub-agents); clicking a parent row keeps children collapsed with an aggregate status badge (needs input / failed / unread / working). Linked worktrees of the same Git repository share one project entry.
- Image/screenshot attachments in composer (paste, drag-and-drop, or upload) routed through Paseo's image payload API, preserved across `historyBefore` pagination.
- Built-in Codex & Antigravity quota/usage footer powered by Paseo's `listUsageReports()` (scrollable popover, reset countdown, pace forecast, and Paseo-style pin toggles to choose which items stay pinned in the footer).
- Paseo-native Rewind (idle-only, with confirmation modal supporting conversation-only or conversation+files revert) and Paseo-managed Git worktree creation/reuse on new drafts.
- Multi-host sidebar switcher connecting through the local Paseo daemon to remote Paseo hosts (such as devboxes) with strict remote filesystem/terminal/`file://` isolation.
- Permission approvals, answering questions, and cancelling the current turn.
- Sending while a turn is running: Settings → Chat → "Follow-up behavior" chooses **Queue** (sent automatically when the turn ends, editable and removable) or **Steer** (inserted into the current turn; providers that cannot steer get the turn interrupted and replaced by Paseo). Cmd+Enter uses the opposite behavior for that message. The queue lives only in this client, and both ends see a message only after it is sent; uncertain results are never resent automatically. Steer supports plain text only.
- Sidebar: filtering, grouping by project or **by status** (needs input → failed → unread → working → done), sorting by latest activity or creation time; instant incremental archive/unarchive, rename, pin (top-level conversations), and folders without full directory rescans.
- Project icons, bottom project switcher (keep only the projects you use; add others back by hand), and two-finger horizontal swipe on the sidebar (1:1 dual-page carousel with spring and edge rubber-banding) to switch projects.
- Read-only file and Git panels with multi-line Diff selection and inline review annotation tray (inserts formatted review notes into composer without auto-sending); a **local terminal** for projects on this machine.
- Performance hardening: bypasses shell shims to invoke native `git`, merges status/numstat queries with singleflight/TTL caching, removes remote 2s hard polling, and avoids holding the `Store` lock across remote RPCs.
- Older conversation history pagination (`historyBefore`).
- Local notes; **English / 简体中文** UI (Settings → General → Language, follows the system by default).

**Hidden / not implemented**
- Remote PTY / relay / tunnel; local filesystem/git panels for remote hosts.
- Deleting conversations and labels.
- Quick input, Monos, automations, Inbox, multi-agent orchestration and other features that depend on upstream's local execution.
- Permission picker: new conversations use the provider's full-access mode, and it is not shown in the UI.

## Credits and license

- Upstream MonoCode (MIT); see [NOTICE](NOTICE) for GPUI. Licenses for the Paseo SDK and its dependencies ship with the package.
- The Chinese strings come from [hardbeat920/monocode#1073](https://github.com/hardbeat920/monocode/pull/1073) (kev17in, MIT).
- This fork is also MIT licensed; see [LICENSE](LICENSE).
