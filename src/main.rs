mod args;
mod azure;
mod controls;
mod display;
mod interactive;
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
use controls::{Key, TerminalInput, adjust_interval};
use display::{list_stages, timer_line};
use interactive::{confirm_selection, prompt_for_url, select_stages, show_selection};
use policy::Policy;
use watch::cycle;

fn run() -> Result<()> {
    let watcher_started = Instant::now();
    let args = Args::parse();
    let interactive = args.url.is_none();
    let url = if let Some(url) = args.url.as_deref() {
        url.to_owned()
    } else {
        prompt_for_url()?
    };
    let target = Target::parse(&url)?;
    let mut policy = Policy::from_args(&args);
    let api = Azure {
        target,
        timeout: Duration::from_secs(args.request_timeout),
    };
    let session = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    if interactive {
        let Some(selected) = select_stages(&api)? else {
            println!("Selection cancelled.");
            return Ok(());
        };
        if args.autoapprove {
            show_selection(&selected)?;
        } else if !confirm_selection(&selected)? {
            println!("Approval cancelled.");
            return Ok(());
        }
        policy = Policy::Selected(selected);
    }
    if args.list_stages {
        return list_stages(&api, &mut std::io::stdout().lock());
    }
    let refresh_screen = !args.once && std::io::stdout().is_terminal();
    let terminal_input = TerminalInput::new(refresh_screen && std::io::stdin().is_terminal())?;
    let show_controls = refresh_screen && terminal_input.enabled();
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stopped);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let mut submitted = HashSet::new();
    let mut interval = args.interval;
    let mut last_escape: Option<Instant> = None;
    while !stopped.load(Ordering::Relaxed) {
        let (build, mut display) = cycle(
            &api,
            &policy,
            args.dry_run,
            refresh_screen,
            (!args.once).then_some(interval),
            show_controls,
            watcher_started,
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
                if let Some(key) = terminal_input.read_key(Duration::ZERO)? {
                    if key == Key::Interrupt {
                        stopped.store(true, Ordering::Relaxed);
                        break;
                    }
                    if key == Key::Escape {
                        let now = Instant::now();
                        if last_escape.is_some_and(|previous| {
                            now.duration_since(previous) <= Duration::from_secs(1)
                        }) {
                            stopped.store(true, Ordering::Relaxed);
                            break;
                        }
                        last_escape = Some(now);
                        continue;
                    }
                    last_escape = None;
                    if key == Key::Cancel {
                        if terminal_input.confirm_exit()? {
                            stopped.store(true, Ordering::Relaxed);
                            break;
                        }
                        display.render(true)?;
                        continue;
                    }
                    if matches!(key, Key::Other('r' | 'R')) {
                        refresh_now = true;
                        break;
                    }
                    if let Key::Other(key) = key {
                        if key.is_ascii() {
                            if let Some(adjusted) = adjust_interval(interval, key as u8) {
                                interval = adjusted;
                                next_refresh = Instant::now() + Duration::from_secs(interval);
                                display.controls_interval = Some(interval);
                                display.timers.clear();
                            }
                        }
                    }
                }
            }
            if refresh_screen {
                let timers = timer_line(
                    &build,
                    watcher_started.elapsed(),
                    Some(next_refresh.saturating_duration_since(Instant::now())),
                    SystemTime::now(),
                )?;
                if timers != display.timers {
                    display.timers = timers;
                    display.render(true)?;
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
    drop(terminal_input);
    println!("Monitoring stopped.");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
