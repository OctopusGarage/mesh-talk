#!/usr/bin/env python3
"""Recompress native Linux bundles before the manual and updater roles diverge."""

import argparse
import filecmp
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

from verify_assets import appimage_executable, appimage_payload_offset, require


def run(*args, **kwargs):
    return subprocess.run(args, check=True, timeout=300, **kwargs)


def keep_smaller(source, candidate):
    before, after = source.stat().st_size, candidate.stat().st_size
    if after < before:
        candidate.chmod(source.stat().st_mode & 0o777)
        candidate.replace(source)
        print(f"{source.name}: {before} → {after} bytes", flush=True)


def optimize_deb(source, work):
    members = run("ar", "t", source, capture_output=True, text=True).stdout.splitlines()
    if members == ["debian-binary", "control.tar.gz", "data.tar.xz"]:
        return
    require(members == ["debian-binary", "control.tar.gz", "data.tar.gz"],
            "unexpected Debian package members")
    for member in members[:2]:
        with (work / member).open("wb") as output:
            run("ar", "p", source, member, stdout=output)
    data = work / "original.tar"
    with data.open("wb") as output:
        run("dpkg-deb", "--fsys-tarfile", source, stdout=output)
    with (work / "data.tar.xz").open("wb") as output:
        run("xz", "-6", "-T1", "-c", data, stdout=output)
    candidate = work / "candidate.deb"
    run("ar", "rc", candidate, "debian-binary", "control.tar.gz", "data.tar.xz", cwd=work)
    restored = work / "restored.tar"
    with restored.open("wb") as output:
        run("dpkg-deb", "--fsys-tarfile", candidate, stdout=output)
    require(filecmp.cmp(data, restored, shallow=False), "Debian data archive changed")
    require(run("ar", "p", candidate, "control.tar.gz", capture_output=True).stdout
            == (work / "control.tar.gz").read_bytes(), "Debian control archive changed")
    keep_smaller(source, candidate)


def appimage_runtime(source, offset):
    # Commands below preserve the runtime bytes and regenerate detached updater
    # signatures later. Do not invalidate a populated embedded signature.
    with source.open("rb") as stream:
        runtime = stream.read(offset)
    require(runtime[8:11] == b"AI\x02", "expected a type-2 AppImage")
    table = struct.unpack_from("<Q", runtime, 40)[0]
    entry, count, names = struct.unpack_from("<HHH", runtime, 58)
    require(names < count, "invalid AppImage ELF section names index")
    names_offset, length = struct.unpack_from("<QQ", runtime, table + names * entry + 24)
    require(names_offset + length <= len(runtime), "invalid AppImage ELF section names")
    section_names = runtime[names_offset:names_offset + length]
    for index in range(count):
        base = table + index * entry
        name_offset = struct.unpack_from("<I", runtime, base)[0]
        name = section_names[name_offset:].split(b"\0", 1)[0]
        if name in (b".sha256_sig", b".sig_key"):
            start, size = struct.unpack_from("<QQ", runtime, base + 24)
            require(start + size <= len(runtime) and not any(runtime[start:start + size]),
                    "cannot recompress an AppImage with an embedded signature")
    return runtime


def optimize_appimage(source, work):
    offset = appimage_payload_offset(source)
    runtime = appimage_runtime(source, offset)
    original = work / "original.pseudo"
    run("unsquashfs", "-o", str(offset), "-pf", original, source)
    filesystem = work / "candidate.sqfs"
    run("mksquashfs", "-", filesystem, "-pf", original, "-comp", "zstd",
        "-Xcompression-level", "22", "-b", "1M", "-noappend", "-processors", "2", "-no-progress")
    restored = work / "restored.pseudo"
    run("unsquashfs", "-pf", restored, filesystem)
    require(filecmp.cmp(original, restored, shallow=False), "AppImage filesystem changed")
    candidate = work / "candidate.AppImage"
    with candidate.open("wb") as output, filesystem.open("rb") as payload:
        output.write(runtime)
        shutil.copyfileobj(payload, output)
    executable = appimage_executable(source)
    require(appimage_executable(candidate) == executable,
            "AppImage application executable changed")
    if sys.platform == "linux" and candidate.stat().st_size < source.stat().st_size:
        # The release runners match the target architecture. Exercise the
        # bundled runtime itself, without FUSE or launching the application.
        candidate.chmod(0o700)
        run(candidate, "--appimage-extract", "usr/bin/mesh-talk", cwd=work)
        require((work / "squashfs-root/usr/bin/mesh-talk").read_bytes() == executable,
                "AppImage runtime extracted different bytes")
    keep_smaller(source, candidate)


def optimize(bundle):
    for directory, extension, compressor in (
        ("deb", "*.deb", optimize_deb),
        ("appimage", "*.AppImage", optimize_appimage),
    ):
        files = list((bundle / directory).glob(extension))
        require(len(files) == 1 and files[0].is_file() and not files[0].is_symlink(),
                f"expected one native {directory} package")
        with tempfile.TemporaryDirectory(prefix=".compression-", dir=files[0].parent) as work:
            compressor(files[0], Path(work))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    optimize(parser.parse_args().bundle.resolve())
