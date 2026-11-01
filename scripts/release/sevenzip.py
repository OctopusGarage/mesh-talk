#!/usr/bin/env python3
"""Pinned release-only 7-Zip bootstrap; never modifies the global tool install."""
import argparse
import hashlib
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

VERSION = '26.05'
MAX_DOWNLOAD = 16 * 1024 * 1024
TIMEOUT = 60
# Official ip7z/7zip release assets and their release SHA256 digests.
ASSETS = {
    ('Linux', 'x64'): ('linux-x64.tar.xz', '22cf82fcc6eb7474f1d5bb4f02c0f8ebd573a718089d6833dc30f8350a3b232e'),
    ('Linux', 'arm64'): ('linux-arm64.tar.xz', '0b71f6cfc34669b3bf69f2347efd3429f77020665003a13b9cdc53b784270421'),
    ('Darwin', 'x64'): ('mac.tar.xz', '12ef6830317743cfd3f0ab1724bde43fcc270d8b6d8564f8c12c2a2b7f993570'),
    ('Darwin', 'arm64'): ('mac.tar.xz', '12ef6830317743cfd3f0ab1724bde43fcc270d8b6d8564f8c12c2a2b7f993570'),
    ('Windows', 'x64'): ('x64.exe', 'fd99e86f57d8f429ef5ed045bf98315af962e2309b0f1ba87a147a44c71647d0'),
    ('Windows', 'arm64'): ('arm64.exe', '86d5790d5dd84cab4636c3bb7fab10d3ed7cd3e57e6119401bd2769e28165268'),
}


def sevenzip_tool():
    """Honor explicit workflow ownership; ambient tools are only a local fallback."""
    if 'MESH_TALK_SEVENZIP' in os.environ:
        tool = Path(os.environ['MESH_TALK_SEVENZIP'])
        if not tool.is_absolute() or not tool.is_file():
            raise RuntimeError('MESH_TALK_SEVENZIP must name an existing absolute tool file')
        return str(tool.resolve())
    return shutil.which('7zz') or shutil.which('7z')


def release_asset(system, machine):
    arch = {'x86_64': 'x64', 'amd64': 'x64', 'aarch64': 'arm64', 'arm64': 'arm64'}.get(machine.lower())
    if (system, arch) not in ASSETS:
        raise RuntimeError(f'Unsupported 7-Zip platform: {system}/{machine}')
    suffix, digest = ASSETS[(system, arch)]
    return '7z' + VERSION.replace('.', '') + '-' + suffix, digest


def download(url):
    for attempt in range(1, 4):
        try:
            return _download_once(url)
        except (urllib.error.URLError, TimeoutError) as error:
            if isinstance(error, urllib.error.HTTPError) and error.code != 429 and not 500 <= error.code < 600:
                raise RuntimeError(f'7-Zip download refused (HTTP {error.code}, attempt {attempt})') from None
            if attempt == 3:
                raise RuntimeError('7-Zip download failed after 3 attempts') from error
            time.sleep(attempt)


def _download_once(url):
    start = time.monotonic()
    chunks = []
    size = 0
    with urllib.request.urlopen(url, timeout=TIMEOUT) as response:
        while True:
            chunk = response.read(min(65536, MAX_DOWNLOAD + 1 - size))
            size += len(chunk)
            if size > MAX_DOWNLOAD:
                raise RuntimeError('7-Zip download exceeds size limit')
            if time.monotonic() - start > TIMEOUT:
                raise RuntimeError('7-Zip download exceeded deadline')
            if not chunk:
                return b''.join(chunks)
            chunks.append(chunk)


def extract(archive, directory, system):
    if system == 'Windows':
        # Official silent switches: https://www.7-zip.org/faq.html .
        # Extract with the runner's existing tool to avoid installer registry effects.
        bootstrap = shutil.which('7z') or shutil.which('7zz')
        if not bootstrap:
            raise RuntimeError('Windows bootstrap requires the runner-provided 7z')
        subprocess.run([bootstrap, 'x', str(archive), f'-o{directory}', '-y'], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=TIMEOUT)
        tool = directory / '7z.exe'
        if not tool.is_file():
            raise RuntimeError('7-Zip archive did not contain 7z.exe')
        return tool
    tool = directory / '7zz'
    with tarfile.open(archive, 'r:xz') as bundle:
        member = bundle.getmember('7zz')
        if not member.isfile() or not 0 < member.size <= MAX_DOWNLOAD:
            raise RuntimeError('7-Zip archive has no bounded regular 7zz executable')
        with bundle.extractfile(member) as source, tool.open('wb') as target:
            shutil.copyfileobj(source, target)
    tool.chmod(0o755)
    return tool


def install(directory, system=None, machine=None):
    system = system or platform.system()
    machine = machine or platform.machine()
    asset, digest = release_asset(system, machine)
    directory = Path(directory).resolve()
    if '\n' in str(directory) or '\r' in str(directory):
        raise RuntimeError('7-Zip directory contains a newline')
    directory.mkdir(parents=True, exist_ok=True)
    data = download(f'https://github.com/ip7z/7zip/releases/download/{VERSION}/{asset}')
    if hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError('7-Zip SHA256 verification failed')
    archive = directory / asset
    archive.write_bytes(data)
    tool = extract(archive, directory, system)
    result = subprocess.run([str(tool), 'i'], check=True, capture_output=True, text=True, timeout=TIMEOUT)
    if not re.search(r'^7-Zip(?: \(z\))? ' + re.escape(VERSION) + r'(?:\s|$)', result.stdout, re.MULTILINE):
        raise RuntimeError(f'Installed 7-Zip version does not match pinned version {VERSION}')
    if os.environ.get('GITHUB_ENV'):
        with open(os.environ['GITHUB_ENV'], 'a', encoding='utf-8') as output:
            output.write(f'MESH_TALK_SEVENZIP={tool}\n')
    return tool


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['install'])
    parser.add_argument('--directory', type=Path)
    args = parser.parse_args()
    directory = args.directory
    if directory is None:
        parent = os.environ.get('RUNNER_TEMP')
        if not parent:
            parser.error('RUNNER_TEMP or --directory is required')
        directory = Path(tempfile.mkdtemp(prefix=f'mesh-talk-sevenzip-{VERSION}-', dir=parent))
    print(install(directory))


if __name__ == '__main__':
    main()
