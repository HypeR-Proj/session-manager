# SPDX-FileCopyrightText: 2026 roolrz
# SPDX-License-Identifier: Apache-2.0

# Build this module within a repo workspace; all generated files belong to OUT.
HYPER_BUILD_SYSTEM ?= $(shell python3 -c 'from pathlib import Path; p=Path.cwd(); print(next((str((r/"project.py").resolve().parent) for r in (p,*p.parents) if (r/"project.py").is_file() and ((r/"project.py").resolve().parent/"mk/entry.mk").is_file()), ""))')
ifeq ($(strip $(HYPER_BUILD_SYSTEM)),)
$(error Set HYPER_BUILD_SYSTEM to the build-tools checkout or initialize a repo workspace)
endif
include $(HYPER_BUILD_SYSTEM)/mk/entry.mk
