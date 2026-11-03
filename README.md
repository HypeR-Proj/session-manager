<!--
SPDX-FileCopyrightText: 2026 roolrz
SPDX-License-Identifier: Apache-2.0
-->

# session-manager

Terminal and root-shell lifecycle owner.

Creates the terminal/shell task groups, delegates their capabilities and resets raw-
console epochs between sessions. Console bytes flow directly through the console and
terminal services rather than through the manager.

See the [implementation entry](src/main.rs) and [component declaration](component.json)
for the public interfaces, build dependencies and delivered artifacts.

## Boundaries

The service is launched with explicit startup authority and session configuration.
Recovery must discard stale input before starting a new shell; it is not a generic login
or POSIX session implementation.

## Build and test

Use this checkout inside the [repo-managed workspace](https://github.com/HypeR-Proj/HypeR).
From this repository directory:

```sh
make
make test
```

An absolute `OUT=/path/to/out` selects a shared output directory. The module
entry delegates to build/system, which prepares the installed SDK and declared
dependencies. Native binaries require HypeR; host module checks cover the tests
selected by component.json. A standalone clone does not contain the workspace’s
other source repositories or Native SDK.


Journal delegation completes the routed endpoint handoff within the original
connection deadline. The shell receives a durable shared query endpoint, so
idle shells retain no per-session query worker in journald.
