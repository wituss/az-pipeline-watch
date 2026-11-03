use anyhow::{Context, Result, ensure};
use std::collections::VecDeque;
use std::{
    io::{IsTerminal, Read},
    process::{Command, Stdio},
};

pub(crate) struct TerminalInput {
    original_settings: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Toggle,
    Confirm,
    Cancel,
    Other(u8),
}

pub(crate) fn next_key(input: &mut VecDeque<u8>) -> Option<Key> {
    let first = *input.front()?;
    if first == 0x1b {
        if input.len() < 3 {
            return None;
        }
        input.pop_front();
        if input.pop_front() != Some(b'[') {
            return Some(Key::Other(0x1b));
        }
        return match input.pop_front() {
            Some(b'A') => Some(Key::Up),
            Some(b'B') => Some(Key::Down),
            Some(_) => Some(Key::Other(0x1b)),
            None => None,
        };
    }
    input.pop_front();
    match first {
        b' ' => Some(Key::Toggle),
        b'\r' | b'\n' => Some(Key::Confirm),
        b'q' | b'Q' => Some(Key::Cancel),
        key => Some(Key::Other(key)),
    }
}

impl TerminalInput {
    pub(crate) fn new(enabled: bool) -> Result<Self> {
        if !enabled {
            return Ok(Self {
                original_settings: None,
            });
        }

        ensure!(
            std::io::stdin().is_terminal(),
            "Interactive controls require a terminal on stdin"
        );
        let output = Command::new("stty")
            .arg("-g")
            .stdout(Stdio::piped())
            .output()
            .context("Cannot read terminal settings using stty")?;
        ensure!(
            output.status.success(),
            "stty -g failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let original_settings = String::from_utf8(output.stdout)
            .context("stty returned invalid terminal settings")?
            .trim()
            .to_owned();
        ensure!(
            !original_settings.is_empty(),
            "stty returned empty settings"
        );

        let status = Command::new("stty")
            .args(["-icanon", "-echo", "min", "0", "time", "0"])
            .status()
            .context("Cannot enable interactive keyboard input using stty")?;
        ensure!(
            status.success(),
            "stty could not enable interactive keyboard input"
        );
        Ok(Self {
            original_settings: Some(original_settings),
        })
    }

    pub(crate) fn enabled(&self) -> bool {
        self.original_settings.is_some()
    }

    pub(crate) fn read_keys(&self) -> Result<Vec<u8>> {
        if !self.enabled() {
            return Ok(Vec::new());
        }
        let mut buffer = [0; 32];
        let count = std::io::stdin()
            .read(&mut buffer)
            .context("Cannot read keyboard input")?;
        Ok(buffer[..count].to_vec())
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        let Some(settings) = &self.original_settings else {
            return;
        };
        match Command::new("stty").arg(settings).status() {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("Warning: could not restore terminal settings ({status})"),
            Err(error) => eprintln!("Warning: could not restore terminal settings: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_arrow_and_selection_keys_without_dropping_partial_sequences() {
        let mut bytes = VecDeque::from([0x1b, b'[']);
        assert_eq!(next_key(&mut bytes), None);
        bytes.push_back(b'A');
        assert_eq!(next_key(&mut bytes), Some(Key::Up));
        assert_eq!(
            next_key(&mut VecDeque::from([0x1b, b'[', b'B'])),
            Some(Key::Down)
        );
        assert_eq!(next_key(&mut VecDeque::from([b' '])), Some(Key::Toggle));
        assert_eq!(next_key(&mut VecDeque::from([b'\r'])), Some(Key::Confirm));
        assert_eq!(next_key(&mut VecDeque::from([b'q'])), Some(Key::Cancel));
    }
}

pub(crate) fn adjust_interval(interval: u64, key: u8) -> Option<u64> {
    match key {
        b'+' | b'=' => Some(interval.saturating_sub(5).max(1)),
        b'-' => Some(interval.saturating_add(5).min(3600)),
        _ => None,
    }
}
