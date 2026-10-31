# Monocode Paseo (experimental)

**English** | [简体中文](README.zh-CN.md)

A fork of the native GPUI client of [MonoCode](https://github.com/hardbeat920/monocode) (synced to `native-v0.16.0-preview.11`) that uses a local [Paseo](https://github.com/getpaseo/paseo) daemon as its backend. The UI is the original MonoCode native shell; conversations, execution and history stay in Paseo, so the Paseo apps and this client see the same conversations and can continue each other's work.

> ⚠️ **Personal experiment.** Only used on the author's own macOS (Apple Silicon) machine with Paseo 0.14.0. No guarantees on behavior, compatibility or stability, and no support. For the real thing, use upstream MonoCode. The upstream README is kept as [README.upstream.md](README.upstream.md).

## Usage

Requirements: macOS; a Paseo daemon already running locally (default `127.0.0.1:6973`, loopback only); Rust stable; Node (tested with 24.22.0); a Paseo source checkout with its dependencies installed (used only at packaging time to bundle the official SDK; the installed app does not need it).

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
- Sub-agents nest under their parent conversation; linked worktrees of the same Git repository share one project entry.
- Permission approvals, answering questions, and cancelling the current turn.
- Sending while a turn is running: Settings → Chat → "Follow-up behavior" chooses **Queue** (sent automatically when the turn ends, editable and removable) or **Steer** (inserted into the current turn; providers that cannot steer get the turn interrupted and replaced by Paseo). Cmd+Enter uses the opposite behavior for that message. The queue lives only in this client, and both ends see a message only after it is sent; uncertain results are never resent automatically. Steer supports plain text only.
- Sidebar: filtering, grouping by project or **by status** (needs input → failed → unread → working → done), sorting by latest activity or creation time; archive and unarchive, rename, pin (top-level conversations), and folders.
- Project icons and the bottom project switcher (keep only the projects you use; add others back by hand).
- Read-only file and Git panels; a **local terminal** for projects on this machine.
- Local notes; **English / 简体中文** UI (Settings → General → Language, follows the system by default).

**Hidden / not implemented**
- Usage display; remote PTY / relay / tunnel.
- Deleting conversations, labels, paging through older history.
- Quick input, Monos, automations, Inbox, multi-agent orchestration and other features that depend on upstream's local execution.
- Permission picker: new conversations use the provider's full-access mode, and it is not shown in the UI.

## Credits and license

- Upstream MonoCode (MIT); see [NOTICE](NOTICE) for GPUI. Licenses for the Paseo SDK and its dependencies ship with the package.
- The Chinese strings come from [hardbeat920/monocode#1073](https://github.com/hardbeat920/monocode/pull/1073) (kev17in, MIT).
- This fork is also MIT licensed; see [LICENSE](LICENSE).
