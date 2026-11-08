#!/usr/bin/env python3
import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parent.parent
NOTICES = (
    "LICENSE",
    "vendor/absolute-url/LICENSE-APACHE",
    "vendor/absolute-url/LICENSE-MIT",
    "vendor/idna-1.1.0/LICENSE-APACHE",
    "vendor/idna-1.1.0/LICENSE-MIT",
    "vendor/icu_normalizer/LICENSE",
)


class LicensePackaging(unittest.TestCase):
    def assert_notices(self, destination):
        self.assertEqual(
            {str(p.relative_to(destination)) for p in destination.rglob('*') if p.is_file()},
            set(NOTICES),
        )
        for notice in NOTICES:
            original = (ROOT / notice).read_bytes()
            installed = (destination / notice).read_bytes()
            self.assertEqual(installed, original, notice)
            print(f"{notice} sha256={hashlib.sha256(installed).hexdigest()}")

    def run_command(self, command, cwd=ROOT, env=None):
        subprocess.run(["sh", "-eu", "-c", command], cwd=cwd, env=env, check=True)

    def test_distribution_entrypoints(self):
        with tempfile.TemporaryDirectory(dir="/tmp/opencode") as directory:
            temp = Path(directory)
            direct = temp / "direct destination"
            subprocess.run(["sh", "packaging/install-licenses.sh", str(direct)], cwd=ROOT, check=True)
            self.assert_notices(direct)

            appimage = (ROOT / "packaging/appimage/build.sh").read_text()
            command = next(line for line in appimage.splitlines() if line.startswith("sh packaging/install-licenses.sh "))
            self.assertLess(appimage.index(command), appimage.index("for dir in bin lib share etc shared"))
            self.run_command(command.replace("AppDir/", f"{temp}/AppDir/"))
            self.assert_notices(temp / "AppDir/share/licenses/zaptide")

            flatpak = ROOT / "packaging/flatpak"
            source = yaml.safe_load((flatpak / "dev.luminusos.ZapTide.yml").read_text())
            source_command = next(c for c in source["modules"][0]["build-commands"] if "install-licenses.sh" in c)
            self.run_command(source_command.replace("/app/", f"{temp}/source-app/"))
            self.assert_notices(temp / "source-app/share/licenses/zaptide")

            workflow = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
            package = next(step for step in workflow["jobs"]["build"]["steps"] if step.get("name") == "Package")["run"]
            archive_command = next(line for line in package.splitlines() if "install-licenses.sh" in line)
            self.assertLess(package.index(archive_command), package.index("cd dist"))
            dist = temp / "dist"
            (dist / "synthetic").mkdir(parents=True)
            self.run_command(archive_command.replace('dist/$name/', f'{dist}/$name/'), env={**os.environ, "name": "synthetic"})
            archive = temp / "synthetic.tar.gz"
            subprocess.run(["tar", "czf", str(archive), "synthetic"], cwd=dist, check=True)
            unpacked = temp / "unpacked"
            unpacked.mkdir()
            with tarfile.open(archive) as tar:
                tar.extractall(unpacked, filter="data")
            built = unpacked / "synthetic"
            self.assert_notices(built / "licenses/zaptide")

            bundle = yaml.safe_load((flatpak / "dev.luminusos.ZapTide.bundle.yml").read_text())
            bundle_command = next(c for c in bundle["modules"][0]["build-commands"] if "licenses/zaptide/." in c)
            self.run_command(bundle_command.replace("/app/", f"{temp}/bundle-app/"), cwd=built)
            self.assert_notices(temp / "bundle-app/share/licenses/zaptide")


if __name__ == "__main__":
    unittest.main()
