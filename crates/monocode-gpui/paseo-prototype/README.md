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

No daemon ownership/lifecycle, remote PTY, agent deletion, remote plugin panels, labels or historical import. Conversation history/execution remain authoritative in Paseo. Closing the client releases only client resources. Model actions/pending sends retain exact agent identity and durable no-replay protections.
