import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";

// Exercise the Python owner directly; no network or machine-wide installation.
test("pinned 7-Zip validates downloads, selection and publication", () => {
  const source = String.raw`
import hashlib, importlib.util, io, os, pathlib, subprocess, tarfile, tempfile, unittest, urllib.error
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('sevenzip', 'scripts/release/sevenzip.py')
s = importlib.util.module_from_spec(spec)
spec.loader.exec_module(s)
class Tests(unittest.TestCase):
    def test_selection(self):
        with tempfile.TemporaryDirectory() as d:
            tool = pathlib.Path(d) / '7zz'
            tool.touch()
            with patch.dict(os.environ, {'MESH_TALK_SEVENZIP': str(tool)}):
                self.assertEqual(s.sevenzip_tool(), str(tool.resolve()))
            with patch.dict(os.environ, {'MESH_TALK_SEVENZIP': str(tool) + '-missing'}):
                with self.assertRaises(RuntimeError): s.sevenzip_tool()
        with patch.dict(os.environ, {}, clear=True), patch('shutil.which', side_effect=lambda n: '/bin/7z' if n == '7z' else None):
            self.assertEqual(s.sevenzip_tool(), '/bin/7z')
    def test_platforms(self):
        for system, machine in [('Linux','x86_64'), ('Linux','aarch64'), ('Darwin','arm64'), ('Darwin','x86_64'), ('Windows','AMD64'), ('Windows','ARM64')]:
            asset, digest = s.release_asset(system, machine)
            self.assertEqual(len(digest), 64)
            self.assertTrue(asset.startswith('7z2605-'))
        with self.assertRaises(RuntimeError): s.release_asset('Linux', 'riscv64')
    def test_hash_before_extract_or_execute(self):
        with tempfile.TemporaryDirectory() as d, patch.object(s, 'download', return_value=b'corrupt'), patch.object(s, 'extract') as extract, patch('subprocess.run') as run:
            with self.assertRaisesRegex(RuntimeError, 'SHA256'): s.install(pathlib.Path(d), 'Linux', 'x86_64')
            extract.assert_not_called()
            run.assert_not_called()
    def test_version_before_environment(self):
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            env = root / 'env'
            data = b'verified fixture'
            with patch.dict(os.environ, {'GITHUB_ENV': str(env)}), patch.object(s, 'release_asset', return_value=('7z2605-linux-x64.tar.xz', hashlib.sha256(data).hexdigest())), patch.object(s, 'download', return_value=data), patch.object(s, 'extract', return_value=root/'7zz'), patch('subprocess.run', return_value=subprocess.CompletedProcess([],0,'7-Zip (z) 26.00','')):
                with self.assertRaisesRegex(RuntimeError, 'version'): s.install(root, 'Linux', 'x86_64')
                self.assertFalse(env.exists())
            with patch.dict(os.environ, {'GITHUB_ENV': str(env)}), patch.object(s, 'release_asset', return_value=('7z2605-linux-x64.tar.xz', hashlib.sha256(data).hexdigest())), patch.object(s, 'download', return_value=data), patch.object(s, 'extract', return_value=root/'7zz'), patch('subprocess.run', return_value=subprocess.CompletedProcess([],0,'7-Zip (z) 26.05 (arm64)','')):
                self.assertEqual(s.install(root, 'Linux', 'x86_64'), root/'7zz')
                self.assertEqual(env.read_text(), 'MESH_TALK_SEVENZIP=' + str(root/'7zz') + '\n')
    def test_download_bound(self):
        with patch.object(s.urllib.request, 'urlopen', return_value=io.BytesIO(b'a' * 17)), patch.object(s, 'MAX_DOWNLOAD', 16):
            with self.assertRaisesRegex(RuntimeError, 'size'): s.download('https://example.invalid')
    def test_windows_extract_uses_bootstrap_not_installer(self):
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            (root/'7z.exe').touch()
            with patch('shutil.which', return_value='/runner/7z'), patch('subprocess.run') as run:
                self.assertEqual(s.extract(root/'official.exe', root, 'Windows'), root/'7z.exe')
                command = run.call_args.args[0]
                self.assertEqual(command[:3], ['/runner/7z', 'x', str(root/'official.exe')])
                self.assertIn('-o' + str(root), command)
            with patch('shutil.which', return_value=None):
                with self.assertRaisesRegex(RuntimeError, 'bootstrap'): s.extract(root/'official.exe', root, 'Windows')
    def test_transient_download_retry_is_bounded(self):
        with patch.object(s.urllib.request, 'urlopen', side_effect=[urllib.error.URLError('transient'), io.BytesIO(b'ok')]), patch.object(s.time, 'sleep'):
            self.assertEqual(s.download('https://example.invalid'), b'ok')
        with patch.object(s.urllib.request, 'urlopen', side_effect=urllib.error.URLError('transient')) as request, patch.object(s.time, 'sleep'):
            with self.assertRaisesRegex(RuntimeError, '3 attempts') as caught: s.download('https://example.invalid')
            self.assertIsInstance(caught.exception.__cause__, urllib.error.URLError)
            self.assertEqual(request.call_count, 3)
    def test_permanent_http_failure_is_not_retried(self):
        with patch.object(s.urllib.request, 'urlopen', side_effect=urllib.error.HTTPError('https://example.invalid', 404, 'Not found', {}, None)) as request:
            with self.assertRaisesRegex(RuntimeError, 'HTTP 404'): s.download('https://example.invalid')
            self.assertEqual(request.call_count, 1)
    def test_download_deadline(self):
        with patch.object(s.urllib.request, 'urlopen', return_value=io.BytesIO(b'ok')), patch.object(s.time, 'monotonic', side_effect=[0, s.TIMEOUT + 1]):
            with self.assertRaisesRegex(RuntimeError, 'deadline'): s.download('https://example.invalid')
    def test_extract_only_regular_tool(self):
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            archive = root/'archive.tar.xz'
            with tarfile.open(archive, 'w:xz') as t:
                member = tarfile.TarInfo('7zz'); member.type = tarfile.SYMTYPE; member.linkname = '/etc/passwd'; t.addfile(member)
            with self.assertRaises(RuntimeError): s.extract(archive, root, 'Linux')
unittest.main()
`;
  const result = spawnSync(
    process.platform === "win32" ? "python" : "python3",
    ["-c", source],
    {
      cwd: new URL("../../", import.meta.url),
      encoding: "utf8",
      timeout: 30_000,
    },
  );
  assert.equal(result.status, 0, result.stdout + result.stderr);
});
