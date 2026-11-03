#!/usr/bin/env python3
"""Bounded native payload inspection shared by delta production and acceptance."""
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile

from sevenzip import sevenzip_tool

MAX_TARGET = 256 * 1024**2
MAX_BASE = 128 * 1024**2


def require(value, message):
    if not value:
        raise ValueError(message)


def mac_tar(path):
    with gzip.open(path, "rb") as source:
        data = source.read(MAX_TARGET + 1)
    require(len(data) <= MAX_TARGET, "macOS updater tar exceeds limit")
    return data


def nsis_files(path):
    tool = sevenzip_tool()
    require(tool, "7-Zip required for NSIS equivalence")
    listing = subprocess.check_output([tool, "l", "-slt", str(path)], text=True, timeout=60)
    require(len(listing) <= 4 * 1024**2 and "Type = Nsis" in listing, "invalid NSIS listing")
    entries = [dict(line.split(" = ", 1) for line in block.splitlines() if " = " in line)
               for block in re.split(r"\r?\n\s*\r?\n", listing)]
    headers = [entry for entry in entries if entry.get("Type") == "Nsis"]
    require(len(headers) == 1, "invalid NSIS listing header")
    header = headers[0]
    require("BadCmd=" not in header.get("SubType", ""),
            "NSIS decoder could not inspect the complete installer; use the pinned release 7-Zip toolchain")
    files = {}
    total = 0
    for fields in entries:
        name = fields.get("Path", "").replace("\\", "/")
        if not name or "Type" in fields or fields.get("Folder") == "+":
            continue
        require(name not in files and not name.startswith("/") and all(p not in ("", ".", "..") for p in name.split("/")), "unsafe or duplicate NSIS member")
        # NSIS generates this installer-specific program, whose bytes differ
        # with compression mode. Every other member, including plugins, is bound.
        if name == "uninstall.exe":
            continue
        size = fields.get("Size", "")
        require(size.isdecimal(), "unknown NSIS member size")
        size = int(size)
        total += size
        require(len(files) < 10000 and total <= MAX_TARGET and size <= MAX_BASE, "NSIS members exceed limits")
        data = subprocess.check_output([tool, "x", "-so", str(path), name], timeout=60)
        # NSIS/7-Zip can report a plugin's pre-stripping size (StartMenu.dll
        # in real published packages). Bind the actual bytes of those installer
        # plugins on both sides; application members retain exact size checks.
        require(0 < len(data) <= size if name.startswith("$PLUGINSDIR/") else len(data) == size, "NSIS member size mismatch")
        files[name] = data
    require("mesh-talk.exe" in files, "missing NSIS application")
    require(b"__TAURI_BUNDLE_TYPE_VAR_NSS" in files["mesh-talk.exe"] and b"__TAURI_BUNDLE_TYPE_VAR_MSI" not in files["mesh-talk.exe"], "NSIS application bundle marker")
    return files


def extract_base(path, os_name):
    if os_name == "windows":
        return nsis_files(path)["mesh-talk.exe"]
    data = mac_tar(path)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        members = [m for m in archive if m.name.removeprefix("./") == "mesh-talk.app/Contents/MacOS/mesh-talk"]
        require(len(members) == 1 and members[0].isfile() and 0 < members[0].size <= MAX_BASE, "invalid macOS base executable")
        return archive.extractfile(members[0]).read()


def equivalent(canonical, target, os_name):
    if os_name == "macos":
        require(mac_tar(canonical) == mac_tar(target), "raw macOS target must preserve exact canonical tar bytes")
    else:
        require(nsis_files(canonical) == nsis_files(target), "raw NSIS application files differ from canonical")


def main():
    command, *args = sys.argv[1:]
    if command == "extract-base":
        source, os_name, output = args
        Path(output).write_bytes(extract_base(Path(source), os_name))
    elif command == "mac-target":
        source, output = args
        Path(output).write_bytes(gzip.compress(mac_tar(Path(source)), compresslevel=0, mtime=0))
    elif command == "equivalent":
        canonical, target, os_name = args
        equivalent(Path(canonical), Path(target), os_name)
    else:
        raise ValueError("unknown delta asset command")


if __name__ == "__main__":
    main()
