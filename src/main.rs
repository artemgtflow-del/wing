mod arp;
mod cli;
mod commands;
mod config;
mod http;
mod i18n;
mod index;
mod install;
mod manifest;
mod paths;
mod resolve;
mod state;
mod sysutil;
mod vercmp;
mod verify;

use clap::Parser;

/// A closed pipe is not an error: `wing search foo | head` must exit quietly.
fn is_broken_pipe(info: &std::panic::PanicHookInfo<'_>) -> bool {
    let msg = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_default();
    let msg = msg.to_lowercase();
    msg.contains("failed printing to stdout")
        || msg.contains("failed printing to stderr")
        || msg.contains("broken pipe")
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        if is_broken_pipe(info) {
            std::process::exit(0);
        }
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "internal error".to_string());
        eprintln!("error: {msg}");
        std::process::exit(1);
    }));

    let cli = cli::Cli::parse();
    if let Err(e) = commands::run(cli) {
        // `run` already reports command errors in the active locale; reaching
        // here means the failure happened before a locale was known.
        eprintln!("wing: {e:#}");
        std::process::exit(1);
    }
}
