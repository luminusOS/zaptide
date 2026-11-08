"""Validate GitHub contribution configuration without network or credentials."""

import json
from pathlib import Path
import re
import tomllib
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]


class RepositoryConfiguration(unittest.TestCase):
    def test_actions_are_immutable_and_checkouts_do_not_keep_credentials(self):
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            workflow = yaml.safe_load(path.read_text())
            events = workflow.get("on", workflow.get(True))
            self.assertNotIn("pull_request_target", events, path)
            for job in workflow["jobs"].values():
                for step in job.get("steps", []):
                    action = step.get("uses")
                    if not action:
                        continue
                    self.assertRegex(action, r"@[0-9a-f]{40}$", path)
                    if action.startswith("actions/checkout@"):
                        self.assertIs(step.get("with", {}).get("persist-credentials"), False)

    def test_dependency_check_runs_weekly_and_allows_only_known_git_sources(self):
        workflow = yaml.safe_load((ROOT / ".github/workflows/deny.yml").read_text())
        events = workflow.get("on", workflow.get(True))
        self.assertTrue(events["schedule"])
        for event in ("pull_request", "push"):
            self.assertTrue({"Cargo.lock", "Cargo.toml", "deny.toml"} <= set(events[event]["paths"]))
        self.assertEqual(events["push"]["branches"], ["main"])
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        step = next(s for s in workflow["jobs"]["cargo-deny"]["steps"]
                    if s.get("uses", "").startswith("EmbarkStudios/cargo-deny-action@"))
        checks = step["with"]["command-arguments"].split()
        self.assertTrue({"advisories", "bans", "sources"} <= set(checks))
        self.assertNotIn("licenses", checks)
        deny = tomllib.loads((ROOT / "deny.toml").read_text())
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
        git = {re.split(r"[?#]", package["source"][len("git+"):])[0]
               for package in lock["package"]
               if package.get("source", "").startswith("git+")}
        self.assertTrue(git)
        self.assertEqual(set(deny["sources"]["allow-git"]), git)
        self.assertEqual(deny["sources"]["unknown-git"], "deny")
    def test_workflow_containers_are_pinned_by_digest(self):
        images = []
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            workflow = yaml.safe_load(path.read_text())
            for job in workflow["jobs"].values():
                containers = [job.get("container")]
                containers += list(job.get("services", {}).values())
                for container in filter(None, containers):
                    image = container["image"] if isinstance(container, dict) else container
                    expression = re.fullmatch(r"\$\{\{\s*matrix\.(\w+)\s*\}\}", image)
                    if expression:
                        matrix = job["strategy"]["matrix"]
                        values = [entry[expression[1]] for entry in matrix.get("include", [])]
                        values += matrix.get(expression[1], [])
                        self.assertTrue(values, (path, image))
                        images += [(path, value) for value in values]
                    else:
                        images.append((path, image))
        self.assertTrue(images)
        for path, image in images:
            self.assertRegex(image, r"^[^@\s$]+@sha256:[0-9a-f]{64}$", path)

    def test_fetched_scripts_are_pinned_to_commits(self):
        paths = list((ROOT / ".github/workflows").glob("*.yml")) + list((ROOT / "packaging").rglob("*.sh"))
        for path in paths:
            text = path.read_text()
            self.assertNotIn("refs/heads/", text, path)
            self.assertNotIn("/releases/latest/", text, path)
            self.assertNotIn("/latest/download/", text, path)
            for ref in re.findall(r"raw\.githubusercontent\.com/[^/\s]+/[^/\s]+/([^/\s]+)/", text):
                self.assertRegex(ref, r"^[0-9a-f]{40}$", path)
        build = (ROOT / "packaging/appimage/build.sh").read_text()
        self.assertRegex(build, r"QUICK_SHARUN_SHA256=[0-9a-f]{64}\n")
        check = build.index("sha256sum -c")
        self.assertLess(build.index('wget "$QUICK_SHARUN_URL"'), check)
        self.assertLess(check, build.index("chmod +x ./quick-sharun"))
        self.assertLess(check, build.index("./quick-sharun /usr/bin/zaptide"))
        self.assertNotIn("self-updater", build)

    def test_pip_installs_exact_versions(self):
        installs = 0
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            for command in re.findall(r"\bpip\"?\s+install\s+([^\n]+)", path.read_text()):
                for argument in command.split():
                    if not argument.startswith("-"):
                        installs += 1
                        self.assertIn("==", argument, path)
        self.assertTrue(installs)

    def test_tag_builds_restore_no_caches(self):
        release_only = {"release.yml", "release-appimage.yml"}
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            workflow = yaml.safe_load(path.read_text())
            for job in workflow["jobs"].values():
                for step in job.get("steps", []):
                    action = step.get("uses", "")
                    if not action.startswith(("Swatinem/rust-cache@", "actions/cache@")):
                        continue
                    self.assertNotIn(path.name, release_only, action)
                    self.assertIn("!startsWith(github.ref, 'refs/tags/')", step.get("if", ""), path)

    def test_workflows_do_not_reference_the_giphy_secret(self):
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            self.assertNotIn("GIPHY", path.read_text().upper(), path)

    def test_required_checks_run_on_every_pull_request(self):
        ruleset = json.loads((ROOT / ".github/main-ruleset.json").read_text())
        ci = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        events = ci.get("on", ci.get(True))
        self.assertIn("pull_request", events)
        self.assertFalse(events["pull_request"])
        self.assertEqual(ci["permissions"], {"contents": "read"})
        rule = next(r for r in ruleset["rules"] if r["type"] == "required_status_checks")
        for check in rule["parameters"]["required_status_checks"]:
            self.assertIn(check["context"], ci["jobs"])
            self.assertNotIn("if", ci["jobs"][check["context"]])
            self.assertEqual(check["integration_id"], 15368)

    def test_main_has_no_bypass_or_direct_update_exception(self):
        ruleset = json.loads((ROOT / ".github/main-ruleset.json").read_text())
        self.assertEqual(ruleset["enforcement"], "active")
        self.assertEqual(ruleset["bypass_actors"], [])
        self.assertEqual(ruleset["conditions"]["ref_name"]["include"], ["refs/heads/main"])
        rules = {rule["type"]: rule.get("parameters", {}) for rule in ruleset["rules"]}
        self.assertTrue({"pull_request", "deletion", "non_fast_forward"} <= rules.keys())
        self.assertNotIn("update", rules)
        self.assertEqual(rules["pull_request"]["required_approving_review_count"], 0)
        self.assertTrue(rules["pull_request"]["required_review_thread_resolution"])

    def test_issue_forms_have_unique_fields_and_required_information(self):
        for path in (ROOT / ".github/ISSUE_TEMPLATE").glob("*.yml"):
            if path.name == "config.yml":
                continue
            form = yaml.safe_load(path.read_text())
            self.assertTrue(form["name"] and form["description"])
            ids = [field["id"] for field in form["body"] if "id" in field]
            self.assertEqual(len(ids), len(set(ids)), path)
            for field in form["body"]:
                self.assertIn(field["type"], {"markdown", "input", "textarea", "dropdown", "checkboxes"})
                self.assertIn("attributes", field)
            self.assertTrue(any(field.get("validations", {}).get("required") for field in form["body"]))

    def test_relative_contributor_links_exist(self):
        for name in ("CONTRIBUTING.md", "SECURITY.md", "CODE_OF_CONDUCT.md", ".github/MAINTAINERS.md"):
            path = ROOT / name
            for target in re.findall(r"\]\(([^)]+)\)", path.read_text()):
                if "://" not in target and not target.startswith("#"):
                    self.assertTrue((path.parent / target.split("#")[0]).exists(), (name, target))


if __name__ == "__main__":
    unittest.main()
