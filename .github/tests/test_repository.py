"""Validate GitHub contribution configuration without network or credentials."""

import json
from pathlib import Path
import re
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
