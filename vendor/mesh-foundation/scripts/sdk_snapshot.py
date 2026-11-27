#!/usr/bin/env python3
"""Export reviewed SDK snapshots; verify contents offline or against a Git checkout."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tempfile

SELECTED = ('Cargo.toml', 'README.md', 'LICENSE', 'crates', 'scripts/sdk_snapshot.py')
MANIFEST = 'SNAPSHOT.json'


def git(source, *args):
    return subprocess.check_output(['git', '-C', str(source), *args])


def safe_path(name):
    path = PurePosixPath(name)
    if not name or path.is_absolute() or '..' in path.parts or '\\' in name or str(path) != name:
        raise ValueError(f'unsafe snapshot path: {name!r}')
    return path


def source_files(source, revision):
    if not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('SOURCE must be a full Git commit SHA')
    files = {}
    for entry in git(source, 'ls-tree', '-rz', revision, '--', *SELECTED).split(b'\0'):
        if not entry:
            continue
        header, raw_name = entry.split(b'\t', 1)
        mode, kind, oid = header.split()
        name = raw_name.decode('utf-8')
        safe_path(name)
        if mode not in (b'100644', b'100755') or kind != b'blob':
            raise ValueError(f'non-regular source file: {name}')
        files[name] = git(source, 'cat-file', 'blob', oid.decode())
    if 'Cargo.toml' not in files:
        raise ValueError('source revision is not an SDK snapshot')
    return files


def disk_files(destination):
    if destination.is_symlink() or not destination.is_dir():
        raise ValueError('snapshot must be a real directory')
    result = {}
    for path in destination.rglob('*'):
        if path.is_symlink():
            raise ValueError(f'symlink in snapshot: {path}')
        if path.is_dir():
            continue
        if not path.is_file():
            raise ValueError(f'non-regular snapshot file: {path}')
        name = path.relative_to(destination).as_posix()
        safe_path(name)
        result[name] = path.read_bytes()
    return result


def digests(files):
    return {name: hashlib.sha256(body).hexdigest() for name, body in sorted(files.items())}


def verify_snapshot(destination, source=None):
    files = disk_files(Path(destination))
    try:
        marker = files.pop('SOURCE').decode().strip()
        manifest = json.loads(files.pop(MANIFEST))
    except (KeyError, ValueError, UnicodeError) as error:
        raise ValueError('missing or invalid snapshot provenance') from error
    if not isinstance(manifest, dict):
        raise ValueError('invalid snapshot manifest')
    if not re.fullmatch(r'[0-9a-f]{40}', marker) or manifest.get('revision') != marker or manifest.get('format') != 1:
        raise ValueError('snapshot revision mismatch')
    expected = manifest.get('files')
    if not isinstance(expected, dict):
        raise ValueError('invalid snapshot file inventory')
    for name in expected:
        safe_path(name)
    if digests(files) != expected:
        raise ValueError('snapshot has missing, extra or modified files')
    if source is not None and digests(source_files(source, marker)) != expected:
        raise ValueError('snapshot does not match its recorded Git source')
    return marker


def export_snapshot(source, destination):
    if Path(destination).is_symlink():
        raise ValueError("snapshot destination must not be a symlink")
    source, destination = Path(source).resolve(), Path(destination).resolve()
    if destination == source or source in destination.parents:
        raise ValueError('destination must be outside the source checkout')
    if git(source, 'status', '--porcelain', '--untracked-files=normal').strip():
        raise ValueError('commit SDK changes before exporting a snapshot')
    revision = git(source, 'rev-parse', 'HEAD').decode().strip()
    files = source_files(source, revision)
    if destination.exists() or destination.is_symlink():
        current = disk_files(destination)
        if current:
            if MANIFEST in current:
                verify_snapshot(destination, source)
            else:
                # Adopt the old SOURCE-only layout only when every byte matches
                # its historical commit. Never overwrite unrelated consumer work.
                try:
                    prior = current.pop('SOURCE').decode().strip()
                except (KeyError, UnicodeError) as error:
                    raise ValueError('unrecognized snapshot destination') from error
                if current != source_files(source, prior):
                    raise ValueError('legacy snapshot differs from its Git source')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.mesh-snapshot-', dir=destination.parent) as temp:
        staged = Path(temp) / 'snapshot'
        staged.mkdir()
        for name, body in files.items():
            path = staged / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(body)
        (staged / 'SOURCE').write_text(revision + '\n')
        (staged / MANIFEST).write_text(json.dumps({'format': 1, 'revision': revision, 'files': digests(files)}, indent=2) + '\n')
        verify_snapshot(staged, source)
        # Keep the verified old snapshot recoverable until the staged tree is
        # installed. A failed rename restores it; no partial file copy is exposed.
        backup = Path(temp) / 'previous'
        existed = destination.exists()
        if existed:
            destination.rename(backup)
        try:
            staged.rename(destination)
        except OSError:
            if existed:
                backup.rename(destination)
            raise
    return revision


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    export = commands.add_parser('export')
    export.add_argument('destination', type=Path)
    export.add_argument('--source', type=Path, default=Path(__file__).resolve().parents[1])
    verify = commands.add_parser('verify')
    verify.add_argument('destination', type=Path)
    verify.add_argument('--source', type=Path)
    args = parser.parse_args()
    try:
        revision = (export_snapshot(args.source, args.destination) if args.command == 'export'
                    else verify_snapshot(args.destination, args.source))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'{error}\n')
    print(f'Verified SDK snapshot {revision}')


if __name__ == '__main__':
    main()
