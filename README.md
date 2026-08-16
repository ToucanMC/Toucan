# Toucan

Toucan is an experimental Minecraft Java Edition server written in Rust. It is
the main project of [ToucanMC](https://github.com/ToucanMC) and is developed as
open-source software.

Toucan currently targets Minecraft Java Edition 26.1.2, protocol 775.

> [!WARNING]
> Toucan is alpha software. It does not provide complete vanilla gameplay and
> should not be used with an important world or relied on for a public server.

## What works

- Server-list status and ping
- Offline-mode login
- Login, Configuration, and Play connection states
- Packet compression with defensive size limits
- Generated terrain and flat worlds
- Loading and lossless patching of supported Anvil chunk fields
- Moving chunk views and chunk unloading
- Basic player movement
- Survival, Creative, Adventure, and Spectator player state
- Basic block breaking and Creative-mode placement
- Data-driven representation of every vanilla 26.1.2 block state
- Basic water and lava flow, buckets, waterlogging, and persisted fluid ticks
- Persistent scheduled block updates, fluid mixing, and basic falling blocks
- Automatic stair, fence, and wall neighbor-state updates
- Shared block updates between nearby players
- Registry-validated 36-slot player inventory with basic pickup clicks
- Player inventory, position, rotation, game mode, and selected-slot persistence
- Automatic saves and clean shutdown handling

Online-mode authentication is not implemented. When online mode is enabled,
Toucan rejects the login instead of accepting an offline identity.

World support is also incomplete. Toucan can represent every vanilla 26.1.2
block state, but it does not implement every block's behavior or placement
rules and does not yet fully support entities, block entities, crafting,
equipment, external containers, dimensions, or arbitrary existing vanilla worlds.
Fluid flow uses vanilla water and overworld lava update rates and prefers nearby
downhill paths, but detailed collision-shape flow, offhand bucket use, and
sneak-aware block interaction precedence are not implemented yet. Falling
blocks currently move in block steps instead of using animated falling-block
entities.

## Requirements

- Rust 1.85 or newer
- A Minecraft Java Edition 26.1.2 client for testing connections

## Build

```bash
cargo build --workspace
```

To run the same checks used by continuous integration:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## Run

The included development configuration uses offline mode:

```bash
cargo run -p toucan-server -- --config config/toucan.toml
```

Toucan listens on `0.0.0.0:25565` by default. Add `localhost:25565` to the
multiplayer server list in Minecraft 26.1.2.

Toucan uses two configuration files. `config/toucan.toml` contains normal
server, gameplay, world, and logging settings and is the file most server owners
should edit. `config/advanced.toml` contains packet limits, runtime sizing,
chunk-cache capacity, scheduled-update budget, and broadcast capacity. If either
file is missing, Toucan creates it from a concise commented template and validates
both files before startup.

`--config PATH` selects the normal configuration file; the advanced file is
named `advanced.toml` in the same directory. Unknown settings are rejected so a
misspelling cannot silently use a default.

### Configuration migration

The previous single-file format is still recognized when it contains a
`[performance]` table. Toucan maps all known old values in memory and prints a
migration warning; it does not overwrite the old file or silently create an
advanced file beside it. Move owner-facing values into the new `[server]`,
`[gameplay]`, `[world]`, and `[logging]` tables, then move tuning values into the
new advanced file. Important renames include:

- `server.address` to `server.bind_address`;
- `server.default_gamemode` to `gameplay.default_game_mode`;
- `server.view_distance` to `gameplay.view_distance_chunks`;
- `performance.max_loaded_chunks` to `world.chunk_cache_max_chunks` in
  `advanced.toml`;
- `performance.max_packets_per_tick` to
  `updates.scheduled_updates_per_tick` (its actual purpose); and
- `network.max_packet_size` to `network.max_uncompressed_packet_bytes`.

## Worlds

Toucan creates the configured world folder and generates missing chunks as
players explore. The `terrain` generator creates simple rolling terrain, while
`flat` creates a flat stone world.

The default world path is `world`, relative to the directory from which the
server is started. Player data is stored in `<world>/playerdata` and overworld
chunks are stored in `<world>/region`.

Supported chunk and player changes are saved automatically and during a clean
shutdown. On Linux, a manual save can be requested with `SIGUSR1`.

Toucan only interprets part of the vanilla world format. It retains the original
raw chunk document and patches only fields it owns, so unrelated data such as
block entities, biome payloads, structures, lighting, and unknown version-specific
tags survive supported block and scheduled-tick changes. Invalid block palettes,
custom states, states from another data version, and malformed properties still
produce typed errors instead of silent substitution. Preservation is not gameplay
support for those systems, and important worlds should still be backed up.

Loaded chunks are retained by explicit lifecycle tickets. Player views, pending
scheduled ticks, unsaved changes, active saves, and temporary users prevent
eviction. Chunks with no remaining reason to stay loaded are evicted in
least-recently-used order. Scheduled updates therefore remain resident through a
save and cannot be dropped merely because a chunk became clean.

## Documentation

The [ToucanMC Documentation repository](https://github.com/ToucanMC/Documentation)
contains short guides covering:

- [architecture](https://github.com/ToucanMC/Documentation/blob/main/architecture.md);
- [vanilla registries](https://github.com/ToucanMC/Documentation/blob/main/registries.md);
- [protocol support](https://github.com/ToucanMC/Documentation/blob/main/protocol.md);
- [world-format support](https://github.com/ToucanMC/Documentation/blob/main/world-format.md);
  and
- [current project status](https://github.com/ToucanMC/Documentation/blob/main/milestones.md).

## Contributing

Contributions and focused bug reports are welcome. Read the
[ToucanMC contribution guidelines](https://github.com/ToucanMC/.github/blob/main/CONTRIBUTING.md)
before opening a pull request.

## Security

Do not report suspected vulnerabilities in a public issue. Follow the
[ToucanMC security policy](https://github.com/ToucanMC/.github/blob/main/SECURITY.md)
and use GitHub's private vulnerability reporting for this repository when it is
available.

## License

Toucan is available under the [MIT License](LICENSE).

Toucan and ToucanMC are independent open-source projects and are not affiliated
with Mojang Studios or Microsoft.
