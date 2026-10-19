//! `macwin`: the macwin command-line agent.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::process::ExitCode;

use anyhow::anyhow;
use clap::Parser;
use macwin_agent::cli::{Cli, Command, ConfigCmd, Probe, Spike};
use macwin_agent::{autostart, commands, logging};

fn main() -> ExitCode {
    // Finder may pass a process serial number (-psn_...) to an app bundle.
    let args: Vec<OsString> = std::env::args_os()
        .filter(|a| !a.to_string_lossy().starts_with("-psn_"))
        .collect();
    let result = if args.len() == 1 && launched_as_app() {
        commands::tray(false)
    } else {
        match Cli::parse_from(args).command {
            // The app runs its UI on this thread, outside any runtime.
            Command::Tray { background } => commands::tray(background),
            command => {
                logging::init();
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)
                    .and_then(|rt| rt.block_on(run(command)))
            }
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", render(&e));
            ExitCode::FAILURE
        }
    }
}

/// MacWin.app opened from Finder, the Dock or `open`, without arguments.
fn launched_as_app() -> bool {
    std::env::current_exe().is_ok_and(|exe| autostart::app_bundle(&exe).is_some())
}

async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Tray { .. } => Err(anyhow!("the app does not run inside the async runtime")),
        Command::Identity => commands::identity(),
        Command::Pair(args) => commands::pair(args).await,
        Command::Unpair => commands::unpair(),
        Command::Config(ConfigCmd::Show) => commands::config_show(),
        Command::Config(ConfigCmd::Set { key, value }) => commands::config_set(&key, &value),
        Command::Run => commands::run().await,
        Command::Autostart { action } => commands::autostart(action),
        Command::Probe(Probe::Screens) => commands::probe_screens(),
        Command::Probe(Probe::Permissions { prompt }) => {
            commands::probe_permissions(prompt);
            Ok(())
        }
        Command::Probe(Probe::Clipboard { watch }) => commands::probe_clipboard(watch).await,
        Command::Probe(Probe::Latency { seconds }) => commands::probe_latency(seconds).await,
        Command::Spike(Spike::Net(args)) => commands::spike_net(args).await,
        Command::Spike(Spike::Capture(args)) => commands::spike_capture(args).await,
        Command::Spike(Spike::Inject(_)) => commands::spike_inject().await,
        Command::Spike(Spike::Kvm(args)) => commands::spike_kvm(args).await,
    }
}

/// The error chain as one line, skipping causes whose text the previous
/// message already includes (our error types embed their source).
fn render(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        let msg = cause.to_string();
        if out.contains(&msg) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&msg);
    }
    out
}
