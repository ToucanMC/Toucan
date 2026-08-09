# Toucan

Toucan is an independent Minecraft Java Edition server implementation written
in Rust. It is being built around bounded resource use, explicit protocol state
machines, vanilla-compatible storage, and clean future extension points for the
Beak WebAssembly mod loader.

> [!WARNING]
> Toucan is alpha software. Clients can join generated terrain and use basic
> block interactions with restart persistence, but full vanilla gameplay and
> arbitrary vanilla-world compatibility remain incomplete.

## Current status

Phase 0 through Phase 3, the movement slice of Phase 4, and the first
persistence slice of Phases 5 and 6 are implemented:

- a stable-Rust workspace with separated configuration, observability,
  protocol, networking, and server crates;
- defensive protocol 775 VarInt, VarLong, string, packed-position, and packet
  frame codecs;
- Handshake and Status connection states, including status response and
  ping/pong;
- automatic first-run config creation plus configurable listening address,
  MOTD, game mode, terrain seed, worker pools, resource bounds, and logging;
- graceful Ctrl+C and SIGTERM shutdown;
- unit, fixed-fixture protocol, malformed-input, and headless status integration
  tests;
- offline username authentication and vanilla-compatible offline UUIDs;
- bounded Minecraft compression with threshold and decompression-bomb checks;
- protocol-775 Login and Configuration state machines, explicit disconnects,
  feature and known-pack negotiation, and a checksum-pinned vanilla registry
  and tag stream;
- a headless compressed connection test that reaches Play state;
- seeded rolling terrain and optional flat generation with stable internal
  block-state handles;
- a bounded shared chunk cache and protocol-775 palette/heightmap serialization;
- flow-controlled chunk batches, per-player moving view subscriptions, and
  chunk unloads;
- Survival, Creative, Adventure, and Spectator login state, with block mutation
  limited to Survival and Creative;
- basic breaking and creative placement for common terrain and building blocks;
- server-authoritative shared block mutation with updates broadcast to every
  player subscribed to the affected chunk;
- automatic creation and reopening of a vanilla-shaped world folder and bounded
  gzip `level.dat`;
- bounded Anvil region header/sector parsing with gzip, zlib, and uncompressed
  chunk reads;
- dirty chunk tracking, crash-safe compact region rewrites that preserve
  unrelated chunks, batch each region once per pass, configurable autosave, and
  shutdown flush;
- bounded vanilla-shaped `playerdata/<uuid>.dat` loading and atomic saves for
  position, rotation, game mode, and selected hotbar slot, including periodic
  online snapshots;
- a bounded operator control channel, `SIGUSR1` manual save command, and 20 Hz
  server tick health counters;
- a headless restart-persistence test covering a protocol-side block break and
  player movement.

Toucan loads supported chunks from region files, generates missing chunks on
demand, and streams the configured moving view in client-acknowledged batches.
Existing `level.dat` and playerdata fields that Toucan does not interpret are
retained. Online mode fails explicitly at Login; it never silently falls back
to offline identity.

Like every Minecraft dedicated server, Toucan is headless: the client performs
graphical rendering. Terrain generation, block state, interaction validation,
game mode, reach checks, and shared mutations are owned by the server; clients
receive packets describing that authoritative state.

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

If the selected config file does not exist, Toucan creates a complete default
file and immediately loads it. `performance.worker_threads = 0` lets Tokio use
its CPU-count default; a positive value fixes the asynchronous worker count.
`performance.chunk_io_threads` bounds the blocking generation/I/O pool.

Add `localhost:25565` to a Minecraft 26.1.2 multiplayer server list. Status and
offline Login are implemented, and the client is spawned on the generated
surface.

## Providing a world

On first startup, Toucan creates the configured `server.world` folder, its
standard subdirectories, and a gzip-compressed `level.dat`. Missing chunks are
generated using `server.world_generator` (`terrain` or `flat`) and
`server.world_seed`. The shared chunk cache is bounded by
`performance.max_loaded_chunks`. Changed chunks are written at
`performance.autosave_interval_seconds` and during graceful shutdown.

On Linux, request a complete save without stopping the server:

```bash
systemctl --user kill -s USR1 toucan-dev.service
```

The request is routed through the bounded server control queue and logs saved
chunk/player counts when complete.

If `level.dat` already exists, Toucan loads its name, spawn, data version, and
seed while retaining unknown NBT fields in memory. Region files are read with
strict bounds, but only Toucan's currently supported block-state identifiers
can be decoded. Unknown blocks fail explicitly instead of being replaced. Do
not point this alpha at an irreplaceable world; block entities, entities, POI,
dimensions, and complete vanilla palettes are not supported yet.

## Project goals

- predictable latency and production reliability;
- eventually complete vanilla gameplay parity;
- independently implemented, versioned protocol support;
- vanilla-compatible world folders;
- authoritative, tick-based simulation without global mutable state;
- future sandboxed extensions through Beak and the WebAssembly Component Model.

See [architecture and crate guide](docs/architecture.md) and
[world-format status](docs/world-format.md) for current design and scope. The
[milestone ledger](docs/milestones.md) records what remains.
