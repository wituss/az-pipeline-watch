use anyhow::{Context, Result, ensure};
use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::{io::IsTerminal, time::Duration};

pub(crate) struct TerminalInput {
    enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Toggle,
    Confirm,
    Cancel,
    Other(char),
}

fn decode_key(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('c' | 'C') => Some(Key::Cancel),
            _ => None,
        };
    }
    match event.code {
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Char(' ') => Some(Key::Toggle),
        KeyCode::Enter => Some(Key::Confirm),
        KeyCode::Esc | KeyCode::Char('q' | 'Q') => Some(Key::Cancel),
        KeyCode::Char(key) => Some(Key::Other(key)),
        _ => None,
    }
}

impl TerminalInput {
    pub(crate) fn new(enabled: bool) -> Result<Self> {
        if enabled {
            ensure!(
                std::io::stdin().is_terminal(),
                "Interactive controls require a terminal on stdin"
            );
            enable_raw_mode().context("Cannot enable terminal raw mode")?;
        }
        let terminal = Self { enabled };
        if enabled {
            execute!(std::io::stdout(), Hide).context("Cannot hide terminal cursor")?;
        }
        Ok(terminal)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn read_key(&self, timeout: Duration) -> Result<Option<Key>> {
        if !self.enabled || !event::poll(timeout).context("Cannot poll terminal input")? {
            return Ok(None);
        }
        match event::read().context("Cannot read terminal input")? {
            Event::Key(key) => Ok(decode_key(key)),
            _ => Ok(None),
        }
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        if self.enabled {
            if let Err(error) = disable_raw_mode() {
                eprintln!("Warning: could not restore terminal settings: {error}");
            }
            if let Err(error) = execute!(std::io::stdout(), Show) {
                eprintln!("Warning: could not restore terminal cursor: {error}");
            }
        }
    }
}

pub(crate) fn adjust_interval(interval: u64, key: u8) -> Option<u64> {
    match key {
        b'+' | b'=' => Some(interval.saturating_sub(5).max(1)),
        b'-' => Some(interval.saturating_add(5).min(3600)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_navigation_selection_and_cancel_events() {
        for (code, expected) in [
            (KeyCode::Up, Key::Up),
            (KeyCode::Down, Key::Down),
            (KeyCode::Char(' '), Key::Toggle),
            (KeyCode::Enter, Key::Confirm),
            (KeyCode::Esc, Key::Cancel),
            (KeyCode::Char('q'), Key::Cancel),
            (KeyCode::Char('+'), Key::Other('+')),
        ] {
            assert_eq!(
                decode_key(KeyEvent::new(code, KeyModifiers::NONE)),
                Some(expected)
            );
        }
        assert_eq!(
            decode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Key::Cancel)
        );
    }

    #[test]
    fn ignores_key_releases_to_avoid_double_toggles_on_windows() {
        let event = KeyEvent::new_with_kind(
            KeyCode::Char(' '),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(decode_key(event), None);
    }
}
