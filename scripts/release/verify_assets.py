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
    members = []
    for block in re.split(r"\r?\n\s*\r?\n", listing):
        fields = dict(line.split(" = ", 1) for line in block.splitlines() if " = " in line)
        name = fields.get("Path", "")
        if name.endswith(suffix) and fields.get("Folder") == "-":
            size = fields.get("Size", "")
            require(size.isdecimal(), "invalid installer archive member size")
            members.append((name, int(size)))
    require(len(members) == 1, "missing or ambiguous installer archive member")
    name, size = members[0]
    data = subprocess.check_output([tool, "x", "-so", str(path), name], timeout=60)
    require(len(data) >= size, "truncated installer archive member")
    # macOS 7zz can append bytes after the listed HFS file length on stdout.
    return data[:size]


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
        return {"macho": hashlib.sha256(binary).hexdigest()}
    return {}


def mac_updater_metadata(source, version, platform):
    found = {}
    total = 0
    with tarfile.open(fileobj=source, mode="r|gz") as archive:
        for count, member in enumerate(archive):
            total += member.size
            require(count < 10000 and total <= MAX_UNCOMPRESSED, "updater archive exceeds size limits")
            name = member.name.removeprefix("./")
            require(name.startswith("mesh-talk.app/") or name == "mesh-talk.app", "unexpected updater app root")
            require(".." not in PurePosixPath(name).parts and "\\" not in name, "unsafe updater archive path")
            if name in ("mesh-talk.app/Contents/Info.plist", "mesh-talk.app/Contents/MacOS/mesh-talk"):
                require(member.isfile() and name not in found and 0 < member.size <= 512 * 1024**2, "invalid updater executable or metadata")
                found[name] = archive.extractfile(member).read()
                if name.endswith("/mesh-talk"):
                    require(member.mode & 0o111, "updater binary is not executable")
    require(len(found) == 2, "missing updater executable or metadata")
    metadata = plistlib.loads(found["mesh-talk.app/Contents/Info.plist"])
    require(metadata.get("CFBundleShortVersionString") == version and metadata.get("CFBundleExecutable") == "mesh-talk", "updater application version")
    binary = found["mesh-talk.app/Contents/MacOS/mesh-talk"]
    cpu = 0x0100000C if platform == "macos_arm64" else 0x01000007
    require(binary[:8] == b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little"), "updater application architecture")
    return {"macho": hashlib.sha256(binary).hexdigest()}


def updater_name(tag, platform):
    extension = "app.tar.gz" if platform.startswith("macos_") else "nsis.exe" if platform.startswith("windows_") else "AppImage"
    return f"mesh-talk-update_{tag}_{platform}.{extension}"


def verify_archive(path, platform, version, check_installers, updater=None):
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
            require(not stat.S_ISLNK(mode), "installer symlink")
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
        updater_files = {updater, updater + ".sig"} if updater else set()
        require(len(top) == len(PLATFORMS[platform]) + len(updater_files) and updater_files <= set(top), "unexpected installer set")
        for name in updater_files:
            require(top[name].file_size > 0, "empty updater payload")
            if name.endswith(".sig"):
                require(top[name].file_size <= 16 * 1024, "updater signature exceeds size limit")
            embedded_hash = hashlib.sha256()
            with archive.open(top[name]) as source:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    embedded_hash.update(block)
            require(embedded_hash.hexdigest() == checksums[name], "updater checksum mismatch")
            standalone = path.parent / name
            standalone_hash = hashlib.sha256()
            with standalone.open("rb") as source:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    standalone_hash.update(block)
            require(standalone.stat().st_size == top[name].file_size and standalone_hash.hexdigest() == checksums[name], "standalone updater must match verified archive")
        installers = {name: entry for name, entry in top.items() if name not in updater_files}
        installer_metadata = {}
        for extension in PLATFORMS[platform]:
            matches = [name for name in installers if name.endswith("." + extension)]
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
        if updater and platform.startswith("macos_") and check_installers:
            with archive.open(top[updater]) as source:
                metadata = mac_updater_metadata(source, version, platform)
            require(metadata == installer_metadata["dmg"], "updater/versioned DMG executable mismatch")
        if updater and not platform.startswith("macos_"):
            extension = "exe" if platform.startswith("windows_") else "AppImage"
            original = next(name for name in installers if name.endswith("." + extension))
            require(checksums[updater] == checksums[original], "updater must match native installer")
        require(all(name.count("/") == 1 for name in files), "unexpected installer archive content")
        if check_installers and platform.startswith("linux_"):
            require(installer_metadata["AppImage"]["elf"] == installer_metadata["deb"]["elf"], "AppImage/versioned Debian executable mismatch")


def verify_post_office_binary(binary, platform):
    if platform.startswith("macos_"):
        cpu = 0x0100000C if platform == "macos_arm64" else 0x01000007
        require(binary[:8] == b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little"), "post-office macOS architecture")
    elif platform.startswith("linux_"):
        machine = b"\xb7\x00" if platform == "linux_aarch64" else b"\x3e\x00"
        require(binary[:6] == b"\x7fELF\x02\x01" and binary[18:20] == machine, "post-office Linux architecture")
    else:
        require(len(binary) >= 64 and binary[:2] == b"MZ", "post-office Windows executable")
        offset = int.from_bytes(binary[0x3C:0x40], "little")
        machine = b"\x64\x86" if platform == "windows_x86_64" else b"\x64\xaa"
        require(binary[offset:offset + 6] == b"PE\0\0" + machine, "post-office Windows architecture")


def verify_post_office_archive(path, platform, check_binaries):
    binary_name = "mesh-talk-node.exe" if platform.startswith("windows_") else "mesh-talk-node"
    expected = {binary_name, "README.md"}
    if platform.startswith("windows_"):
        expected.add("install-post-office-windows.ps1")
    with zipfile.ZipFile(path) as archive:
        members = archive.infolist()
        names = [entry.filename for entry in members]
        require(len(names) == len(set(names)), "duplicate post-office archive member")
        require(len(members) <= 5 and sum(entry.file_size for entry in members) <= MAX_UNCOMPRESSED, "post-office archive exceeds size limits")
        files = {}
        for entry in members:
            name = entry.filename
            require(name.startswith("release/") and "\\" not in name and "\0" not in name and ".." not in PurePosixPath(name).parts, "unsafe post-office archive path")
            mode = entry.external_attr >> 16
            require(not stat.S_ISLNK(mode) and (not mode or stat.S_IFMT(mode) in (0, stat.S_IFREG, stat.S_IFDIR)), "unsafe post-office archive member")
            if not entry.is_dir():
                require(name.count("/") == 1, "unexpected post-office archive contents")
                files[name.removeprefix("release/")] = entry
        require(set(files) == expected | {"SHA256SUMS"}, "unexpected post-office archive contents")
        manifest = archive.read(files["SHA256SUMS"]).decode("utf8")
        checksums = {}
        for line in manifest.splitlines():
            match = re.fullmatch(r"([a-fA-F0-9]{64}) [ *](?:\./)?([^/\\]+)", line)
            require(match is not None, "invalid post-office checksum manifest")
            digest, name = match.groups()
            require(name not in checksums, "duplicate post-office checksum")
            checksums[name] = digest.lower()
        require(set(checksums) == expected, "incomplete post-office checksum manifest")
        for name in expected:
            entry = files[name]
            require(entry.file_size > 0, "empty post-office archive contents")
            data = archive.read(entry)
            require(hashlib.sha256(data).hexdigest() == checksums[name], "post-office checksum mismatch")
            if name == binary_name and check_binaries:
                require(len(data) >= 1024, "post-office binary too small")
                verify_post_office_binary(data, platform)
                if not platform.startswith("windows_"):
                    require(entry.external_attr >> 16 & 0o111, "post-office binary is not executable")


def verify_directory(root, tag, version, *, check_installers=True):
    root = Path(root)
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-ci\.[0-9]+|-nightly\.[1-9][0-9]*\.[1-9][0-9]*)?", tag), "invalid artifact tag")
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version), "invalid version")
    expected = {"mesh-talk.cdx.json"}
    with_updater = any(path.name.startswith("mesh-talk-update_") for path in root.iterdir())
    for platform in PLATFORMS:
        name = f"mesh-talk_{tag}_{platform}.zip"
        expected.update((name, name + ".bundle"))
        if with_updater:
            update = updater_name(tag, platform)
            expected.update((update, update + ".sig"))
        relay_name = f"mesh-talk-post-office_{tag}_{platform}.zip"
        expected.update((relay_name, relay_name + ".bundle"))
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
        verify_archive(archive, platform, version, check_installers, updater_name(tag, platform) if with_updater else None)
        relay_archive = root / f"mesh-talk-post-office_{tag}_{platform}.zip"
        relay_bundle = json.loads(Path(str(relay_archive) + ".bundle").read_text())
        require(isinstance(relay_bundle, dict) and relay_bundle.get("mediaType"), "invalid post-office signature bundle")
        verify_post_office_archive(relay_archive, platform, check_installers)
    return [f"{platform}:desktop" for platform in PLATFORMS] + [f"{platform}:post-office" for platform in PLATFORMS]


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
