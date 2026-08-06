# Toucan

Toucan is an independent Minecraft Java Edition server implementation written
in Rust. It is being built around bounded resource use, explicit protocol state
machines, vanilla-compatible storage, and clean future extension points for the
Beak WebAssembly mod loader.

> [!WARNING]
> Toucan is alpha software. Clients can join a temporary generated flat spawn,
> but changes are not persisted and normal gameplay is still incomplete.

## Current status

Phase 0 through Phase 2 are implemented:

- a stable-Rust workspace with separated configuration, observability,
  protocol, networking, and server crates;
- defensive protocol 775 VarInt, VarLong, string, packed-position, and packet
  frame codecs;
- Handshake and Status connection states, including status response and
  ping/pong;
- configurable listening address, MOTD, player limit, resource bounds, and
  logging;
- graceful Ctrl+C and SIGTERM shutdown;
- unit, fixed-fixture protocol, malformed-input, and headless status integration
  tests;
- offline username authentication and vanilla-compatible offline UUIDs;
- bounded Minecraft compression with threshold and decompression-bomb checks;
- protocol-775 Login and Configuration state machines, explicit disconnects,
  feature and known-pack negotiation, and a checksum-pinned vanilla registry
  and tag stream;
- a headless compressed connection test that reaches Play state.

Phase 3 is underway with bounded, lossless NBT support and a read-only
`level.dat` metadata loader. Toucan sends a bounded 5×5 generated flat chunk
area so a client can finish loading and spawn. Anvil region loading remains
unfinished.

World-folder chunks, authoritative movement state, block interaction, and
persistence are not implemented yet. Online mode fails explicitly at Login; it
never silently falls back to offline identity.

## Build and test

Toucan requires stable Rust 1.85 or newer.

```bash
cargo build --workspace
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features
```

## Run

The checked-in development configuration uses offline mode.

```bash
cargo run -p toucan-server -- --config config/toucan.toml
```

Add `localhost:25565` to a Minecraft 26.1.2 multiplayer server list. Status and
offline Login are implemented, and the client is spawned at `0.5, 65, 0.5` on
a temporary stone platform.

## Providing a world

World loading is the Phase 3 milestone. The configured `server.world` path is
validated, but the server coordinator does not open or modify it yet. Do not point
this alpha at an irreplaceable world. When storage support lands, use a backup
of a vanilla-compatible world folder until compatibility tests are complete.

## Project goals

- predictable latency and production reliability;
- eventually complete vanilla gameplay parity;
- independently implemented, versioned protocol support;
- vanilla-compatible world folders;
- authoritative, tick-based simulation without global mutable state;
- future sandboxed extensions through Beak and the WebAssembly Component Model.

See [architecture](docs/architecture.md), [protocol coverage](docs/protocol.md),
and [milestones](docs/milestones.md) for current design and scope.
