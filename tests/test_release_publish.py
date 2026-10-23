"""Check release publication transitions without contacting GitHub."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/publish-release.sh"


class ReleasePublishTests(unittest.TestCase):
    def publish(self, releases, missing_asset=False, tag="v0.1.0", prerelease="false"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "dist").mkdir()
            (root / "bin").mkdir()
            shutil.copyfile(SCRIPT, root / "scripts/publish-release.sh")
            for arch in ("arm64", "x86_64"):
                if not missing_asset or arch == "arm64":
                    (root / f"dist/IRresponsible-0.1.0-macos-{arch}.zip").write_bytes(b"archive fixture")
            gh = root / "bin/gh"
            gh.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ["CALLS"], "a") as stream:
    stream.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "api":
    print(os.environ["RELEASES"])
''')
            sha = root / "bin/sha256sum"
            sha.write_text('''#!/usr/bin/env python3
import hashlib, pathlib, sys
for name in sys.argv[1:]:
    print(hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest() + "  " + name)
''')
            gh.chmod(0o755)
            sha.chmod(0o755)
            calls = root / "calls"
            env = {**os.environ, "PATH": f"{root / 'bin'}:{os.environ['PATH']}",
                   "GH_REPO": "example/irresponsible", "RELEASE_TAG": tag,
                   "VERSION": "0.1.0", "PRERELEASE": prerelease, "CALLS": str(calls),
                   "RELEASES": json.dumps([releases])}
            result = subprocess.run(["bash", str(root / "scripts/publish-release.sh")],
                                    env=env, capture_output=True, text=True)
            commands = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            return result, commands

    def test_new_release_uploads_to_a_draft_before_publication(self):
        result, commands = self.publish([])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([command[:2] for command in commands],
                         [["api", "--paginate"], ["release", "create"],
                          ["release", "upload"], ["release", "edit"]])
        self.assertIn("--draft", commands[1])
        self.assertIn("--verify-tag", commands[1])
        self.assertIn("SHA256SUMS", commands[2])
        self.assertIn("--draft=false", commands[3])

    def test_prerelease_flag_is_preserved_when_publishing(self):
        result, commands = self.publish([], prerelease="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--prerelease", commands[1])
        self.assertIn("--prerelease=true", commands[-1])

    def test_failed_draft_can_resume_without_creating_another_release(self):
        result, commands = self.publish([{"tag_name": "v0.1.0", "draft": True}])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([command[:2] for command in commands[1:]],
                         [["release", "upload"], ["release", "edit"]])

    def test_published_release_is_left_unchanged(self):
        result, commands = self.publish([{"tag_name": "v0.1.0", "draft": False}])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(commands), 1)
        self.assertEqual(commands[0][0], "api")

    def test_incomplete_or_mismatched_builds_never_contact_github(self):
        for kwargs in ({"missing_asset": True}, {"tag": "v0.2.0"}):
            result, commands = self.publish([], **kwargs)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(commands, [])


if __name__ == "__main__":
    unittest.main()
