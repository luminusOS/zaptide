# Repository settings

These are administrative settings, not Actions jobs. Changing a file does not
change live GitHub enforcement. Apply settings only with repository administrator
permission and verify the resulting rules through GitHub's API or UI.

## Main branch

`main-ruleset.json` is the desired repository ruleset. It requires PRs, resolved
review threads, linear history, and the existing GitHub Actions checks `fmt` and
`check`. GitHub Actions has integration ID `15368`; binding the checks to this
integration prevents an unrelated status publisher from satisfying them.

There are no bypass actors, including administrators. Approvals are intentionally
zero because a sole maintainer cannot approve their own PR. Review remains
encouraged. Force-pushes and branch deletion are blocked. Never add an unrestricted
`update` rule: it can prevent ordinary PR merges as well as direct pushes.

Create the ruleset once with:

```sh
gh api --method POST repos/luminusOS/zaptide/rulesets --input .github/main-ruleset.json
```

For an existing rule, inspect its ID and use `PUT .../rulesets/RULESET_ID` with the
same input instead of creating duplicate rules. Inspect inherited organization
rules too; repository rules cannot weaken them. Keep a copy of prior settings
before making administrative changes. After applying, read the ruleset back and
confirm enforcement is active and the parameters match the file.

Do not require path-filtered Flatpak or tag-only release jobs on all PRs; those
checks do not always run. If CI job names change, update the live required checks
and this definition together, or merges can remain blocked indefinitely.

## Merge and security settings

`repository-settings.json` records the corresponding repository-level settings:

```sh
gh api --method PUT repos/luminusOS/zaptide/vulnerability-alerts
gh api --method PATCH repos/luminusOS/zaptide --input .github/repository-settings.json
gh api --method PUT repos/luminusOS/zaptide/private-vulnerability-reporting
gh api --method PUT repos/luminusOS/zaptide/actions/permissions/workflow \
  -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false
```

Validate local configuration with Python and `PyYAML==6.0.3` using
`python .github/tests/test_repository.py`. The `repository` CI job runs these tests.

- Enable squash merging, disable merge commits/rebase merging, and automatically
  delete merged topic branches. Do not auto-merge dependency updates without review.
- Enable the dependency graph, Dependabot vulnerability alerts and security
  updates, private vulnerability reporting, secret scanning, and secret-scanning
  push protection where GitHub supports them.
- Keep the Actions default token read-only and do not allow Actions to approve PRs.
  Workflows grant write access only to publishing jobs that require it.
- Never run untrusted PR code with secrets or use `pull_request_target` to check
  out and execute the PR head. Maintain GitHub's approval gate for fork workflows.
- Keep Actions pinned to reviewed full commit SHAs. Dependabot opens update PRs;
  inspect upstream changes before merging them.

Secret-scanning push protection and branch protection address different risks:
one blocks recognized credentials, while the other enforces the PR workflow.
Administrators can still change repository settings; no ruleset removes that
administrative capability. Review settings changes rather than claiming they are
impossible.
