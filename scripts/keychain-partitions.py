#!/usr/bin/env python3
"""Read one labeled item's partition ACL from a security dump-keychain -a text file."""

import pathlib
import plistlib
import re
import sys


def decode_blob(value):
    value = value.strip().strip('"')
    hex_value = value.removeprefix("0x")
    if re.fullmatch(r"[0-9a-fA-F]+", hex_value) and len(hex_value) % 2 == 0:
        return bytes.fromhex(hex_value).decode("utf-8", errors="replace").rstrip("\x00")
    return value


def partitions(description):
    value = decode_blob(description)
    if value.lstrip().startswith("<?xml") or value.lstrip().startswith("<plist"):
        return ",".join(plistlib.loads(value.encode("utf-8"))["Partitions"])
    return ",".join(part.strip() for part in value.split(","))


def item_blocks(dump):
    # A class follows its keychain header; a second header starts a new item.
    blocks = []
    current = []
    has_class = False
    for line in dump.splitlines(keepends=True):
        if has_class and (line.startswith("keychain:") or line.startswith("class:")):
            blocks.append("".join(current))
            current = []
            has_class = False
        current.append(line)
        has_class |= line.startswith("class:")
    if current:
        blocks.append("".join(current))
    return blocks


def partition_for_label(label, dump):
    matching_blocks = []
    matching_descriptions = []
    for block in item_blocks(dump):
        labels = re.findall(r'^\s*(?:"labl"|"Label"|0x00000001)\s*<blob>=(.+)$', block, re.M)
        if label not in (decode_blob(value) for value in labels):
            continue
        matching_blocks.append(block)
        for entry in re.split(r"(?=^\s*entry \d+:)", block, flags=re.M):
            if not re.search(r"^\s*authorizations \(\d+\):.*\bpartition_id\b", entry, re.M):
                continue
            description = re.search(r"^\s*description:\s*(.+)$", entry, re.M)
            if description:
                matching_descriptions.append(description.group(1))
    if len(matching_descriptions) != 1:
        print(f"expected one partition list for {label}, found {len(matching_descriptions)}", file=sys.stderr)
        for block in matching_blocks:
            print("matching dump block:\n" + block, file=sys.stderr)
            for line in block.splitlines():
                if "description:" in line or "partition_id" in line:
                    print("raw ACL: " + line, file=sys.stderr)
        if not matching_blocks:
            print("no matching label; dump item labels:", file=sys.stderr)
            for block in item_blocks(dump):
                for line in block.splitlines():
                    if '"labl"' in line or '"Label"' in line or "0x00000001" in line:
                        print(line, file=sys.stderr)
        raise ValueError("partition ACL count mismatch")
    return partitions(matching_descriptions[0])


if __name__ == "__main__":
    try:
        print(partition_for_label(sys.argv[1], pathlib.Path(sys.argv[2]).read_text()))
    except (IndexError, OSError, ValueError, KeyError, plistlib.InvalidFileException) as exc:
        print(f"keychain partition parse failed: {exc}", file=sys.stderr)
        sys.exit(1)
