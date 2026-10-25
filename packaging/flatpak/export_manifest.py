#!/usr/bin/env python3
"""Build and validate the traceable Flatpak release export."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import tomllib
import xml.etree.ElementTree as ET

from jsonschema import Draft202012Validator


HERE = Path(__file__).resolve().parent
APP_ID = "dev.luminusos.ZapTide"
REPOSITORY = "luminusOS/zaptide"
WORKFLOW = ".github/workflows/release.yml"
ARCHITECTURES = ("x86_64", "aarch64")
PINNED_SCHEMA_SHA256 = "a17635d45c5a0f0c2c51a075133f31884ce14cd1393daa20600255836732fe8a"
SCHEMA_PATH = HERE / "contract" / "distribution.schema.json"
TAG_PATTERN = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-rc\.?([1-9][0-9]*))?\Z")
REVISION_PATTERN = re.compile(r"[a-f0-9]{40}\Z")
COMMIT_PATTERN = re.compile(r"[a-f0-9]{64}\Z")


def load_schema():
    schema_bytes = SCHEMA_PATH.read_bytes()
    if hashlib.sha256(schema_bytes).hexdigest() != PINNED_SCHEMA_SHA256:
        raise ValueError("vendored distribution schema hash does not match its pin")
    schema = json.loads(schema_bytes)
    Draft202012Validator.check_schema(schema)
    return schema


def version_from_tag(tag):
    match = TAG_PATTERN.fullmatch(tag) if isinstance(tag, str) else None
    if match is None:
        raise ValueError("invalid release tag")
    version = ".".join(match.groups()[:3])
    if match.group(4):
        version += f"-rc.{match.group(4)}"
    return version


def is_prerelease(tag):
    return "-rc." in version_from_tag(tag)


def branch_for_tag(tag):
    return "testing" if is_prerelease(tag) else "stable"


def _positive_int(value, field):
    if type(value) is not int or value < 1:
        raise ValueError(f"{field} must be a positive integer")
    return value


def build_artifact_record(arch, artifact_id, bundle_path):
    if arch not in ARCHITECTURES:
        raise ValueError("unsupported artifact architecture")
    artifact_id = _positive_int(artifact_id, "artifact_id")
    path = Path(bundle_path)
    expected_arch_suffix = f"-{arch}.flatpak"
    if not path.is_file() or not path.name.endswith(expected_arch_suffix):
        raise ValueError(f"bundle filename does not match {arch}")
    if len(path.name) > 200 or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", path.name):
        raise ValueError("bundle filename is not contract-safe")
    digest = hashlib.sha256()
    size_bytes = 0
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
            size_bytes += len(chunk)
    if size_bytes < 1:
        raise ValueError("bundle is empty")
    return {"artifact_id": artifact_id, "filename": path.name,
            "sha256": digest.hexdigest(), "size_bytes": size_bytes}


def validate_artifact_record(record, arch, bundle_path):
    if not isinstance(record, dict) or record.get("architecture") != arch:
        raise ValueError(f"artifact metadata does not match {arch}")
    artifact = build_artifact_record(arch, record["artifact_id"], bundle_path)
    if any(record.get(key) != value for key, value in artifact.items()):
        raise ValueError(f"downloaded {arch} bundle does not match uploaded metadata")
    return artifact


def build_manifest(version, tag, revision, run_id, run_attempt, artifacts):
    normalized_version = version_from_tag(tag)
    if version != normalized_version:
        raise ValueError("release version and tag disagree")
    if not isinstance(revision, str) or not REVISION_PATTERN.fullmatch(revision):
        raise ValueError("revision must be a full lowercase git SHA")
    run_id = _positive_int(run_id, "run_id")
    run_attempt = _positive_int(run_attempt, "run_attempt")
    if not isinstance(artifacts, dict) or set(artifacts) != set(ARCHITECTURES):
        raise ValueError("both x86_64 and aarch64 artifacts are required")

    artifact_records = {
        arch: build_artifact_record(arch, artifacts[arch]["artifact_id"],
                                    artifacts[arch]["path"])
        for arch in ARCHITECTURES
    }
    for arch, artifact in artifact_records.items():
        if artifact["filename"] != f"zaptide-{tag}-{arch}.flatpak":
            raise ValueError(f"bundle filename does not match release tag and {arch}")
    if len({item["artifact_id"] for item in artifact_records.values()}) != 2:
        raise ValueError("architectures must identify distinct build artifacts")

    manifest = {
        "schema_version": 1,
        "kind": "release",
        "format": "flatpak",
        "application": APP_ID,
        "version": version,
        "source": {
            "repository": REPOSITORY,
            "tag": tag,
            "revision": revision,
            "workflow": WORKFLOW,
            "run_id": run_id,
            "run_attempt": run_attempt,
        },
        "artifacts": artifact_records,
    }
    errors = sorted(Draft202012Validator(load_schema()).iter_errors(manifest),
                    key=lambda error: list(error.absolute_path))
    if errors:
        raise ValueError(f"release manifest does not satisfy contract: {errors[0].message}")
    return manifest


def build_export_report(reports, tag):
    if not isinstance(reports, dict) or set(reports) != set(ARCHITECTURES):
        raise ValueError("Flatpak verification reports must include both architectures")
    branch = branch_for_tag(tag)
    architectures = {}
    for arch in ARCHITECTURES:
        report = reports[arch]
        expected_ref = f"app/{APP_ID}/{arch}/{branch}"
        expected_runtime = f"org.gnome.Platform/{arch}/51"
        if (not isinstance(report, dict)
                or report.get("application") != APP_ID
                or report.get("architecture") != arch
                or report.get("ref") != expected_ref
                or report.get("runtime") != expected_runtime
                or not isinstance(report.get("ostree_commit"), str)
                or not COMMIT_PATTERN.fullmatch(report["ostree_commit"])):
            raise ValueError(f"Flatpak verification report does not match {arch}")
        architectures[arch] = {
            "ref": report["ref"],
            "ostree_commit": report["ostree_commit"],
            "runtime": report["runtime"],
        }
    return {"schema_version": 1, "application": APP_ID, "branch": branch,
            "architectures": architectures}


def _read_json(path):
    return json.loads(Path(path).read_text())


def _read_release_version(tag):
    version = version_from_tag(tag)
    cargo = tomllib.loads((HERE.parent.parent / "Cargo.toml").read_text())
    if cargo["package"]["version"] != version.split("-rc.", 1)[0]:
        raise ValueError("release tag does not match Cargo.toml version")
    metainfo = ET.parse(HERE / f"{APP_ID}.metainfo.xml").getroot()
    newest = metainfo.find("releases/release")
    if newest is None or newest.get("version") != version.split("-rc.", 1)[0]:
        raise ValueError("release tag does not match newest metainfo release")
    return version


def main(argv=None):
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    record_parser = subparsers.add_parser("record")
    record_parser.add_argument("--arch", choices=ARCHITECTURES, required=True)
    record_parser.add_argument("--artifact-id", type=int, required=True)
    record_parser.add_argument("--bundle", type=Path, required=True)
    record_parser.add_argument("--output", type=Path, required=True)
    branch_parser = subparsers.add_parser("branch")
    branch_parser.add_argument("--tag", required=True)
    manifest_parser = subparsers.add_parser("manifest")
    manifest_parser.add_argument("--tag", required=True)
    manifest_parser.add_argument("--revision", required=True)
    manifest_parser.add_argument("--run-id", type=int, required=True)
    manifest_parser.add_argument("--run-attempt", type=int, required=True)
    manifest_parser.add_argument("--metadata-dir", type=Path, required=True)
    manifest_parser.add_argument("--bundle-dir", type=Path, required=True)
    manifest_parser.add_argument("--report-dir", type=Path, required=True)
    manifest_parser.add_argument("--output", type=Path, required=True)
    manifest_parser.add_argument("--report-output", type=Path, required=True)
    args = parser.parse_args(argv)

    try:
        if args.command == "branch":
            print(branch_for_tag(args.tag))
            return 0
        if args.command == "record":
            record = build_artifact_record(args.arch, args.artifact_id, args.bundle)
            args.output.write_text(json.dumps({"architecture": args.arch, **record}, indent=2) + "\n")
            return 0

        tag = args.tag
        version = _read_release_version(tag)
        artifacts = {}
        reports = {}
        for arch in ARCHITECTURES:
            record = _read_json(args.metadata_dir / arch / f"flatpak-export-{arch}.json")
            bundle_path = args.bundle_dir / arch / record["filename"]
            validate_artifact_record(record, arch, bundle_path)
            artifacts[arch] = {"artifact_id": record["artifact_id"], "path": bundle_path}
            reports[arch] = _read_json(args.report_dir / arch / f"flatpak-export-report-{arch}.json")

        manifest = build_manifest(version, tag, args.revision, args.run_id,
                                  args.run_attempt, artifacts)
        export_report = build_export_report(reports, tag)
        args.output.write_text(json.dumps(manifest, indent=2) + "\n")
        args.report_output.write_text(json.dumps(export_report, indent=2) + "\n")
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"export failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
