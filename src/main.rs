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
        let terminal_failed = std::cell::Cell::new(false);
        let services = SessionServices {
            startup,
            root: &root,
            environment: &environment,
            config: &config,
            epoch,
            logs: &logs,
            terminal_failed: &terminal_failed,
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
        if let Err(error) = &result {
            // Report before reset/join: either operation can depend on the
            // service that just failed. The manager supplies Console fallback.
            if let Ok(connector) = startup.borrow(hyper_component::CONNECTOR) {
                let _ = hyper_component::connection::report_failure(
                    connector,
                    &format!("session epoch={epoch}: {error}"),
                );
            }
        }
        if terminal_failed.get() {
            // Closing terminal-service's endpoints wakes existing app I/O with
            // PEER_CLOSED. Give applications a bounded opportunity to save and
            // exit before closing the entire old session task group. In
            // particular, never wire an old editor to a new shell's byte stream.
            let deadline = hyper_os::time::deadline_after(std::time::Duration::from_millis(
                config.failure_grace_ms,
            ))?
            .as_raw();
            match group.as_task_group().wait_terminated(deadline) {
                Ok(()) | Err(hyper_os::Error::Status(hyper_os::Status::TIMED_OUT)) => {}
                Err(error) => return Err(error.into()),
            }
        }
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
    terminal_failed: &'a std::cell::Cell<bool>,
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
        terminal_failed,
    } = services;
    let epoch = *epoch;
    let (terminal_process, retirement, control, ready, recovery) =
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
        return Err(terminal_failure(
            &terminal_process,
            &retirement,
            "terminal-service exited during startup",
        )
        .into());
    }
    let mut bytes = [0; 32];
    let count = match ready.as_byte_channel().try_receive(&mut bytes) {
        Ok(count) => count,
        Err(hyper_os::Error::Status(hyper_os::Status::PEER_CLOSED)) => {
            // Exit withdraws active handles before publishing TERMINATED.
            // Readiness can therefore close first; still collect the child's
            // termination reason instead of losing it behind a channel error.
            return Err(terminal_failure(
                &terminal_process,
                &retirement,
                "terminal-service closed readiness during startup",
            )
            .into());
        }
        Err(error) => return Err(error.into()),
    };
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
            return Err(terminal_failure(
                &terminal_process,
                &retirement,
                "terminal-service exited before session release",
            )
            .into());
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
    let waits = [
        WaitItem::new(
            terminal_process.as_handle_ref(),
            ObjectSignals::<ProcessObject>::TERMINATED,
        ),
        WaitItem::new(
            shell.as_handle_ref(),
            ObjectSignals::<ProcessObject>::TERMINATED,
        ),
    ];
    #[cfg(feature = "test-service-failure")]
    let mut shell_first = false;
    let exited = loop {
        #[cfg(feature = "test-service-failure")]
        let deadline = {
            // Only the isolated failure fixture can request this injection.
            // Keep the linear terminal supervisor in its normal owner.
            if epoch == 1
                && let Ok(request) = std::fs::read_to_string("/tmp/terminal-fault-request")
            {
                std::fs::remove_file("/tmp/terminal-fault-request")?;
                terminal_process.as_process_supervisor().request_stop()?;
                if request == "shell-first" {
                    // Force the shell-wins observation, leaving its child app
                    // alive to prove the grace period applies in this order.
                    shell_first = true;
                    shell.as_process_supervisor().request_stop()?;
                }
            }
            hyper_os::time::deadline_after(std::time::Duration::from_millis(50))?.as_raw()
        };
        #[cfg(not(feature = "test-service-failure"))]
        let deadline = hyper_os::DEADLINE_INFINITE;
        #[cfg(feature = "test-service-failure")]
        let first = usize::from(shell_first);
        #[cfg(not(feature = "test-service-failure"))]
        let first = 0;
        match wait_many(&waits[first..], deadline) {
            Ok(observation) => break first + observation.index,
            Err(hyper_os::Error::Status(hyper_os::Status::TIMED_OUT)) => continue,
            Err(error) => return Err(error.into()),
        }
    };
    // Endpoint closure can make the shell exit while the terminal is still
    // Stopping. The latched reason precedes closure; TERMINATED need not have
    // been published yet, so the wait winner alone cannot select cleanup.
    if exited == 0
        || terminal_process
            .as_process_supervisor()
            .info()?
            .terminal
            .is_some()
    {
        terminal_failed.set(true);
        return Err(
            terminal_failure(&terminal_process, &retirement, "terminal-service exited").into(),
        );
    }
    match terminal.release_child(id) {
        Ok(()) => {}
        Err(hyper_os::Error::Status(hyper_os::Status::PEER_CLOSED)) => {
            // Exit can also start after the lifecycle snapshot above. A lost
            // management endpoint still requires the disconnected-app grace.
            terminal_failed.set(true);
            return Err(terminal_failure(
                &terminal_process,
                &retirement,
                "terminal-service closed control while releasing shell",
            )
            .into());
        }
        Err(error) => return Err(error.into()),
    }
    use hyper_os::task::ProcessTermination;
    match shell.as_process_supervisor().info()?.terminal {
        Some(
            ProcessTermination::ProcessExited { status: 0 }
            | ProcessTermination::ThreadExited { status: 0 }
            | ProcessTermination::LastThreadExited { status: 0 },
        ) => Ok(()),
        _ => Err(process_failure(&shell, "root shell failed").into()),
    }
}

fn terminal_failure(
    process: &OwnedHandle<ProcessObject>,
    retirement: &hyper_os::task::ProcessRetirement,
    context: &str,
) -> String {
    let retained = hyper_os::time::deadline_after(std::time::Duration::from_secs(2))
        .and_then(|deadline| retirement.wait_ready(deadline.as_raw()));
    let detail = match retained {
        Ok(retained) => format!(
            "{}; fault={} pc=0x{:x} address=0x{:x} retained={} bytes",
            process_failure(process, context),
            retained.fault_class,
            retained.fault_pc,
            retained.fault_address,
            retained.retained_bytes,
        ),
        Err(error) => format!(
            "{}; postmortem unavailable: {error:?}",
            process_failure(process, context)
        ),
    };
    // Observation is complete before permitting address-space reclamation.
    // Endpoints have already closed; this does not prolong the failed session.
    match retirement.release() {
        Ok(()) => detail,
        Err(error) => format!("{detail}; retirement release failed: {error:?}"),
    }
}

fn process_failure(process: &OwnedHandle<ProcessObject>, context: &str) -> String {
    let koid = process.basic_info().map(|info| info.koid.get());
    let reason = process
        .as_process_supervisor()
        .info()
        .map(|info| info.terminal);
    format!("{context}; process={koid:?}; reason={reason:?}")
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
