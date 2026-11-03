mod args;
mod azure;
mod controls;
mod display;
mod model;
mod policy;
mod watch;

#[cfg(test)]
mod tests;

use anyhow::{Result, ensure};
use clap::Parser;
use std::{
    collections::HashSet,
    io::IsTerminal,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use args::{Args, Target};
use azure::Azure;
use controls::{TerminalInput, adjust_interval};
use display::{list_stages, render_refresh_interval};
use policy::Policy;
use watch::cycle;

fn run() -> Result<()> {
    let args = Args::parse();
    let target = Target::parse(&args.url)?;
    let policy = Policy::from_args(&args);
    let api = Azure {
        target,
        timeout: Duration::from_secs(args.request_timeout),
    };
    if args.list_stages {
        return list_stages(&api, &mut std::io::stdout().lock());
    }
    let refresh_screen = !args.once && std::io::stdout().is_terminal();
    let terminal_input = TerminalInput::new(refresh_screen && std::io::stdin().is_terminal())?;
    let show_controls = refresh_screen && terminal_input.enabled();
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopped);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let session = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let mut submitted = HashSet::new();
    let mut interval = args.interval;
    while !stopped.load(Ordering::Relaxed) {
        let build = cycle(
            &api,
            &policy,
            args.dry_run,
            refresh_screen,
            interval,
            show_controls,
            &mut submitted,
            &session,
            &stopped,
        )?;
        if build.status == "completed" {
            ensure!(
                matches!(
                    build.result.as_deref(),
                    Some("succeeded" | "partiallySucceeded")
                ),
                "Build completed with result: {}",
                build.result.as_deref().unwrap_or("unknown")
            );
            return Ok(());
        }
        if args.once {
            return Ok(());
        }
        let mut next_refresh = Instant::now() + Duration::from_secs(interval);
        let mut refresh_now = false;
        while Instant::now() < next_refresh {
            if stopped.load(Ordering::Relaxed) {
                break;
            }
            if show_controls {
                for key in terminal_input.read_keys()? {
                    if key == b'r' || key == b'R' {
                        refresh_now = true;
                        break;
                    }
                    if let Some(adjusted) = adjust_interval(interval, key) {
                        interval = adjusted;
                        next_refresh = Instant::now() + Duration::from_secs(interval);
                        render_refresh_interval(interval)?;
                    }
                }
            }
            if refresh_now {
                break;
            }
            thread::sleep(
                Duration::from_millis(100)
                    .min(next_refresh.saturating_duration_since(Instant::now())),
            );
        }
        if refresh_now {
            continue;
        }
    }
    println!("Monitoring stopped.");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
