<!--
SPDX-FileCopyrightText: 2026 roolrz
SPDX-License-Identifier: Apache-2.0
-->

# Contributing

Use a pull request against `main`. Explain the behavior changed and the module
tests performed. Coordinated changes share a `topic:` label across repositories.
Use **Run system CI** and **Submit topic** in the PR Checks tab to verify and
submit the whole group. Requests are processed by the central Actions coordinator;
allow several minutes. The ordinary Merge button is restricted to the coordinator
App. Administrators may bypass reviews, and can explicitly force a whole-topic
submission through the central workflow. Direct pushes, force pushes and deletion
of main are prohibited for everyone. See [topic controls](https://github.com/HypeR-Proj/ci/blob/main/docs/topic-controls.md).

Preserve SPDX headers and third-party notices. Sign off commits with `git commit -s`.
