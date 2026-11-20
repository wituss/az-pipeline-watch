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
use display::{WatchDisplay, format_duration, list_stages, timer_line};
use interactive::{confirm_selection, prompt_for_url, select_stages, show_selection};
use policy::Policy;
use watch::cycle;

struct MonitorUi {
    build: Option<model::Build>,
    display: WatchDisplay,
    interval: u64,
    last_escape: Option<Instant>,
}

impl MonitorUi {
    fn tick(
        &mut self,
        terminal: &TerminalInput,
        stopped: &AtomicBool,
        started: Instant,
        next_refresh: &mut Option<Instant>,
        force_render: bool,
    ) -> Result<()> {
        let mut changed = force_render;
        if terminal.enabled() && !stopped.load(Ordering::Relaxed) {
            if let Some(key) = terminal.read_key(Duration::ZERO)? {
                changed = true;
                if self.display.exit_pending {
                    self.last_escape = None;
                    match key {
                        Key::Other('y' | 'Y') | Key::Interrupt => {
                            stopped.store(true, Ordering::Relaxed);
                            self.display.exit_pending = false;
                        }
                        Key::Other('n' | 'N') | Key::Confirm | Key::Cancel | Key::Escape => {
                            self.display.exit_pending = false
                        }
                        _ => {}
                    }
                } else {
                    match key {
                        Key::Interrupt => stopped.store(true, Ordering::Relaxed),
                        Key::Escape => {
                            let now = Instant::now();
                            if self.last_escape.is_some_and(|previous| {
                                now.duration_since(previous) <= Duration::from_secs(1)
                            }) {
                                stopped.store(true, Ordering::Relaxed);
                            } else {
                                self.last_escape = Some(now);
                            }
                        }
                        key => {
                            self.last_escape = None;
                            match key {
                                Key::Cancel => self.display.exit_pending = true,
                                Key::Other('r' | 'R') => {
                                    if let Some(deadline) = next_refresh {
                                        *deadline = Instant::now();
                                    }
                                }
                                Key::Other(key) if key.is_ascii() => {
                                    if let Some(interval) =
                                        adjust_interval(self.interval, key as u8)
                                    {
                                        self.interval = interval;
                                        if let Some(deadline) = next_refresh {
                                            *deadline =
                                                Instant::now() + Duration::from_secs(interval);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        let remaining = next_refresh
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::ZERO);
        let timers = if let Some(build) = &self.build {
            timer_line(build, started.elapsed(), Some(remaining), SystemTime::now())?
        } else {
            format!(
                "Watcher: {} | Next refresh: now | Pipeline: -",
                format_duration(started.elapsed())
            )
        };
        let escape_pending = self
            .last_escape
            .is_some_and(|previous| previous.elapsed() <= Duration::from_secs(1));
        let stopping = stopped.load(Ordering::Relaxed);
        changed |= timers != self.display.timers
            || escape_pending != self.display.escape_pending
            || stopping != self.display.stopping;
        self.display.timers = timers;
        self.display.escape_pending = escape_pending;
        self.display.stopping = stopping;
        self.display.controls_interval = terminal.enabled().then_some(self.interval);
        if changed {
            self.display.render(true)?;
        }
        Ok(())
    }
}

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
    let stopped = Arc::new(AtomicBool::new(false));
    let api = Azure {
        target,
        timeout: Duration::from_secs(args.request_timeout),
        stopped: Arc::clone(&stopped),
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
    let signal = Arc::clone(&stopped);
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let mut submitted = HashSet::new();
    let mut ui = MonitorUi {
        build: None,
        display: WatchDisplay {
            info: "Loading pipeline data...\n".into(),
            pipeline: String::new(),
            timers: String::new(),
            controls_interval: None,
            escape_pending: false,
            exit_pending: false,
            stopping: false,
        },
        interval: args.interval,
        last_escape: None,
    };
    while !stopped.load(Ordering::Relaxed) {
        let interval = ui.interval;
        let (build, mut display) = thread::scope(|scope| -> Result<_> {
            let worker = scope.spawn(|| {
                cycle(
                    &api,
                    &policy,
                    args.dry_run,
                    (!args.once).then_some(interval),
                    show_controls,
                    watcher_started,
                    &mut submitted,
                    &session,
                    &stopped,
                )
            });
            let ui_result = (|| -> Result<()> {
                if refresh_screen {
                    ui.tick(&terminal_input, &stopped, watcher_started, &mut None, true)?;
                }
                while !worker.is_finished() {
                    if refresh_screen {
                        ui.tick(&terminal_input, &stopped, watcher_started, &mut None, false)?;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(())
            })();
            if ui_result.is_err() {
                stopped.store(true, Ordering::Relaxed);
            }
            let result = worker
                .join()
                .map_err(|_| anyhow::anyhow!("Refresh worker panicked"))?;
            // Always collect worker errors, including uncertain approval writes on exit.
            let result = result?;
            ui_result?;
            Ok(result)
        })?;
        display.controls_interval = show_controls.then_some(ui.interval);
        display.exit_pending = ui.display.exit_pending;
        display.escape_pending = ui.display.escape_pending;
        display.stopping = stopped.load(Ordering::Relaxed);
        display.timers = timer_line(
            &build,
            watcher_started.elapsed(),
            (!args.once).then_some(Duration::from_secs(ui.interval)),
            SystemTime::now(),
        )?;
        display.render(refresh_screen)?;
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
        ui.build = Some(build);
        ui.display = display;
        let mut next_refresh = Some(Instant::now() + Duration::from_secs(ui.interval));
        while next_refresh.is_some_and(|deadline| Instant::now() < deadline) {
            if stopped.load(Ordering::Relaxed) {
                break;
            }
            if refresh_screen {
                ui.tick(
                    &terminal_input,
                    &stopped,
                    watcher_started,
                    &mut next_refresh,
                    false,
                )?;
            }
            thread::sleep(
                Duration::from_millis(100).min(
                    next_refresh
                        .expect("waiting has a deadline")
                        .saturating_duration_since(Instant::now()),
                ),
            );
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
