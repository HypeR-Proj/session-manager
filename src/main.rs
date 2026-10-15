// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Owns terminal and root-shell lifetimes; console bytes never pass through it.

mod config;
mod launch;

use hyper_os::handle::{OwnedHandle, ProcessObject, TaskGroupObject};
use hyper_os::startup::{self, Startup};
use hyper_os::task::create_task_group;
use hyper_os::wait::{ObjectSignals, WaitItem, wait_many};
use hyper_terminal::Terminal;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

fn application_main(mut startup: Startup<'_>) -> ExitCode {
    match run(&mut startup) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("HypeR session-manager: failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(startup: &mut Startup<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let mut session_start = startup.take_optional(hyper_service::session::START)?;
    let config = config::Config::load()?;
    let root = startup.take_root_directory()?;
    let environment = hyper_os::fs::Directory::from_handle(
        startup.take(hyper_service::process::ENVIRONMENT_DIRECTORY)?,
    );
    let controller = startup.take(hyper_service::session::CONSOLE_CONTROLLER)?;
    let logs = startup.take(hyper_service::session::LOG_CONNECTOR)?;
    let mut epoch = 0_u64;
    let mut failures = 0;
    let recovery_start = Arc::new(Mutex::new(RecoveryProfile::default()));
    loop {
        epoch = epoch.checked_add(1).ok_or("session epoch exhausted")?;
        // Reset includes a cutoff in the kernel RX FIFO, not merely userspace
        // buffers, so stale editor input cannot become a new shell command.
        hyper_service::console::reset_raw(controller.as_handle_ref(), epoch)?;
        let binding = hyper_service::console::attach_raw(controller.as_handle_ref(), epoch)?;
        if epoch != 1 {
            let mut output = hyper_os::stream::connect(
                binding.output.as_handle_ref(),
                hyper_os::stream::ACCESS_WRITE,
            )?;
            if let Some(start) = recovery_start
                .lock()
                .map_err(|_| "recovery profile poisoned")?
                .start
            {
                output.write_all(&[start])?;
            }
            output.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l\r\n")?;
            output.fence()?;
        }
        let group = create_task_group(
            startup.borrow(startup::TASK_FACTORY)?,
            startup.borrow(startup::RESOURCE_DOMAIN)?,
        )?;
        let terminal_group = create_task_group(
            startup.borrow(startup::TASK_FACTORY)?,
            startup.borrow(startup::RESOURCE_DOMAIN)?,
        )?;
        let began = hyper_os::time::monotonic_now()?.as_nanoseconds();
        let mut recovery_worker = None;
        let services = SessionServices {
            startup,
            root: &root,
            environment: &environment,
            config: &config,
            epoch,
            logs: &logs,
        };
        let result = run_session(
            &services,
            &group,
            &terminal_group,
            binding,
            &recovery_start,
            &mut recovery_worker,
            &mut session_start,
        );
        let reset = hyper_service::console::reset_raw(controller.as_handle_ref(), epoch);
        stop_groups([&group, &terminal_group])?;
        if let Some(worker) = recovery_worker {
            let _ = worker.join();
        }
        reset?;
        if session_start.is_some() {
            // A failed startup gate must never be bypassed by a fresh epoch.
            return result;
        }
        let elapsed = hyper_os::time::monotonic_now()?
            .as_nanoseconds()
            .saturating_sub(began);
        if elapsed >= 10_000_000_000 {
            failures = 0;
        } else {
            failures += 1;
        }
        match result {
            Ok(()) if failures > config.restart_limit => {
                return Err("root shell repeatedly exits during startup".into());
            }
            Ok(()) => {}
            Err(error) => {
                eprintln!("HypeR session-manager: session ended: {error}");
                if failures > config.restart_limit {
                    return Err(error);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Capabilities and image policy retained across terminal session epochs.
struct SessionServices<'a, 'startup> {
    epoch: u64,
    startup: &'a Startup<'startup>,
    root: &'a hyper_os::fs::Directory,
    environment: &'a hyper_os::fs::Directory,
    config: &'a config::Config,
    logs: &'a OwnedHandle<hyper_os::handle::CapabilityChannelObject>,
}

fn run_session(
    services: &SessionServices<'_, '_>,
    group: &OwnedHandle<TaskGroupObject>,
    terminal_group: &OwnedHandle<TaskGroupObject>,
    binding: hyper_service::console::RawBinding,
    recovery_start: &Arc<Mutex<RecoveryProfile>>,
    recovery_worker: &mut Option<std::thread::JoinHandle<()>>,
    session_start: &mut Option<OwnedHandle<hyper_os::handle::ByteChannelObject>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let SessionServices {
        startup,
        root,
        environment,
        config,
        logs,
        epoch,
    } = services;
    let epoch = *epoch;
    let (terminal_process, control, ready, recovery) =
        launch::terminal(startup, environment, config, logs, terminal_group, binding)?;
    let retained = Arc::clone(recovery_start);
    *recovery_worker = Some(
        std::thread::Builder::new()
            .name("terminal-recovery".into())
            .spawn(
                move || {
                    while accept_recovery(&recovery, &retained, epoch).is_ok() {}
                },
            )?,
    );
    let deadline = hyper_os::time::deadline_after(std::time::Duration::from_secs(10))?.as_raw();
    let observation = wait_many(
        &[
            WaitItem::new(
                terminal_process.as_handle_ref(),
                ObjectSignals::<ProcessObject>::TERMINATED,
            ),
            WaitItem::new(
                ready.as_handle_ref(),
                ObjectSignals::<hyper_os::handle::ByteChannelObject>::READABLE
                    .union(ObjectSignals::<hyper_os::handle::ByteChannelObject>::PEER_CLOSED),
            ),
        ],
        deadline,
    )?
    .index;
    if observation == 0 {
        return Err("terminal exited during startup".into());
    }
    let mut bytes = [0; 32];
    let count = ready.as_byte_channel().receive(&mut bytes)?;
    if &bytes[..count] != b"terminal-ready\n" {
        return Err("invalid terminal readiness reply".into());
    }
    let mut terminal = Terminal::from_handle(control);
    // Readiness belongs to the initial migration transaction. Its receiver exits
    // once bootstrap retires; later terminal epochs must not depend on it.
    if epoch == 1 {
        hyper_component::notify(startup, &hyper_component::Notification::Ready)?;
    }
    if let Some(start) = session_start.as_ref() {
        // The coordinator releases only after ending the direct display and
        // attempting predecessor retirement. No terminal FENCE is fabricated.
        let observation = wait_many(
            &[
                WaitItem::new(
                    terminal_process.as_handle_ref(),
                    ObjectSignals::<ProcessObject>::TERMINATED,
                ),
                WaitItem::new(
                    start.as_handle_ref(),
                    ObjectSignals::<hyper_os::handle::ByteChannelObject>::READABLE
                        .union(ObjectSignals::<hyper_os::handle::ByteChannelObject>::PEER_CLOSED),
                ),
            ],
            hyper_os::time::deadline_after(std::time::Duration::from_secs(120))?.as_raw(),
        )?;
        if observation.index == 0 {
            return Err("terminal exited before session release".into());
        }
        let mut bytes = [0; 64];
        let n = start.as_byte_channel().try_receive(&mut bytes)?;
        if &bytes[..n] != hyper_service::session::START_MESSAGE {
            return Err("invalid session release".into());
        }
        session_start.take();
    }
    let client = terminal.prepare_child()?;
    let id = client.id;
    let shell = launch::shell(startup, root, environment, config, group, client)?;
    terminal.bind_child(id, shell.as_process_supervisor().observe()?)?;
    let observation = wait_many(
        &[
            WaitItem::new(
                terminal_process.as_handle_ref(),
                ObjectSignals::<ProcessObject>::TERMINATED,
            ),
            WaitItem::new(
                shell.as_handle_ref(),
                ObjectSignals::<ProcessObject>::TERMINATED,
            ),
        ],
        hyper_os::DEADLINE_INFINITE,
    )?;
    if observation.index == 0 {
        return Err("terminal service exited".into());
    }
    terminal.release_child(id)?;
    use hyper_os::task::ProcessTermination;
    match shell.as_process_supervisor().info()?.terminal {
        Some(
            ProcessTermination::ProcessExited { status: 0 }
            | ProcessTermination::ThreadExited { status: 0 }
            | ProcessTermination::LastThreadExited { status: 0 },
        ) => Ok(()),
        _ => Err("root shell failed".into()),
    }
}

fn stop_groups(groups: [&OwnedHandle<TaskGroupObject>; 2]) -> hyper_os::Result<()> {
    let mut failure = None;
    // Close both admission paths before waiting: a delayed shell retirement
    // must not leave the old terminal alive through its own group capability.
    for group in groups {
        if let Err(error) = group.as_task_group().request_stop() {
            failure.get_or_insert(error);
        }
    }
    let deadline = hyper_os::time::deadline_after(std::time::Duration::from_secs(5))?.as_raw();
    for group in groups {
        if let Err(error) = group.as_task_group().wait_terminated(deadline) {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

fn main() -> ExitCode {
    match hyper_rt::process::startup() {
        Ok(startup) => application_main(startup),
        Err(error) => {
            eprintln!("session-manager: startup: {error:?}");
            ExitCode::FAILURE
        }
    }
}

fn accept_recovery(
    channel: &OwnedHandle<hyper_os::handle::ByteChannelObject>,
    retained: &Mutex<RecoveryProfile>,
    epoch: u64,
) -> hyper_os::Result<()> {
    let mut bytes = [0; 16];
    let count = channel.as_byte_channel().receive(&mut bytes)?;
    let start = u16::from_le_bytes([bytes[8], bytes[9]]);
    if count != 16 || start > 256 || bytes[10..].iter().any(|value| *value != 0) {
        return Err(hyper_os::Error::InvalidResponse);
    }
    let generation = u64::from_le_bytes(
        bytes[..8]
            .try_into()
            .map_err(|_| hyper_os::Error::InvalidResponse)?,
    );
    let start = u8::try_from(start).ok();
    {
        let mut profile = retained
            .lock()
            .map_err(|_| hyper_os::Error::InvalidResponse)?;
        if generation == 0
            || (profile.epoch == epoch
                && (generation < profile.generation
                    || (generation == profile.generation && start != profile.start)))
        {
            return Err(hyper_os::Error::InvalidResponse);
        }
        *profile = RecoveryProfile {
            epoch,
            generation,
            start,
        };
    }
    channel.as_byte_channel().send(&bytes)
}

#[derive(Default)]
struct RecoveryProfile {
    epoch: u64,
    generation: u64,
    start: Option<u8>,
}
