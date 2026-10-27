#!/usr/bin/env python3
"""Check manifest parity and revision-pinned generation without network access."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

import yaml

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
APP_ID = "dev.luminusos.ZapTide"
sys.path.insert(0, str(HERE))
import export_manifest


class FlatpakPackaging(unittest.TestCase):
    def test_source_and_bundle_have_the_same_sandbox(self):
        source = yaml.safe_load((HERE / f"{APP_ID}.yml").read_text())
        bundle = yaml.safe_load((HERE / f"{APP_ID}.bundle.yml").read_text())
        for key in ("id", "runtime", "runtime-version", "sdk", "command", "finish-args"):
            self.assertEqual(source[key], bundle[key], key)
        self.assertIn("--persist=.local/state", source["finish-args"])
        self.assertIn("--talk-name=org.freedesktop.secrets", source["finish-args"])
        self.assertFalse(any(arg.startswith("--filesystem=") for arg in source["finish-args"]))
        meta = ET.parse(HERE / f"{APP_ID}.metainfo.xml").getroot()
        self.assertEqual(meta.findtext("id"), APP_ID)
        self.assertEqual(meta.findtext("launchable"), f"{APP_ID}.desktop")

    def test_local_build_fetches_cargo_dependencies(self):
        manifest = yaml.safe_load((HERE / f"{APP_ID}.yml").read_text())
        self.assertIn("--share=network", manifest["build-options"]["build-args"])
        module = manifest["modules"][0]
        self.assertEqual(module["build-commands"][0], "cargo build --release --locked")
        self.assertNotIn("cargo-sources.json", module["sources"])

    def test_generation_uses_the_requested_revisions_lockfile(self):
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        lock = subprocess.check_output(["git", "show", f"{revision}:Cargo.lock"], cwd=ROOT)
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            stub = directory / "generator.py"
            stub.write_text('''import hashlib, json, pathlib, sys
lock = pathlib.Path(sys.argv[1]).read_bytes()
pathlib.Path(sys.argv[sys.argv.index("-o") + 1]).write_text(json.dumps({"lock_sha256": hashlib.sha256(lock).hexdigest()}))
''')
            output = directory / "output"
            subprocess.run([str(HERE / "flathub.sh"), revision, str(output)], cwd=ROOT,
                           env={**os.environ, "FLATPAK_CARGO_GENERATOR": str(stub)}, check=True)
            sources = json.loads((output / "cargo-sources.json").read_text())
            self.assertEqual(sources["lock_sha256"], hashlib.sha256(lock).hexdigest())
            manifest = yaml.safe_load((output / f"{APP_ID}.yml").read_text())
            self.assertEqual(manifest["modules"][0]["sources"][0]["commit"], revision)
            self.assertEqual(manifest["modules"][0]["sources"][0]["type"], "git")
            self.assertNotIn("build-args", manifest["build-options"])
            self.assertEqual(manifest["modules"][0]["build-commands"][0],
                             "cargo --offline build --release --locked")
            self.assertIn("cargo-sources.json", manifest["modules"][0]["sources"])
            self.assertTrue((output / f"{APP_ID}.metainfo.xml").is_file())

    def test_export_manifest_records_both_architectures_from_bundle_files(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = {}
            for arch in ("x86_64", "aarch64"):
                bundle = Path(directory) / f"zaptide-v1.2.3-{arch}.flatpak"
                bundle.write_bytes(f"bundle:{arch}".encode())
                artifacts[arch] = {"artifact_id": 100 if arch == "x86_64" else 200,
                                   "path": bundle}

            manifest = export_manifest.build_manifest(
                "1.2.3", "v1.2.3", "a" * 40, 1234, 2, artifacts)

        self.assertEqual(manifest["source"], {
            "repository": "luminusOS/zaptide", "tag": "v1.2.3",
            "revision": "a" * 40, "workflow": ".github/workflows/release.yml",
            "run_id": 1234, "run_attempt": 2,
        })
        self.assertEqual(set(manifest["artifacts"]), {"x86_64", "aarch64"})
        self.assertEqual(manifest["artifacts"]["aarch64"]["filename"],
                         "zaptide-v1.2.3-aarch64.flatpak")
        self.assertEqual(manifest["artifacts"]["x86_64"]["size_bytes"], len(b"bundle:x86_64"))
        self.assertEqual(manifest["artifacts"]["x86_64"]["artifact_id"], 100)
        for arch in ("x86_64", "aarch64"):
            self.assertEqual(manifest["artifacts"][arch]["sha256"],
                             hashlib.sha256(f"bundle:{arch}".encode()).hexdigest())

    def test_export_manifest_normalizes_rc_tag_and_checks_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = self._export_artifacts(Path(directory), "v1.2.3-rc1")
            manifest = export_manifest.build_manifest(
                "1.2.3-rc.1", "v1.2.3-rc1", "b" * 40, 1, 1, artifacts)
        self.assertEqual(manifest["version"], "1.2.3-rc.1")
        self.assertTrue(export_manifest.is_prerelease(manifest["source"]["tag"]))
        self.assertEqual(export_manifest.branch_for_tag("v1.2.3-rc1"), "testing")
        self.assertEqual(export_manifest.branch_for_tag("v1.2.3-rc.1"), "testing")
        self.assertEqual(export_manifest.branch_for_tag("v1.2.3"), "stable")
        self.assertTrue(export_manifest.is_prerelease("v1.2.3-rc.1"))

    def test_uploaded_bundle_metadata_must_match_downloaded_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = self._export_artifacts(Path(directory), "v1.2.3")
            arch = "aarch64"
            metadata = {"architecture": arch,
                        **export_manifest.build_artifact_record(
                            arch, artifacts[arch]["artifact_id"], artifacts[arch]["path"])}
            checked = export_manifest.validate_artifact_record(
                metadata, arch, artifacts[arch]["path"])
            self.assertEqual(checked["sha256"], metadata["sha256"])
            metadata["sha256"] = "0" * 64
            with self.assertRaises(ValueError):
                export_manifest.validate_artifact_record(
                    metadata, arch, artifacts[arch]["path"])

    def test_export_manifest_rejects_missing_architecture_and_bad_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = self._export_artifacts(Path(directory), "v1.2.3")
            with self.assertRaises(ValueError):
                export_manifest.build_manifest("1.2.3", "v1.2.3", "a" * 40,
                                               1, 1, {"x86_64": artifacts["x86_64"]})
            for revision, artifact_id in (("a" * 39, 3), ("a" * 40, True)):
                changed = dict(artifacts)
                changed["aarch64"] = {**artifacts["aarch64"], "artifact_id": artifact_id}
                with self.subTest(revision=revision, artifact_id=artifact_id), self.assertRaises(ValueError):
                    export_manifest.build_manifest("1.2.3", "v1.2.3", revision,
                                                   1, 1, changed)
            changed = dict(artifacts)
            changed["aarch64"] = {**artifacts["aarch64"], "artifact_id": 1}
            with self.assertRaises(ValueError):
                export_manifest.build_manifest("1.2.3", "v1.2.3", "a" * 40,
                                               1, 1, changed)

    def test_export_manifest_rejects_architecture_filename_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = self._export_artifacts(Path(directory), "v1.2.3")
            artifacts["aarch64"]["path"] = artifacts["x86_64"]["path"]
            with self.assertRaises(ValueError):
                export_manifest.build_manifest("1.2.3", "v1.2.3", "a" * 40,
                                               1, 1, artifacts)

    def test_export_manifest_rejects_bundle_from_another_version(self):
        with tempfile.TemporaryDirectory() as directory:
            artifacts = self._export_artifacts(Path(directory), "v1.2.2")
            with self.assertRaises(ValueError):
                export_manifest.build_manifest("1.2.3", "v1.2.3", "a" * 40,
                                               1, 1, artifacts)

    def test_export_report_requires_matching_refs_runtime_and_commit(self):
        reports = {
            arch: {"application": export_manifest.APP_ID, "architecture": arch,
                   "ref": f"app/{export_manifest.APP_ID}/{arch}/stable",
                   "ostree_commit": "c" * 64,
                   "runtime": f"org.gnome.Platform/{arch}/51"}
            for arch in ("x86_64", "aarch64")
        }
        result = export_manifest.build_export_report(reports, "v1.2.3")
        self.assertEqual(result["architectures"]["aarch64"]["runtime"],
                         "org.gnome.Platform/aarch64/51")
        testing_reports = {
            arch: {**report, "ref": report["ref"].replace("/stable", "/testing")}
            for arch, report in reports.items()
        }
        self.assertEqual(export_manifest.build_export_report(
            testing_reports, "v1.2.3-rc.1")["branch"], "testing")
        reports["aarch64"]["ref"] = "app/dev.luminusos.ZapTide/x86_64/stable"
        with self.assertRaises(ValueError):
            export_manifest.build_export_report(reports, "v1.2.3")

    def test_export_report_rejects_ref_branch_mismatches_for_both_channels(self):
        reports = {
            arch: {"application": export_manifest.APP_ID, "architecture": arch,
                   "ref": f"app/{export_manifest.APP_ID}/{arch}/stable",
                   "ostree_commit": "c" * 64,
                   "runtime": f"org.gnome.Platform/{arch}/51"}
            for arch in ("x86_64", "aarch64")
        }
        with self.assertRaises(ValueError):
            export_manifest.build_export_report(reports, "v1.2.3-rc1")
        for arch, report in reports.items():
            report["ref"] = f"app/{export_manifest.APP_ID}/{arch}/testing"
        with self.assertRaises(ValueError):
            export_manifest.build_export_report(reports, "v1.2.3")

    def test_vendored_contract_schema_matches_approved_hash(self):
        schema = HERE / "contract" / "distribution.schema.json"
        self.assertEqual(hashlib.sha256(schema.read_bytes()).hexdigest(),
                         export_manifest.PINNED_SCHEMA_SHA256)

    def test_release_workflow_exports_both_architectures_without_new_secrets(self):
        workflow_path = ROOT / ".github/workflows/release.yml"
        workflow_text = workflow_path.read_text()
        workflow = yaml.safe_load(workflow_text)
        jobs = workflow["jobs"]
        self.assertEqual(jobs["flatpak"]["needs"], "build")
        self.assertEqual(jobs["manifest"]["needs"], "flatpak")
        self.assertIn("manifest", jobs["publish"]["needs"])
        self.assertEqual(jobs["flatpak"]["permissions"], {"contents": "read"})
        self.assertEqual(jobs["manifest"]["permissions"], {"contents": "read"})
        self.assertNotIn("ZAPTIDE_GIPHY_KEY", workflow.get("env", {}))
        self.assertIn("ZAPTIDE_GIPHY_KEY", jobs["build"]["env"])
        self.assertEqual({entry["arch"] for entry in jobs["flatpak"]["strategy"]["matrix"]["include"]},
                         {"x86_64", "aarch64"})
        self.assertIn("steps.upload-bundle.outputs.artifact-id", workflow_text)
        self.assertIn("steps.flatpak-branch.outputs.branch", workflow_text)
        self.assertIn("contains(github.ref_name, '-rc')", workflow_text)
        self.assertNotIn('branch: stable', workflow_text)
        self.assertIn(r"-rc\.?[1-9][0-9]*", workflow_text)
        for name in ("flatpak", "manifest"):
            self.assertNotIn("secrets.", yaml.safe_dump(jobs[name]))
        for action in re.findall(r"uses:\s*([^\s]+)", workflow_text):
            if action.startswith("./"):
                continue
            self.assertRegex(action, r"@(?:[a-f0-9]{40})(?:\s|$)", action)
        publish_files = yaml.safe_load(workflow_text)["jobs"]["publish"]["steps"][-1]["with"]["files"]
        self.assertIn("dist/checksums.txt", publish_files)
        self.assertNotIn("manifest", publish_files)
        self.assertNotIn("export-report", publish_files)
        self.assertIn("flatpak-release-manifest.json", workflow_text)

    @staticmethod
    def _export_artifacts(directory, tag):
        artifacts = {}
        for index, arch in enumerate(("x86_64", "aarch64"), 1):
            bundle = directory / f"zaptide-{tag}-{arch}.flatpak"
            bundle.write_bytes(f"bundle:{arch}".encode())
            artifacts[arch] = {"artifact_id": index, "path": bundle}
        return artifacts


if __name__ == "__main__":
    unittest.main()
