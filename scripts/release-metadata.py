#!/usr/bin/env python3
"""Validate version-tag releases against Cargo's package metadata."""
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.run(
    ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
    cwd=root, check=True, capture_output=True, text=True,
).stdout)
package = next(package for package in metadata["packages"]
               if Path(package["manifest_path"]) == root / "Cargo.toml")
version = package["version"]
tag = os.environ.get("RELEASE_TAG", "")
if tag and tag != f"v{version}":
    raise SystemExit(f"Release tag {tag!r} must match Cargo.toml: v{version}")
prerelease = "-" in version.split("+", 1)[0]
output = f"version={version}\nprerelease={str(prerelease).lower()}\n"
if path := os.environ.get("GITHUB_OUTPUT"):
    with open(path, "a", encoding="utf-8") as stream:
        stream.write(output)
print(output, end="")
