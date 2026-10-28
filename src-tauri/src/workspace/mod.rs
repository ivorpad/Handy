pub mod automation;
#[cfg(target_os = "linux")]
pub mod capture_linux;
#[cfg(any(target_os = "windows", target_os = "linux", test))]
pub mod capture_pcm;
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod capture_portable;
#[cfg(target_os = "windows")]
pub mod capture_windows;
pub mod command_classifier;
pub mod connectors;
pub mod context;
pub mod corrections;
pub mod generation;
pub mod learning;
pub mod meeting_capture;
pub mod meeting_diarization;
pub mod meeting_speakers;
pub mod meeting_timeline;
pub mod meetings;
pub mod notes;
pub mod personalization;
pub mod profiles;
pub mod search;
pub mod snippets;
pub mod store;
pub mod voice_commands;
