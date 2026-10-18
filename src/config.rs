// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Session policy is owned by the manager, independently of component routing.

use serde::Deserialize;
use std::io::Read;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) terminal: String,
    pub(crate) shell: String,
    pub(crate) restart_limit: u32,
    /// Let disconnected applications finish or save before stopping the old
    /// session. This never reconnects their terminal to the replacement epoch.
    pub(crate) failure_grace_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            terminal: "svc/terminal-service".into(),
            shell: "bin/sh".into(),
            restart_limit: 3,
            failure_grace_ms: 2000,
        }
    }
}

impl Config {
    pub(crate) fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let file = match std::fs::File::open("/etc/hyper/session.json") {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err("session configuration exceeds 4096 bytes".into());
        }
        let config: Self = serde_json::from_slice(&bytes)?;
        for image in [&config.terminal, &config.shell] {
            if image.is_empty()
                || image.starts_with('/')
                || image.as_bytes().contains(&0)
                || image.split('/').any(|part| matches!(part, "" | "." | ".."))
            {
                return Err("session images must be relative to the system environment".into());
            }
        }
        if config.restart_limit > 10 {
            return Err("session restart limit exceeds 10 attempts".into());
        }
        if config.failure_grace_ms > 10_000 {
            return Err("session failure grace exceeds 10000 ms".into());
        }
        Ok(config)
    }
}
