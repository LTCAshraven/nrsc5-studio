#[cfg(target_os = "linux")]
pub use crate::linaudio::{AudioControlError, ProcessVolumeControl};

#[cfg(target_os = "windows")]
pub use crate::winaudio::{AudioControlError, ProcessVolumeControl};
