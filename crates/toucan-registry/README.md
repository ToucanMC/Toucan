# Toucan registry fixtures

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
