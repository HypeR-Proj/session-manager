// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Startup grants keep the terminal isolated from shell construction authority.

use crate::config::Config;
use hyper_os::capability_channel::CapabilityChannel;
use hyper_os::fs::Directory;
use hyper_os::handle::{
    ByteChannelObject, CapabilityChannelObject, OwnedHandle, ProcessObject, Rights, RightsOffer,
    TaskGroupObject,
};
use hyper_os::startup::{self, Startup};
use hyper_os::task::ProcessBuilder;
use hyper_service::{process, stdio};
use hyper_terminal::{PreparedClient, client};

type TerminalLaunch = (
    OwnedHandle<ProcessObject>,
    hyper_os::task::ProcessRetirement,
    OwnedHandle<CapabilityChannelObject>,
    OwnedHandle<ByteChannelObject>,
    OwnedHandle<ByteChannelObject>,
);

fn builder(
    startup: &Startup<'_>,
    environment: &Directory,
    group: &OwnedHandle<TaskGroupObject>,
    image: &str,
    name: &str,
) -> hyper_os::Result<ProcessBuilder> {
    let executable = environment.open(image, process::EXECUTABLE_FILE_RIGHTS)?;
    let loader = process::open_loader(environment, process::LoaderKind::Dynamic)?;
    let libraries = process::open_library_directory(environment)?;
    let builder = ProcessBuilder::create(
        startup.borrow(startup::TASK_FACTORY)?,
        group.as_handle_ref(),
        startup.borrow(startup::RESOURCE_DOMAIN)?,
        executable.as_handle_ref(),
        loader.as_handle_ref(),
    )?;
    builder.set_name(name)?;
    builder.add_argument(image)?;
    builder.add_handle_duplicate(
        libraries.as_handle_ref(),
        startup::DYNAMIC_LIBRARY_DIRECTORY.as_raw(),
        RightsOffer::Exact(Rights::READ.union(Rights::EXECUTE).union(Rights::PROTECT)),
    )?;
    Ok(builder)
}

pub(super) fn terminal(
    startup: &Startup<'_>,
    environment: &Directory,
    config: &Config,
    logs: &OwnedHandle<CapabilityChannelObject>,
    group: &OwnedHandle<TaskGroupObject>,
    binding: hyper_service::console::RawBinding,
) -> hyper_os::Result<TerminalLaunch> {
    let builder = builder(
        startup,
        environment,
        group,
        &config.terminal,
        "terminal-service",
    )?;
    let (control, server) = CapabilityChannel::create()?;
    let (ready, ready_writer) = hyper_os::channel::create_pair()?;
    let (recovery, recovery_service) = hyper_os::channel::create_pair()?;
    let (log_writer, log_reader) = hyper_os::channel::create_pair()?;
    hyper_service::console::register_log(logs.as_handle_ref(), "terminal-service", log_reader)?;
    for (handle, purpose, rights) in [
        (
            binding.input,
            client::RAW_INPUT.as_raw(),
            hyper_os::stream::CONNECTOR_RIGHTS,
        ),
        (
            binding.output,
            client::RAW_OUTPUT.as_raw(),
            hyper_os::stream::CONNECTOR_RIGHTS,
        ),
        (
            server.into_handle(),
            client::MANAGEMENT.as_raw(),
            client::CONTROL_RIGHTS,
        ),
    ] {
        builder
            .add_handle_move(handle, purpose, RightsOffer::Exact(rights))
            .map_err(|failure| failure.error())?;
    }
    builder
        .add_handle_move(
            ready_writer,
            client::READY.as_raw(),
            RightsOffer::Exact(Rights::WRITE.union(Rights::WAIT)),
        )
        .map_err(|failure| failure.error())?;
    builder
        .add_handle_move(
            log_writer,
            stdio::STANDARD_ERROR.as_raw(),
            RightsOffer::Exact(stdio::STANDARD_ERROR_CONTRACT.allowed_rights()),
        )
        .map_err(|failure| failure.error())?;
    builder
        .add_handle_move(
            recovery_service,
            client::RECOVERY.as_raw(),
            RightsOffer::Exact(Rights::READ.union(Rights::WRITE).union(Rights::WAIT)),
        )
        .map_err(|failure| failure.error())?;
    let retirement = builder.retain_on_exit(8 * 1024 * 1024, 10_000_000_000)?;
    builder.seal()?;
    let process = builder.start().map_err(|failure| failure.error())?;
    Ok((process, retirement, control.into_handle(), ready, recovery))
}

pub(super) fn shell(
    startup: &Startup<'_>,
    root: &Directory,
    environment: &Directory,
    config: &Config,
    group: &OwnedHandle<TaskGroupObject>,
    client: PreparedClient,
) -> hyper_os::Result<OwnedHandle<ProcessObject>> {
    let builder = builder(startup, environment, group, &config.shell, "shell")?;
    macro_rules! delegate {
        ($purpose:expr, $contract:expr) => {
            builder.add_handle_duplicate(
                startup.borrow($purpose)?,
                $purpose.as_raw(),
                RightsOffer::Exact($contract.allowed_rights()),
            )?;
        };
    }
    builder.add_handle_duplicate(
        root.as_handle_ref(),
        startup::ROOT_DIRECTORY.as_raw(),
        RightsOffer::Exact(process::SHELL_ROOT_DIRECTORY_CONTRACT.allowed_rights()),
    )?;
    delegate!(startup::TASK_FACTORY, process::SHELL_TASK_FACTORY_CONTRACT);
    builder.add_handle_duplicate(
        group.as_handle_ref(),
        startup::TASK_GROUP.as_raw(),
        RightsOffer::Exact(process::SHELL_TASK_GROUP_CONTRACT.allowed_rights()),
    )?;
    delegate!(
        startup::RESOURCE_DOMAIN,
        process::SHELL_RESOURCE_DOMAIN_CONTRACT
    );
    let loader = process::open_loader(environment, process::LoaderKind::Dynamic)?;
    let libraries = process::open_library_directory(environment)?;
    let journal = hyper_component::connection::connect(
        startup.borrow(hyper_component::CONNECTOR)?,
        "journal",
        hyper_service_ipc::deadline()?,
    )?;
    process::ShellDelegation {
        environment: environment.as_handle_ref(),
        loader: loader.as_handle_ref(),
        libraries: libraries.as_handle_ref(),
        tasks: startup.borrow(startup::TASK_INSPECTOR)?,
        objects: startup.borrow(startup::OBJECT_INSPECTOR)?,
        memory: startup.borrow(startup::MEMORY_INSPECTOR)?,
        cpus: startup.borrow(startup::CPU_INSPECTOR)?,
        services: startup.borrow(hyper_component::OBSERVE).ok(),
        service_control: startup.borrow(hyper_component::CONTROL).ok(),
        journal: Some(journal.as_handle_ref()),
        filesystem: startup
            .borrow(hyper_service::filesystem::CONTROL_CLIENT)
            .ok(),
        virtual_machines: startup.borrow(hyper_service::vm::MANAGER_CONNECTION).ok(),
    }
    .add_to(&builder)?;
    finish_interactive(builder, client)
}

fn finish_interactive(
    builder: ProcessBuilder,
    client: PreparedClient,
) -> hyper_os::Result<OwnedHandle<ProcessObject>> {
    for purpose in [
        hyper_os::stream::STANDARD_INPUT.as_raw(),
        hyper_os::stream::STANDARD_OUTPUT.as_raw(),
        hyper_os::stream::STANDARD_ERROR.as_raw(),
    ] {
        builder.add_handle_duplicate(
            client.stream.as_handle_ref(),
            purpose,
            RightsOffer::Exact(hyper_os::stream::CONNECTOR_RIGHTS),
        )?;
    }
    builder
        .add_handle_move(
            client.control,
            client::CONTROL.as_raw(),
            RightsOffer::Exact(client::CONTROL_RIGHTS),
        )
        .map_err(|failure| failure.error())?;
    builder
        .add_handle_move(
            client.events,
            client::EVENTS.as_raw(),
            RightsOffer::Exact(client::EVENT_RIGHTS),
        )
        .map_err(|failure| failure.error())?;
    builder.seal()?;
    builder.start().map_err(|failure| failure.error())
}
