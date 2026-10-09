#!/usr/bin/env python3
"""Validate release contents without extracting archives or executing installers.

Signature and source-provenance verification are separate mandatory CI steps.
Native metadata tools: exiftool, msiinfo, dpkg-deb and rpm (Ubuntu verifier).
"""

import argparse
import hashlib
import io
import json
import plistlib
import posixpath
import re
import stat
import shutil
import struct
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path, PurePosixPath

PLATFORMS = {
    "macos_arm64": ("dmg",),
    "macos_x86_64": ("dmg",),
    "windows_x86_64": ("exe", "msi"),
    "windows_arm64": ("exe", "msi"),
    "linux_x86_64": ("deb", "rpm", "AppImage"),
    "linux_aarch64": ("deb", "rpm", "AppImage"),
}
MAX_UNCOMPRESSED = 2 * 1024**3


def require(condition, message):
    if not condition:
        raise ValueError(message)


def archive_member(path, suffix):
    tool = shutil.which("7zz") or shutil.which("7z")
    require(tool, "7-Zip is required to inspect installer contents")
    listing = subprocess.check_output([tool, "l", "-slt", str(path)], text=True, timeout=60)
    members = [line.removeprefix("Path = ") for line in listing.splitlines() if line.startswith("Path = ") and line.endswith(suffix)]
    require(len(members) == 1, "missing or ambiguous installer archive member")
    return subprocess.check_output([tool, "x", "-so", str(path), members[0]], timeout=60)


def normalize_bundle_markers(section):
    # Tauri patches this fixed-width marker per package format, as documented in
    # tauri-utils::platform::bundle_type. Do not normalize application versions.
    return re.sub(rb"__TAURI_BUNDLE_TYPE_VAR_(?:DEB|RPM|APP)", b"__TAURI_BUNDLE_TYPE_VAR_UNK", section)


def appimage_executable(path):
    # Follow AppImage/type2-runtime's read_elf64 offset calculation without
    # executing the runtime. Ubuntu's 7-Zip lacks some SquashFS compression codecs.
    total = path.stat().st_size
    with path.open("rb") as image:
        header = image.read(64)
        require(len(header) == 64, "truncated AppImage header")
        section_offset = struct.unpack_from("<Q", header, 40)[0]
        size, count = struct.unpack_from("<HH", header, 58)
        table_end = section_offset + size * count
        require(section_offset >= 64 and size >= 64 and count > 0 and table_end <= total, "AppImage runtime section bounds")
        image.seek(section_offset + size * (count - 1))
        last_section = image.read(64)
        offset, length = struct.unpack_from("<QQ", last_section, 24)
        payload_offset = max(table_end, offset + length)
        require(payload_offset + 96 <= total, "AppImage filesystem bounds")
        image.seek(payload_offset)
        require(image.read(4) == b"hsqs", "AppImage SquashFS signature")
    return subprocess.check_output(["unsquashfs", "-cat", "-no-wildcards", "-o", str(payload_offset), str(path), "usr/bin/mesh-talk"], timeout=60)


def elf_fingerprint(binary, platform=None):
    """Match executable code/constants across linuxdeploy's RPATH rewriting."""
    machine = b"\xb7\x00" if platform == "linux_aarch64" else b"\x3e\x00"
    require(len(binary) >= 64 and binary[:6] == b"\x7fELF\x02\x01" and binary[18:20] == machine, "embedded Linux application architecture")
    section_offset = struct.unpack_from("<Q", binary, 40)[0]
    size, count, names_index = struct.unpack_from("<HHH", binary, 58)
    require(size >= 64 and count > 0 and names_index < count and section_offset + size * count <= len(binary), "ELF section bounds")
    def contents(index):
        offset, length = struct.unpack_from("<QQ", binary, section_offset + size * index + 24)
        require(offset + length <= len(binary), "ELF section content bounds")
        return binary[offset:offset + length]
    names = contents(names_index)
    hashes = {}
    for index in range(count):
        name_offset = struct.unpack_from("<I", binary, section_offset + size * index)[0]
        require(name_offset < len(names), "ELF section name bounds")
        name = names[name_offset:].split(b"\0", 1)[0]
        if name in (b".text", b".rodata"):
            require(name not in hashes and contents(index), "duplicate or empty executable section")
            section = contents(index)
            if name == b".rodata":
                section = normalize_bundle_markers(section)
            hashes[name.decode()] = hashlib.sha256(section).hexdigest()
    require(len(hashes) == 2, "missing executable code/constants")
    return hashes


def native_metadata(path, extension, version, platform=None):
    def command(*args):
        return subprocess.check_output(args, text=True, timeout=60).strip()

    if extension == "exe":
        with path.open("rb") as binary:
            require(binary.read(2) == b"MZ", "Windows executable header")
            binary.seek(0x3C)
            offset = int.from_bytes(binary.read(4), "little")
            binary.seek(offset)
            # NSIS commonly uses an x86 bootstrapper to install an x64 app.
            # MSI's Template below provides the application target architecture.
            require(binary.read(6) in (b"PE\0\0\x64\x86", b"PE\0\0\x4c\x01"), "Windows installer bootstrapper architecture")
        records = json.loads(command("exiftool", "-j", "-ProductVersion", "-FileVersion", str(path)))
        require(len(records) == 1, "Windows executable metadata")
        for field in ("ProductVersion", "FileVersion"):
            require(records[0].get(field) in (version, version + ".0"), "Windows executable version")
    elif extension == "msi":
        rows = command("msiinfo", "export", str(path), "Property").splitlines()
        properties = dict(row.split("\t", 1) for row in rows if "\t" in row)
        require(properties.get("ProductVersion") == version, "Windows MSI version")
        summary = command("msiinfo", "suminfo", str(path))
        architecture = "Arm64" if platform == "windows_arm64" else "x64"
        require(re.search(rf"^Template:\s*{architecture};", summary, re.MULTILINE), "Windows MSI architecture")
    elif extension == "deb":
        require(command("dpkg-deb", "-f", str(path), "Version") == version, "Debian version")
        architecture = "arm64" if platform == "linux_aarch64" else "amd64"
        require(command("dpkg-deb", "-f", str(path), "Architecture") == architecture, "Debian architecture")
        payload = subprocess.check_output(["dpkg-deb", "--fsys-tarfile", str(path)], timeout=60)
        with tarfile.open(fileobj=io.BytesIO(payload)) as archive:
            matches = [member for member in archive.getmembers() if member.name.removeprefix("./") == "usr/bin/mesh-talk" and member.isfile()]
            require(len(matches) == 1, "missing Debian executable")
            return {"elf": elf_fingerprint(archive.extractfile(matches[0]).read(), platform)}
    elif extension == "rpm":
        architecture = "aarch64" if platform == "linux_aarch64" else "x86_64"
        require(command("rpm", "-qp", "--qf", "%{VERSION}\n%{ARCH}", str(path)).splitlines() == [version, architecture], "RPM version/architecture")
    elif extension == "AppImage":
        with path.open("rb") as binary:
            header = binary.read(20)
        machine = b"\xb7\x00" if platform == "linux_aarch64" else b"\x3e\x00"
        require(header[:6] == b"\x7fELF\x02\x01" and header[18:20] == machine, "AppImage architecture")
        require(header[8:11] == b"AI\x02", "AppImage Type 2 format")
        # AppImage has no application-version field. Bind its actual executable
        # code/constants to the independently versioned Debian package below.
        return {"elf": elf_fingerprint(appimage_executable(path), platform)}
    elif extension == "dmg":
        require(path.stat().st_size >= 512, "invalid DMG size")
        with path.open("rb") as image:
            image.seek(-512, 2)
            require(image.read(4) == b"koly", "invalid DMG UDIF trailer")
        metadata = plistlib.loads(archive_member(path, "mesh-talk.app/Contents/Info.plist"))
        require(metadata.get("CFBundleShortVersionString") == version and metadata.get("CFBundleExecutable") == "mesh-talk", "DMG embedded application version")
        binary = archive_member(path, "mesh-talk.app/Contents/MacOS/mesh-talk")
        expected_cpu = 0x0100000C if platform == "macos_arm64" else 0x01000007
        require(binary[:8] == b"\xcf\xfa\xed\xfe" + expected_cpu.to_bytes(4, "little"), "DMG embedded application architecture")
        return {"binary": hashlib.sha256(binary).hexdigest()}
    return {}


def verify_archive(path, platform, version, check_installers):
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        names = [entry.filename for entry in members]
        require(len(names) == len(set(names)), "duplicate archive member")
        require(len(members) <= 10000 and sum(entry.file_size for entry in members) <= MAX_UNCOMPRESSED, "archive exceeds size limits")
        files = {}
        for entry in members:
            name = entry.filename
            parts = PurePosixPath(name).parts
            require(name.startswith("release/") and "\\" not in name and "\0" not in name and not any(part in ("..", ".") for part in name.rstrip("/").split("/")) and all(parts), "unsafe archive path")
            mode = entry.external_attr >> 16
            if stat.S_ISLNK(mode):
                require(platform.startswith("macos") and name.startswith("release/mesh-talk.app/"), "installer symlink")
                target = archive.read(entry).decode("utf8")
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
                require(not target.startswith("/") and "\\" not in target and resolved.startswith("release/mesh-talk.app/"), "unsafe app symlink")
                continue
            require(not mode or stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR), "special archive member")
            if not entry.is_dir():
                files[name] = entry
        top = {name.removeprefix("release/"): entry for name, entry in files.items() if name.count("/") == 1}
        require("SHA256SUMS" in top, "missing checksum manifest")
        manifest = archive.read(top.pop("SHA256SUMS")).decode("utf8")
        checksums = {}
        for line in manifest.splitlines():
            match = re.fullmatch(r"([a-fA-F0-9]{64}) [ *](?:\./)?([^/\\]+)", line)
            require(match is not None, "invalid checksum line")
            digest, name = match.groups()
            require(name not in checksums, "duplicate checksum entry")
            checksums[name] = digest.lower()
        require(checksums and set(checksums) == set(top), "incomplete checksum manifest")
        require(len(top) == len(PLATFORMS[platform]), "unexpected installer set")
        installer_metadata = {}
        for extension in PLATFORMS[platform]:
            matches = [name for name in top if name.endswith("." + extension)]
            require(len(matches) == 1, "missing or duplicate installer format")
            name = matches[0]
            require(top[name].file_size > 0, "empty installer")
            digest = hashlib.sha256()
            with archive.open(top[name]) as source:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(block)
            require(digest.hexdigest() == checksums[name], "installer checksum mismatch")
            if check_installers:
                with tempfile.TemporaryDirectory(prefix="mesh-talk-installer-") as temporary:
                    installer = Path(temporary) / ("installer." + extension)
                    with archive.open(top[name]) as source, installer.open("wb") as destination:
                        shutil.copyfileobj(source, destination)
                    installer_metadata[extension] = native_metadata(installer, extension, version, platform)
        if platform.startswith("macos"):
            prefix = "release/mesh-talk.app/Contents/"
            require(prefix + "Info.plist" in files, "missing macOS application metadata")
            metadata = plistlib.loads(archive.read(files[prefix + "Info.plist"]))
            require(metadata.get("CFBundleShortVersionString") == version, "macOS application version")
            require(metadata.get("CFBundleExecutable") == "mesh-talk", "unexpected macOS executable")
            binary_name = prefix + "MacOS/mesh-talk"
            require(binary_name in files, "missing macOS executable")
            with archive.open(files[binary_name]) as binary:
                header = binary.read(8)
            cpu = 0x0100000C if platform.endswith("arm64") else 0x01000007
            require(header == b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little"), "macOS application architecture")
            if check_installers:
                require(installer_metadata["dmg"]["binary"] == hashlib.sha256(archive.read(files[binary_name])).hexdigest(), "DMG/application executable mismatch")
            require(all(name.startswith("release/mesh-talk.app/") or name.count("/") == 1 for name in files), "unexpected app archive content")
        else:
            require(all(name.count("/") == 1 for name in files), "unexpected installer archive content")
            if check_installers and platform.startswith("linux_"):
                require(installer_metadata["AppImage"]["elf"] == installer_metadata["deb"]["elf"], "AppImage/versioned Debian executable mismatch")


def verify_directory(root, tag, version, *, check_installers=True):
    root = Path(root)
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-ci\.[0-9]+)?", tag), "invalid artifact tag")
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version), "invalid version")
    expected = {"mesh-talk.cdx.json"}
    for platform in PLATFORMS:
        expected.update((f"mesh-talk_{tag}_{platform}.zip", f"mesh-talk_{tag}_{platform}.zip.bundle"))
    require({path.name for path in root.iterdir()} == expected, "incomplete or unexpected asset set")
    for name in expected:
        path = root / name
        require(path.is_file() and not path.is_symlink() and path.stat().st_size > 0, "invalid asset file")
    sbom = json.loads((root / "mesh-talk.cdx.json").read_text())
    require(sbom.get("bomFormat") == "CycloneDX" and isinstance(sbom.get("components"), list) and sbom["components"], "invalid or empty SBOM")
    for platform in PLATFORMS:
        archive = root / f"mesh-talk_{tag}_{platform}.zip"
        bundle = json.loads(Path(str(archive) + ".bundle").read_text())
        require(isinstance(bundle, dict) and bundle.get("mediaType"), "invalid signature bundle")
        verify_archive(archive, platform, version, check_installers)
    return list(PLATFORMS)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    for platform in verify_directory(args.directory, args.tag, args.version):
        print(f"{platform}: contents, checksums and installer metadata verified")


if __name__ == "__main__":
    main()
