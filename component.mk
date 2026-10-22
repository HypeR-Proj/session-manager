# SPDX-FileCopyrightText: 2026 roolrz
# SPDX-License-Identifier: Apache-2.0

$(eval $(call native-rust-app,svc/session-manager,hyper-session-manager,session-manager,svc/session-manager,development,std))
