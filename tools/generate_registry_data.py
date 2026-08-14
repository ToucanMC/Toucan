#!/usr/bin/env python3
"""Generate Toucan's compact vanilla registry artifact from Mojang reports."""

from __future__ import annotations

import argparse
import base64
import gzip
import hashlib
import json
import struct
from pathlib import Path
from typing import Any


MINECRAFT_VERSION = "26.1.2"
PROTOCOL_VERSION = 775
SERVER_URL = (
    "https://piston-data.mojang.com/v1/objects/"
    "97ccd4c0ed3f81bbb7bfacddd1090b0c56f9bc51/server.jar"
)


class PacketReader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.offset = 0

    def read(self, length: int) -> bytes:
        end = self.offset + length
        if length < 0 or end > len(self.data):
            raise ValueError("registry packet ended unexpectedly")
        value = self.data[self.offset:end]
        self.offset = end
        return value

    def u8(self) -> int:
        return self.read(1)[0]

    def i32(self) -> int:
        return struct.unpack(">i", self.read(4))[0]

    def varint(self) -> int:
        value = 0
        for index in range(5):
            byte = self.u8()
            value |= (byte & 0x7F) << (index * 7)
            if byte & 0x80 == 0:
                return value
        raise ValueError("registry packet contains an oversized VarInt")

    def string(self) -> str:
        return self.read(self.varint()).decode("utf-8")

    def nbt_string(self) -> None:
        length = struct.unpack(">H", self.read(2))[0]
        self.read(length)

    def skip_nbt_payload(self, tag_type: int) -> None:
        if tag_type == 0:
            return
        if tag_type == 1:
            self.read(1)
        elif tag_type == 2:
            self.read(2)
        elif tag_type in (3, 5):
            self.read(4)
        elif tag_type in (4, 6):
            self.read(8)
        elif tag_type == 7:
            self.read(self.i32())
        elif tag_type == 8:
            self.nbt_string()
        elif tag_type == 9:
            element_type = self.u8()
            for _ in range(self.i32()):
                self.skip_nbt_payload(element_type)
        elif tag_type == 10:
            while True:
                child_type = self.u8()
                if child_type == 0:
                    break
                self.nbt_string()
                self.skip_nbt_payload(child_type)
        elif tag_type == 11:
            self.read(self.i32() * 4)
        elif tag_type == 12:
            self.read(self.i32() * 8)
        else:
            raise ValueError(f"registry packet contains unknown NBT tag {tag_type}")

    def skip_network_nbt(self) -> None:
        self.skip_nbt_payload(self.u8())


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_biomes(fixture_path: Path) -> list[dict[str, Any]]:
    encoded = "".join(fixture_path.read_text(encoding="ascii").split())
    packets = json.loads(gzip.decompress(base64.b64decode(encoded)))
    for packet in packets:
        if packet["id"] != 7:
            continue
        reader = PacketReader(base64.b64decode(packet["payload"]))
        registry_name = reader.string()
        count = reader.varint()
        entries = []
        for registry_id in range(count):
            name = reader.string()
            if reader.u8() != 0:
                reader.skip_network_nbt()
            entries.append({"id": registry_id, "name": name})
        if reader.offset != len(reader.data):
            raise ValueError(f"trailing bytes in {registry_name} registry packet")
        if registry_name == "minecraft:worldgen/biome":
            return entries
    raise ValueError("configuration fixture has no biome registry packet")


def collision_category(definition_type: str) -> str:
    empty = {
        "minecraft:air",
        "minecraft:end_portal",
        "minecraft:fire",
        "minecraft:light",
        "minecraft:liquid",
        "minecraft:nether_portal",
        "minecraft:soul_fire",
        "minecraft:structure_void",
    }
    full_cube = {
        "minecraft:block",
        "minecraft:concrete_powder",
        "minecraft:drop_experience",
        "minecraft:infested",
        "minecraft:infested_rotated_pillar",
        "minecraft:netherrack",
        "minecraft:rotated_pillar",
        "minecraft:sand",
        "minecraft:weathering_copper_full",
    }
    if definition_type in empty:
        return "empty"
    if definition_type in full_cube:
        return "full_cube"
    return "complex"


def generate(reports: Path, fixture: Path) -> dict[str, Any]:
    blocks_path = reports / "blocks.json"
    registries_path = reports / "registries.json"
    components_path = reports / "minecraft/components/item"
    raw_blocks = json.loads(blocks_path.read_text(encoding="utf-8"))
    raw_registries = json.loads(registries_path.read_text(encoding="utf-8"))
    block_entries = raw_registries["minecraft:block"]["entries"]
    item_entries = raw_registries["minecraft:item"]["entries"]

    blocks = []
    all_state_ids: set[int] = set()
    for name, entry in sorted(block_entries.items(), key=lambda pair: pair[1]["protocol_id"]):
        source = raw_blocks[name]
        property_names = sorted(source.get("properties", {}))
        properties = [
            {"name": property_name, "values": source["properties"][property_name]}
            for property_name in property_names
        ]
        states = []
        default_state = None
        for state in source["states"]:
            state_id = state["id"]
            if state_id in all_state_ids:
                raise ValueError(f"duplicate block state ID {state_id}")
            all_state_ids.add(state_id)
            values = [state.get("properties", {})[key] for key in property_names]
            states.append({"id": state_id, "values": values})
            if state.get("default", False):
                if default_state is not None:
                    raise ValueError(f"multiple default states for {name}")
                default_state = state_id
        if default_state is None:
            raise ValueError(f"missing default state for {name}")
        definition_type = source["definition"]["type"]
        blocks.append(
            {
                "id": entry["protocol_id"],
                "name": name,
                "default_state": default_state,
                "collision": collision_category(definition_type),
                "replaceable": definition_type
                in {
                    "minecraft:air",
                    "minecraft:fire",
                    "minecraft:liquid",
                    "minecraft:soul_fire",
                },
                "properties": properties,
                "states": states,
            }
        )

    if all_state_ids != set(range(len(all_state_ids))):
        raise ValueError("block state IDs are not contiguous from zero")

    items = []
    for name, entry in sorted(item_entries.items(), key=lambda pair: pair[1]["protocol_id"]):
        namespace, path = name.split(":", 1)
        component_file = reports / namespace / "components/item" / f"{path}.json"
        if not component_file.is_file():
            raise ValueError(f"missing default components for {name}")
        components = json.loads(component_file.read_text(encoding="utf-8"))["components"]
        max_stack_size = components.get("minecraft:max_stack_size")
        if not isinstance(max_stack_size, int) or not 1 <= max_stack_size <= 99:
            raise ValueError(f"invalid maximum stack size for {name}")
        items.append(
            {
                "id": entry["protocol_id"],
                "name": name,
                "max_stack_size": max_stack_size,
                "block": block_entries.get(name, {}).get("protocol_id"),
            }
        )

    return {
        "minecraft_version": MINECRAFT_VERSION,
        "protocol_version": PROTOCOL_VERSION,
        "source": {
            "server_url": SERVER_URL,
            "blocks_sha256": sha256(blocks_path),
            "registries_sha256": sha256(registries_path),
            "configuration_fixture_sha256": sha256(fixture),
        },
        "blocks": blocks,
        "items": items,
        "biomes": load_biomes(fixture),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--reports", type=Path, required=True)
    parser.add_argument("--configuration-fixture", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    generated = generate(args.reports, args.configuration_fixture)
    payload = json.dumps(generated, ensure_ascii=True, separators=(",", ":")).encode("utf-8")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("wb") as target:
        with gzip.GzipFile(filename="", mode="wb", fileobj=target, mtime=0) as compressed:
            compressed.write(payload)

    print(
        f"wrote {args.output}: {len(generated['blocks'])} blocks, "
        f"{sum(len(block['states']) for block in generated['blocks'])} states, "
        f"{len(generated['items'])} items, {len(generated['biomes'])} biomes"
    )


if __name__ == "__main__":
    main()
