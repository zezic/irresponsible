"""Exercise tag/version validation with actual Cargo package metadata."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/release-metadata.py"


class ReleaseMetadataTests(unittest.TestCase):
    def metadata(self, version, tag):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "src").mkdir()
            shutil.copyfile(SCRIPT, root / "scripts/release-metadata.py")
            (root / "Cargo.toml").write_text(
                f'[package]\nname = "irresponsible"\nversion = "{version}"\nedition = "2024"\n')
            (root / "src/lib.rs").write_text("")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root,
                           check=True, capture_output=True)
            output = root / "output"
            result = subprocess.run(["python3", str(root / "scripts/release-metadata.py")],
                                    env={**os.environ, "RELEASE_TAG": tag, "GITHUB_OUTPUT": str(output)},
                                    capture_output=True, text=True)
            return result, output.read_text() if output.exists() else ""

    def test_matching_tag_and_branch_builds_emit_cargo_version(self):
        for tag in ("", "v0.1.0"):
            result, output = self.metadata("0.1.0", tag)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output, "version=0.1.0\nprerelease=false\n")

    def test_mismatched_tag_fails_without_publishing_outputs(self):
        result, output = self.metadata("0.1.0", "v0.2.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must match Cargo.toml", result.stderr)
        self.assertEqual(output, "")

    def test_prerelease_and_build_metadata_have_distinct_release_status(self):
        for version, expected in (("1.2.3-rc.1+build.7", "true"), ("1.2.3+build-alpha", "false")):
            result, output = self.metadata(version, f"v{version}")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"prerelease={expected}\n", output)


if __name__ == "__main__":
    unittest.main()
