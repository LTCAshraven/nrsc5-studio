#![cfg(target_os = "linux")]

use std::process::Command;
use thiserror::Error;

/// Which target the volume controller is currently driving.
#[derive(Debug, Clone, PartialEq)]
pub enum ActiveMode {
    PerApp,
    SystemSink,
}

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

#[derive(Debug, Clone)]
enum SessionTarget {
    SinkInput(u32),
    DefaultSink(String),
}

pub struct ProcessVolumeControl {
    session: Option<(u32, SessionTarget)>,
    pub active_mode: Option<ActiveMode>,
}

impl ProcessVolumeControl {
    pub fn new() -> Self {
        Self { session: None, active_mode: None }
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

    fn target_healthy(target: &SessionTarget) -> bool {
        match target {
            SessionTarget::SinkInput(idx) => Self::run_pactl(&[
                "get-sink-input-volume".to_string(),
                idx.to_string(),
            ])
            .is_ok(),
            SessionTarget::DefaultSink(name) => {
                Self::run_pactl(&["get-sink-volume".to_string(), name.clone()]).is_ok()
            }
        }
    }

    fn find_session(pid: u32) -> Result<u32, AudioControlError> {
        let out = Self::run_pactl(&["list".to_string(), "sink-inputs".to_string()])?;
        let mut current_idx: Option<u32> = None;
        let mut current_pid: Option<u32> = None;
        let mut current_binary: Option<String> = None;
        let mut binary_match_idx: Option<u32> = None;

        for line in out.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Sink Input #") {
                if let (Some(idx), Some(bin)) = (current_idx, current_binary.as_ref()) {
                    if bin == "nrsc5" {
                        binary_match_idx = Some(idx);
                    }
                }
                current_idx = rest.trim().parse::<u32>().ok();
                current_pid = None;
                current_binary = None;
                continue;
            }

            if let Some((_, rhs)) = trimmed.split_once('=') {
                let val = rhs.trim().trim_matches('"').to_string();
                if trimmed.starts_with("application.process.id") {
                    current_pid = val.parse::<u32>().ok();
                } else if trimmed.starts_with("application.process.binary") {
                    current_binary = Some(val);
                }
            }

            let Some(idx) = current_idx else {
                continue;
            };

            if current_pid == Some(pid) {
                return Ok(idx);
            }
        }

        // Finalize last block.
        if let (Some(idx), Some(bin)) = (current_idx, current_binary.as_ref()) {
            if bin == "nrsc5" {
                binary_match_idx = Some(idx);
            }
        }

        // PipeWire/Pulse metadata varies across setups; if process ID isn't
        // published, fall back to the first sink input whose process binary is
        // `nrsc5`.
        if let Some(idx) = binary_match_idx {
            return Ok(idx);
        }

        Err(AudioControlError::SessionNotFound(pid))
    }

    fn default_sink() -> Result<String, AudioControlError> {
        let out = Self::run_pactl(&["get-default-sink".to_string()])?;
        let sink = out.trim();
        if sink.is_empty() {
            return Err(AudioControlError::Parse(
                "empty default sink from pactl get-default-sink".to_string(),
            ));
        }
        Ok(sink.to_string())
    }

    fn ensure(&mut self, pid: u32) -> Result<SessionTarget, AudioControlError> {
        let cached_ok = match self.session {
            Some((cached_pid, ref target)) if cached_pid == pid => match target {
                // The system-sink target is a fallback that fires when
                // `find_session` can't yet locate a matching sink-input —
                // typically because libao hasn't connected to PulseAudio
                // yet at the moment the GUI first calls `set_volume`.
                // Never treat this state as cached: retry `find_session`
                // on every call so we transparently upgrade to PerApp
                // mode once libao publishes its sink-input.
                SessionTarget::DefaultSink(_) => false,
                SessionTarget::SinkInput(_) => Self::target_healthy(target),
            },
            _ => false,
        };

        if !cached_ok {
            if let Ok(idx) = Self::find_session(pid) {
                self.session = Some((pid, SessionTarget::SinkInput(idx)));
                self.active_mode = Some(ActiveMode::PerApp);
            } else {
                // Fallback mode: control the default sink when per-process
                // sink-input matching is unavailable in this audio stack.
                let sink = Self::default_sink()?;
                self.session = Some((pid, SessionTarget::DefaultSink(sink)));
                self.active_mode = Some(ActiveMode::SystemSink);
            }
        }

        Ok(self.session
            .as_ref()
            .expect("session populated above")
            .1
            .clone())
    }

    pub fn set_volume(&mut self, pid: u32, value: f32) -> Result<(), AudioControlError> {
        let target = self.ensure(pid)?;
        let pct = (value.clamp(0.0, 1.0) * 100.0).round() as i32;
        match target {
            SessionTarget::SinkInput(idx) => {
                Self::run_pactl(&[
                    "set-sink-input-volume".to_string(),
                    idx.to_string(),
                    format!("{pct}%"),
                ])?;
            }
            SessionTarget::DefaultSink(name) => {
                Self::run_pactl(&[
                    "set-sink-volume".to_string(),
                    name,
                    format!("{pct}%"),
                ])?;
            }
        }
        Ok(())
    }

    pub fn set_mute(&mut self, pid: u32, mute: bool) -> Result<(), AudioControlError> {
        let target = self.ensure(pid)?;
        let flag = if mute { "1" } else { "0" };
        match target {
            SessionTarget::SinkInput(idx) => {
                Self::run_pactl(&[
                    "set-sink-input-mute".to_string(),
                    idx.to_string(),
                    flag.to_string(),
                ])?;
            }
            SessionTarget::DefaultSink(name) => {
                Self::run_pactl(&[
                    "set-sink-mute".to_string(),
                    name,
                    flag.to_string(),
                ])?;
            }
        }
        Ok(())
    }

    pub fn get_volume(&mut self, pid: u32) -> Result<f32, AudioControlError> {
        let target = self.ensure(pid)?;
        let out = match target {
            SessionTarget::SinkInput(idx) => Self::run_pactl(&[
                "get-sink-input-volume".to_string(),
                idx.to_string(),
            ])?,
            SessionTarget::DefaultSink(name) => {
                Self::run_pactl(&["get-sink-volume".to_string(), name])?
            }
        };

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
        self.active_mode = None;
    }
}

impl Default for ProcessVolumeControl {
    fn default() -> Self {
        Self::new()
    }
}
