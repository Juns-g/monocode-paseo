# Native Paseo adapter (experimental)

Normal launch uses the original GPUI Shell and an isolated Store. A bundled Node sidecar translates Monocode Host RPC to the existing local Paseo daemon using its official SDK. The legacy standalone window remains available only for explicit probes.

Build with Rust stable and Node (tested with24.22.0; other runtime versions not validated); the local Paseo source checkout must have its existing dependencies installed. No dependency installation or toolchain upgrade is performed by the packaging helper.

```sh
RUSTUP_TOOLCHAIN=stable CARGO_BUILD_JOBS=2 cargo build --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui --release
PASEO_SOURCE=/absolute/path/to/paseo node crates/monocode-gpui/paseo-prototype/bundle.mjs /tmp/native-paseo-resources
PASEO_SOURCE=/absolute/path/to/paseo python3 crates/monocode-gpui/paseo-prototype/package.py /tmp/native-paseo-candidate
```

The bundle records the Node executable found while packaging; prototype.json may override nodePath. The SDK is bundled; the installed app does not need the source checkout. Daemon authentication is resolved by the official local credential helper, never copied into source.

Runtime-only configuration: ~/Library/Application Support/Monocode Paseo Prototype/prototype.json (or PASEO_PROTOTYPE_CONFIG). Optional fields: endpoint (loopback only, default127.0.0.1:6973), serverId, defaultProjectNames, nodePath, paseoHome. Existing saved project selections win over new default names. With no custom defaults, the newest existing project is selected. A learned daemon identity is pinned in the existing isolated connection store, and reconnect/launch identity changes are refused unless explicitly configured.

### Recent-window directory (`directoryRecentDays`)

Optional prototype.json number. Absent: remote hosts list only agents updated in the last 7 days; the local host keeps the full scan. A positive number (fractions allowed): that window on every host, local included. `0`, a negative or a non-number value: full scan everywhere (old behaviour). A windowed directory requests `sort updated_at desc`, 200 per page, **unarchived only**, and stops at the first agent older than the cutoff. Older agents are not listed; opening one by id (e.g. a descendant-tree jump) fetches it directly and keeps it listed for the process lifetime (max 64). The UI search does not query the server, so older sessions are not findable from it; raise the window or set `0` to see them. Remote directories stay fresh for 30 s (local 2.5 s); mutations still force a refresh, and remote `connect` no longer waits for the directory.

### Remote hosts without the local daemon (`remoteHostTransport`, `proxy`)

Topology: the sidecar reads the host directory from `<paseoHome>/hosts.json` (re-read when mtime/size change) and, by default, opens each remote host **directly** with the official `createHostClient` (relay offers are end-to-end encrypted; ws/ssh targets work too). Every host still gets its own loopback Host RPC endpoint and adapter. The local daemon is only one more host, so Byte keeps working while it is stopped. The saved `connection` (pairing offer) is passed to the SDK only and is never logged or returned by any RPC.

- `remoteHostTransport` (prototype.json): `"auto"` (default) connects directly when the hosts.json row has a connection, and otherwise uses the old route through the local daemon. `"direct"` refuses hosts without a saved connection. `"routed"` brings back the old behaviour exactly (hosts listed by, and routed through, the local daemon).
- A direct link must report the saved `serverId`, otherwise it is refused and closed. It reconnects with backoff (1–30 s) and keeps the same identity. Losing the local daemon does not touch direct links.
- Status in the host list comes from the sidecar's own live link: `online` while connected, `connecting` while reconnecting, `offline` for 30 s after a failed direct connect. Otherwise it is the daemon's status or the saved hosts.json status.
- Proxy for `wss://` links: `HTTPS_PROXY`/`https_proxy`, then `HTTP_PROXY`/`http_proxy`. `NO_PROXY`/`no_proxy` and loopback are bypassed. `proxy` (prototype.json string, e.g. `"http://127.0.0.1:7897"`) overrides the environment. Apps launched from Finder/Dock usually do not inherit shell proxy variables, so set `proxy` there.
- Local daemon down at launch: the sidecar takes its identity from `serverId` or `<paseoHome>/server-id`. It announces its endpoint after 5 s and keeps reconnecting in the background. It serves the last local directory snapshot (`paseo-directory-snapshot.json` in the isolated directory, written at most every 5 min). Without a snapshot, local `projects.list` fails and the GUI cannot finish starting (Rust requires at least one project). Start the daemon once to create the snapshot.
- The relay Noise crypto is bundled. When the Paseo checkout has no `@noble/*` packages installed, `bundle.mjs` substitutes `noble-shim.mjs` (node:crypto). The shim is checked against RFC 7748/8439/4231 vectors in `noble-shim.test.mjs`.

No daemon ownership/lifecycle, remote PTY, agent deletion, remote plugin panels, labels or historical import. Conversation history/execution remain authoritative in Paseo. Closing the client releases only client resources. Model actions/pending sends retain exact agent identity and durable no-replay protections.
