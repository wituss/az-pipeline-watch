mod args;
mod azure;
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
use display::list_stages;
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
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopped);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let session = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let mut submitted = HashSet::new();
    while !stopped.load(Ordering::Relaxed) {
        let build = cycle(
            &api,
            &policy,
            args.dry_run,
            refresh_screen,
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
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(args.interval) {
            if stopped.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
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
