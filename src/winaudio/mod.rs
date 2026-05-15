//! Per-process audio volume control on Windows via COM (`IAudioSessionManager2`).
//!
//! Used to control `nrsc5.exe`'s audio output: we look up the audio session
//! whose process ID matches the spawned child and call `ISimpleAudioVolume`
//! to set master volume and mute state.
//!
//! Notes / sharp edges:
//! * A session does **not** exist until the target process actually plays audio.
//! * Sessions can disappear when the process exits; cached handles must be
//!   re-discovered after every start / retune.
//! * COM apartment threading: all calls must come from the same thread that
//!   initialized COM. The GUI's `update()` is the main thread, so we init
//!   COM lazily there.

#![cfg(target_os = "windows")]

use thiserror::Error;
use windows::core::Interface;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};

#[derive(Debug, Error)]
pub enum AudioControlError {
    #[error("audio session for PID {0} not found (process may not be playing yet)")]
    SessionNotFound(u32),
    #[error("Windows COM call failed: {0:?}")]
    Hresult(windows::core::HRESULT),
}

impl From<windows::core::Error> for AudioControlError {
    fn from(e: windows::core::Error) -> Self {
        Self::Hresult(e.code())
    }
}

/// Volume controller scoped to a single process's audio session.
///
/// Construct once and reuse; the COM session handle is cached and only
/// re-discovered when the target PID changes or the cached handle goes stale.
pub struct ProcessVolumeControl {
    /// Cached `(pid, ISimpleAudioVolume)` for the most recently attached session.
    session: Option<(u32, ISimpleAudioVolume)>,
}

impl ProcessVolumeControl {
    pub fn new() -> Self {
        // Initialize COM on the calling thread. If COM is already initialized
        // in a compatible mode the call returns S_FALSE which is not an error.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        Self { session: None }
    }

    /// Walk all active audio sessions on the default render endpoint and return
    /// the `ISimpleAudioVolume` for the one whose process ID matches `pid`.
    fn find_session(pid: u32) -> Result<ISimpleAudioVolume, AudioControlError> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
            let manager: IAudioSessionManager2 =
                device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)?;
            let sessions = manager.GetSessionEnumerator()?;
            let count = sessions.GetCount()?;
            for i in 0..count {
                let ctrl = sessions.GetSession(i)?;
                let ctrl2: IAudioSessionControl2 = ctrl.cast()?;
                let session_pid = ctrl2.GetProcessId()?;
                if session_pid == pid {
                    let vol: ISimpleAudioVolume = ctrl.cast()?;
                    return Ok(vol);
                }
            }
            Err(AudioControlError::SessionNotFound(pid))
        }
    }

    /// Ensure we hold a valid session handle for `pid`, refreshing if stale.
    fn ensure(&mut self, pid: u32) -> Result<&ISimpleAudioVolume, AudioControlError> {
        let cached_ok = match &self.session {
            Some((cached_pid, vol)) if *cached_pid == pid => {
                // Probe the cached handle. If the session died, the call errors
                // and we fall through to rediscovery.
                unsafe { vol.GetMasterVolume().is_ok() }
            }
            _ => false,
        };

        if !cached_ok {
            let vol = Self::find_session(pid)?;
            self.session = Some((pid, vol));
        }

        Ok(&self.session.as_ref().expect("session populated above").1)
    }

    pub fn set_volume(&mut self, pid: u32, value: f32) -> Result<(), AudioControlError> {
        let value = value.clamp(0.0, 1.0);
        let vol = self.ensure(pid)?;
        unsafe { vol.SetMasterVolume(value, std::ptr::null())? };
        Ok(())
    }

    pub fn set_mute(&mut self, pid: u32, mute: bool) -> Result<(), AudioControlError> {
        let vol = self.ensure(pid)?;
        unsafe { vol.SetMute(mute, std::ptr::null())? };
        Ok(())
    }

    pub fn get_volume(&mut self, pid: u32) -> Result<f32, AudioControlError> {
        let vol = self.ensure(pid)?;
        unsafe { Ok(vol.GetMasterVolume()?) }
    }

    /// Drop the cached session handle (call when the target process exits).
    pub fn detach(&mut self) {
        self.session = None;
    }
}

impl Default for ProcessVolumeControl {
    fn default() -> Self {
        Self::new()
    }
}
