#![cfg(target_os = "linux")]

use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioControlError {
    #[error("audio session for PID {0} not found (process may not be playing yet)")]
    SessionNotFound(u32),
    #[error("pactl unavailable: {0}")]
    PactlUnavailable(String),
    #[error("pactl call `{cmd}` failed: {stderr}")]
    PactlFailed { cmd: String, stderr: String },
    #[error("failed to parse pactl output: {0}")]
    Parse(String),
}

pub struct ProcessVolumeControl {
    session: Option<(u32, u32)>,
}

impl ProcessVolumeControl {
    pub fn new() -> Self {
        Self { session: None }
    }

    fn run_pactl(args: &[String]) -> Result<String, AudioControlError> {
        let mut cmd = Command::new("pactl");
        cmd.args(args);
        let out = cmd.output().map_err(|e| {
            AudioControlError::PactlUnavailable(format!("spawn pactl failed: {e}"))
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(AudioControlError::PactlFailed {
                cmd: format!("pactl {}", args.join(" ")),
                stderr,
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn find_session(pid: u32) -> Result<u32, AudioControlError> {
        let out = Self::run_pactl(&["list".to_string(), "sink-inputs".to_string()])?;
        let mut current_idx: Option<u32> = None;

        for line in out.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Sink Input #") {
                current_idx = rest.trim().parse::<u32>().ok();
                continue;
            }

            if !trimmed.starts_with("application.process.id") {
                continue;
            }

            let Some(idx) = current_idx else {
                continue;
            };
            let Some((_, rhs)) = trimmed.split_once('=') else {
                continue;
            };
            let pid_str = rhs.trim().trim_matches('"');
            if let Ok(found_pid) = pid_str.parse::<u32>() {
                if found_pid == pid {
                    return Ok(idx);
                }
            }
        }

        Err(AudioControlError::SessionNotFound(pid))
    }

    fn ensure(&mut self, pid: u32) -> Result<u32, AudioControlError> {
        let cached_ok = match self.session {
            Some((cached_pid, idx)) if cached_pid == pid => {
                Self::run_pactl(&[
                    "get-sink-input-volume".to_string(),
                    idx.to_string(),
                ])
                .is_ok()
            }
            _ => false,
        };

        if !cached_ok {
            let idx = Self::find_session(pid)?;
            self.session = Some((pid, idx));
        }

        Ok(self.session.expect("session populated above").1)
    }

    pub fn set_volume(&mut self, pid: u32, value: f32) -> Result<(), AudioControlError> {
        let idx = self.ensure(pid)?;
        let pct = (value.clamp(0.0, 1.0) * 100.0).round() as i32;
        Self::run_pactl(&[
            "set-sink-input-volume".to_string(),
            idx.to_string(),
            format!("{pct}%"),
        ])?;
        Ok(())
    }

    pub fn set_mute(&mut self, pid: u32, mute: bool) -> Result<(), AudioControlError> {
        let idx = self.ensure(pid)?;
        let flag = if mute { "1" } else { "0" };
        Self::run_pactl(&[
            "set-sink-input-mute".to_string(),
            idx.to_string(),
            flag.to_string(),
        ])?;
        Ok(())
    }

    pub fn get_volume(&mut self, pid: u32) -> Result<f32, AudioControlError> {
        let idx = self.ensure(pid)?;
        let out = Self::run_pactl(&[
            "get-sink-input-volume".to_string(),
            idx.to_string(),
        ])?;

        for token in out.split_whitespace() {
            if let Some(raw) = token.strip_suffix('%') {
                if let Ok(p) = raw.parse::<f32>() {
                    return Ok((p / 100.0).clamp(0.0, 1.0));
                }
            }
        }

        Err(AudioControlError::Parse(
            "no percentage found in pactl volume output".to_string(),
        ))
    }

    pub fn detach(&mut self) {
        self.session = None;
    }
}

impl Default for ProcessVolumeControl {
    fn default() -> Self {
        Self::new()
    }
}
