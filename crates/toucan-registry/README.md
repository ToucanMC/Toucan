# Toucan vanilla registry data

Toucan bundles two reviewed Minecraft 26.1.2 / protocol 775 data artifacts.

## Vanilla block, item, and biome data

`data/26.1.2/vanilla.json.gz` is generated from Mojang's official 26.1.2
server data reports. It contains compact definitions for 1,168 blocks, 29,873
block states, 1,506 items, and 65 configured biomes. Runtime startup inflates
and validates this file once; gameplay then uses immutable vector-backed IDs.

Sources:

- `reports/blocks.json` provides every block, property combination, default
  state, and global block-state ID.
- `reports/registries.json` provides block and item protocol IDs.
- `reports/minecraft/components/item/*.json` provides maximum stack sizes.
- the reviewed Configuration capture below provides the configured biome order.

The source server is Mojang's official 26.1.2 server artifact:

```text
https://piston-data.mojang.com/v1/objects/97ccd4c0ed3f81bbb7bfacddd1090b0c56f9bc51/server.jar
```

Generate Mojang's reports in a temporary directory, then refresh the compact
artifact from the repository root:

```bash
java -DbundlerMainClass=net.minecraft.data.Main \
  -jar /tmp/minecraft-26.1.2-server.jar \
  --reports --output /tmp/minecraft-26.1.2-reports

python3 tools/generate_registry_data.py \
  --reports /tmp/minecraft-26.1.2-reports/reports \
  --configuration-fixture crates/toucan-registry/src/configuration_26_1_2.json.gz.b64 \
  --output crates/toucan-registry/data/26.1.2/vanilla.json.gz
```

The generator emits deterministic gzip output and rejects duplicate, missing,
non-contiguous, or inconsistent registry data. Review count and ID changes and
run all registry, world, protocol, and network tests before accepting an
updated artifact. A future Minecraft version must use a separate version
directory rather than replacing data needed by an existing adapter.

## Configuration fixture

`src/configuration_26_1_2.json.gz.b64` is a text-safe encoding of a reviewed
vanilla Minecraft Java Edition 26.1.2 configuration capture. The decoded gzip
contains JSON packet records, not Mojang executable code.

- protocol: 775
- world data version: 4790
- decoded JSON SHA-256:
  `fdd36af0e682d702577e8ac183c86ddf6970edd4648e861d4306bddeff206824`
- packet shape: 28 Registry Data (`0x07`) packets followed by one Update Tags
  (`0x0d`) packet

The loader validates the decompressed size, checksum, count, packet IDs, and
base64 payloads before the data can reach a client. Refresh this fixture only
from an intentional vanilla 26.1.2 capture and update both the checksum and
protocol documentation in the same change.

## API boundary

This crate is internal, versioned vanilla infrastructure. Its compact IDs and
immutable definitions are designed for world and protocol hot paths. They are
not the Beak extension registry API and do not provide custom registration,
stable plugin handles, or protocol ID allocation.
