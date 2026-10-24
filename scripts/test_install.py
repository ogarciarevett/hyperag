"""Exercise installer integrity and platform selection with synthetic release assets."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("install.sh").resolve()
TARGETS = [("Darwin", "arm64", "aarch64-apple-darwin"), ("Darwin", "x86_64", "x86_64-apple-darwin"),
           ("Linux", "x86_64", "x86_64-unknown-linux-gnu"), ("Linux", "aarch64", "aarch64-unknown-linux-gnu")]


class Installer(unittest.TestCase):
    def fixture(self, root, target, tamper=False):
        name = f"hyperag-v0.2.0-{target}"
        archive = root / (name + ".tar.gz")
        data = b"#!/bin/sh\necho 'hyperag synthetic-test-only'\n"
        with tarfile.open(archive, "w:gz") as tar:
            item = tarfile.TarInfo(name + "/hyperag")
            item.size, item.mode = len(data), 0o755
            tar.addfile(item, io.BytesIO(data))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (root / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
        if tamper:
            archive.write_bytes(archive.read_bytes() + b"synthetic tamper")
        tools = root / "tools"
        tools.mkdir()
        curl = tools / "curl"
        curl.write_text(f"#!{sys.executable}\n" + '''import json,os,pathlib,shutil,sys
args=sys.argv[1:];root=pathlib.Path(os.environ['SYNTHETIC_RELEASE']);url=args[-1]
name=os.environ['SYNTHETIC_ARCHIVE']
if '-o' in args:
    file=name if url.endswith('/assets/1') else 'SHA256SUMS'
    shutil.copyfile(root/file,args[args.index('-o')+1])
elif url.endswith('/latest'):print(json.dumps({'tag_name':'v0.2.0'}))
else:print(json.dumps({'assets':[{'url':'https://api.github.com/repos/demo/test/releases/assets/1','name':name},{'url':'https://api.github.com/repos/demo/test/releases/assets/2','name':'SHA256SUMS'}]}))
''')
        curl.chmod(0o755)
        return tools, archive

    def run_installer(self, root, platform, tamper=False):
        system, machine, target = platform
        tools, archive = self.fixture(root, target, tamper)
        uname = tools / "uname"
        uname.write_text(f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
        uname.chmod(0o755)
        dest = root / "bin with spaces"
        env = {**os.environ, "PATH": str(tools) + os.pathsep + os.environ["PATH"], "SYNTHETIC_RELEASE": str(root),
               "SYNTHETIC_ARCHIVE": archive.name, "HYPERAG_INSTALL_DIR": str(dest), "GH_TOKEN": "fake-test-only"}
        env.pop("HYPERAG_VERSION", None)
        result = subprocess.run(["sh", str(SCRIPT)], env=env, capture_output=True, text=True)
        return result, dest

    def test_all_unix_release_targets_install_after_checksum_verification(self):
        for target in TARGETS:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                r, dest = self.run_installer(Path(directory), target)
                self.assertEqual(r.returncode, 0, r.stderr)
                binary = dest / "hyperag"
                self.assertTrue(os.access(binary, os.X_OK))
                self.assertIn("synthetic-test-only", subprocess.check_output([str(binary)], text=True))

    def test_checksum_failure_never_installs_the_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            r, dest = self.run_installer(Path(directory), TARGETS[2], tamper=True)
            self.assertNotEqual(r.returncode, 0)
            self.assertFalse((dest / "hyperag").exists())


if __name__ == "__main__":
    unittest.main()
