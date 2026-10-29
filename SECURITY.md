<!--
SPDX-FileCopyrightText: 2026 roolrz
SPDX-License-Identifier: Apache-2.0
-->

# Security Policy

## Project status

HypeR is an experimental hypervisor kernel under active development. It is not
currently suitable for production systems, untrusted guest workloads, or
environments that depend on stable isolation, compatibility, or data-integrity
guarantees.

Security reports are nevertheless important. Early disclosure gives the
project an opportunity to correct unsafe interfaces, architecture contracts,
guest-isolation defects, and supply-chain problems before they become stable
dependencies for users.

## Supported versions

HypeR does not yet publish supported releases or provide a security maintenance
window. Security fixes are developed against the current `main` branch on a
best-effort basis.

| Version | Security support |
| --- | --- |
| Current `main` branch | Best effort |
| Tags, forks, and downstream builds | Not supported |

This policy will be revised before HypeR declares its first supported release.

## Reporting a vulnerability

Do not report an undisclosed vulnerability in a public issue, pull request,
discussion, or commit.

Use GitHub's
[private vulnerability reporting](https://github.com/HypeR-Proj/session-manager/security/advisories/new)
to send the report to the repository maintainers. Private vulnerability
reporting must be enabled in the repository settings before the repository is
made public.

Please include, when available:

- the affected architecture, configuration, and revision;
- the security boundary or invariant that is violated;
- steps or a minimal reproducer;
- the expected and observed behavior;
- the likely impact and required attacker capabilities;
- whether the issue has been disclosed anywhere else; and
- any proposed fix or mitigation.

Reports about memory safety, host or guest privilege boundaries, stage-1 or
stage-2 translation, interrupt and vCPU isolation, firmware input parsing,
unsafe API contracts, crash-console exposure, or build and guest-artifact
integrity are in scope. Ordinary bugs without a confidentiality, integrity, or
availability impact may be filed in the public issue tracker.

## Response and disclosure

The maintainers will handle reports on a best-effort basis and do not currently
promise a response or remediation service-level agreement. The intended
process is to:

1. acknowledge and privately triage the report;
2. identify affected revisions and practical mitigations;
3. develop and validate a fix across the affected architecture contracts;
4. coordinate a disclosure date with the reporter; and
5. publish a GitHub Security Advisory when the impact warrants one.

Please allow time for coordinated remediation before publishing technical
details. If a report is out of scope or cannot be reproduced, the maintainers
will explain that conclusion through the private advisory.

## Research expectations

Good-faith research must use systems and workloads the researcher owns or is
authorized to test. Do not access third-party data, degrade third-party
services, or retain sensitive information beyond what is necessary to
demonstrate the issue. A minimal proof of concept is preferred over destructive
exploitation.

This policy does not create a warranty, support commitment, or authorization to
test systems without their owner's permission. The Apache License 2.0 warranty
and liability terms continue to apply.
