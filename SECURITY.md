# Security policy

## Report a vulnerability privately

Use [Report a vulnerability](https://github.com/luminusOS/zaptide/security/advisories/new)
to contact the maintainers through GitHub's private vulnerability reporting.
Do not open a public issue or PR with exploit details before coordinated disclosure.

Include the affected version or commit, installation method, impact, and minimal
reproduction steps using synthetic data. Include a proposed mitigation if known.
Never include real conversations, archives, linked-device credentials, QR codes,
pairing codes, phone numbers, keyring secrets, or third-party account data.

Test only systems and accounts you own or have explicit permission to test.
Do not access another person's messages, disrupt WhatsApp services, or run
destructive tests against a production profile.

## Supported code

Security fixes target the current `main` branch and the next applicable release.
Older releases do not have a guaranteed backport or support window. Reports
against older versions are still welcome, especially if the issue affects current
code. There is no guaranteed response deadline or bounty program.

## Disclosure and credentials

Keep details private while maintainers assess the issue and prepare a fix.
Coordinate publication through the private advisory. If a credential was exposed,
revoke or rotate it first; deleting a public comment or Git commit does not make
it safe again.

Ordinary bugs and feature requests belong in public issues after personal data
has been removed. ZapTide is an unofficial WhatsApp client; protocol behavior may
also depend on upstream libraries and WhatsApp itself.
