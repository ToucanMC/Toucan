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
- Loading and saving supported Anvil region data
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

If the selected configuration file does not exist, Toucan creates it with the
default settings. Server address, message of the day, game mode, world seed,
view distance, resource limits, autosave interval, and logging can be changed
in the configuration file.

## Worlds

Toucan creates the configured world folder and generates missing chunks as
players explore. The `terrain` generator creates simple rolling terrain, while
`flat` creates a flat stone world.

Supported chunk and player changes are saved automatically and during a clean
shutdown. On Linux, a manual save can be requested with `SIGUSR1`.

Toucan only understands part of the vanilla world format. Custom states,
states from another data version, and malformed properties cause an explicit
error instead of being silently replaced, but unsupported data in a changed
chunk may not be preserved. Always keep a backup.

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
