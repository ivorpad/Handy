//! Explicit meeting capture and the shared microphone reservation.
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Serialize, Deserialize, Type)]
pub struct CaptureCapabilities {
    pub microphone: bool,
    pub system_audio: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Reservation {
    Idle,
    Dictation(u64),
    Processing(u64),
    Starting,
    Meeting,
    Stopping,
}
static RESERVATION: Mutex<Reservation> = Mutex::new(Reservation::Idle);
static CANCELLATION_GATE: Mutex<()> = Mutex::new(());
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

pub struct ProcessingReservation(u64);
impl Drop for ProcessingReservation {
    fn drop(&mut self) {
        if let Ok(mut reservation) = RESERVATION.lock() {
            if *reservation == Reservation::Processing(self.0) {
                *reservation = Reservation::Idle;
            }
        }
    }
}

/// Hold the shared speech engine for an entire imported/meeting transcription.
/// Capture and dictation are excluded until the owning worker exits.
pub fn try_reserve_processing() -> Result<ProcessingReservation, String> {
    let _cancellation = CANCELLATION_GATE.lock().map_err(|e| e.to_string())?;
    let mut reservation = RESERVATION.lock().map_err(|e| e.to_string())?;
    if *reservation != Reservation::Idle {
        return Err(
            "Wait for dictation or meeting capture to finish before transcribing saved audio"
                .into(),
        );
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    *reservation = Reservation::Processing(token);
    Ok(ProcessingReservation(token))
}

/// Keep the global engine cancel/unload sequence atomic with a saved-audio
/// processing reservation. The closure may release dictation ownership because
/// this gate is distinct from the reservation mutex.
pub fn with_dictation_cancellation<T>(operation: impl FnOnce() -> T) -> Option<T> {
    let _cancellation = CANCELLATION_GATE.lock().ok()?;
    if is_processing() {
        return None;
    }
    Some(operation())
}

pub fn is_processing() -> bool {
    RESERVATION
        .lock()
        .map(|state| matches!(*state, Reservation::Processing(_)))
        .unwrap_or(true)
}

pub fn try_reserve_dictation() -> Result<u64, String> {
    let mut reservation = RESERVATION.lock().map_err(|e| e.to_string())?;
    if *reservation != Reservation::Idle {
        return Err("Audio capture is already in use".into());
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    *reservation = Reservation::Dictation(token);
    Ok(token)
}

pub fn release_dictation(token: u64) {
    if let Ok(mut reservation) = RESERVATION.lock() {
        if *reservation == Reservation::Dictation(token) {
            *reservation = Reservation::Idle;
        }
    }
}

pub fn is_recording() -> bool {
    RESERVATION
        .lock()
        .map(|state| {
            matches!(
                *state,
                Reservation::Starting | Reservation::Meeting | Reservation::Stopping
            )
        })
        .unwrap_or(true)
}

pub fn capabilities() -> CaptureCapabilities {
    #[cfg(target_os = "macos")]
    {
        let system_audio = unsafe { handy_meeting_system_available() };
        CaptureCapabilities {
            microphone: true,
            system_audio,
            reason: (!system_audio).then(|| "System audio requires macOS 13 or later".into()),
        }
    }
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        super::capture_portable::capabilities()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    CaptureCapabilities {
        microphone: false,
        system_audio: false,
        reason: Some(
            "Live meeting capture is unavailable on this platform; import a WAV recording".into(),
        ),
    }
}

pub fn start(directory: &Path, microphone: bool, system_audio: bool) -> Result<(), String> {
    if !microphone && !system_audio {
        return Err("Select at least one audio source".into());
    }
    let capabilities = capabilities();
    if (microphone && !capabilities.microphone) || (system_audio && !capabilities.system_audio) {
        return Err(capabilities
            .reason
            .unwrap_or_else(|| "Unsupported audio source".into()));
    }
    {
        let mut reservation = RESERVATION.lock().map_err(|e| e.to_string())?;
        if *reservation != Reservation::Idle {
            return Err("Stop dictation or saved-audio processing before recording".into());
        }
        *reservation = Reservation::Starting;
    }
    let result = platform_start(directory, microphone, system_audio);
    *RESERVATION.lock().map_err(|e| e.to_string())? = if result.is_ok() {
        Reservation::Meeting
    } else {
        Reservation::Idle
    };
    result
}

fn platform_start(directory: &Path, microphone: bool, system_audio: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(directory.as_os_str().as_bytes())
            .map_err(|_| "Recording path contains a null byte")?;
        native_result(unsafe { handy_meeting_start(path.as_ptr(), microphone, system_audio) })
    }
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        super::capture_portable::start(directory, microphone, system_audio)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (directory, microphone, system_audio);
        Err("Live meeting capture is not available on this platform".into())
    }
}

pub fn stop() -> Result<(), String> {
    {
        let mut reservation = RESERVATION.lock().map_err(|e| e.to_string())?;
        if *reservation != Reservation::Meeting {
            return Err("No meeting is recording, or capture is still starting/stopping".into());
        }
        *reservation = Reservation::Stopping;
    }
    #[cfg(target_os = "macos")]
    let result = native_result(unsafe { handy_meeting_stop() });
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let result = super::capture_portable::stop();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let result = Err("Live meeting capture is not available on this platform".into());
    *RESERVATION.lock().map_err(|e| e.to_string())? = Reservation::Idle;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_reservation_excludes_other_starts_and_release_preserves_meetings() {
        let first = try_reserve_dictation().unwrap();
        assert!(try_reserve_dictation().is_err());
        assert!(!is_recording());
        release_dictation(first);
        for state in [
            Reservation::Starting,
            Reservation::Meeting,
            Reservation::Stopping,
        ] {
            *RESERVATION.lock().unwrap() = state;
            assert!(is_recording());
            assert!(try_reserve_dictation().is_err());
            release_dictation(first);
            assert!(is_recording());
        }
        *RESERVATION.lock().unwrap() = Reservation::Idle;
        let second = try_reserve_dictation().unwrap();
        release_dictation(first);
        assert!(
            try_reserve_dictation().is_err(),
            "stale owner must not release a newer session"
        );
        release_dictation(second);
        assert!(!is_recording());
        let processing = try_reserve_processing().unwrap();
        assert!(is_processing());
        assert!(try_reserve_dictation().is_err());
        assert!(try_reserve_processing().is_err());
        assert!(with_dictation_cancellation(|| panic!("must not cancel saved audio")).is_none());
        release_dictation(second);
        assert!(is_processing());
        drop(processing);
        assert!(!is_processing());
        assert_eq!(with_dictation_cancellation(|| 7), Some(7));
        let third = try_reserve_dictation().unwrap();
        release_dictation(third);

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cancelling = std::thread::spawn(move || {
            with_dictation_cancellation(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let (reserved_tx, reserved_rx) = std::sync::mpsc::channel();
        let processing = std::thread::spawn(move || {
            let _owner = try_reserve_processing().unwrap();
            reserved_tx.send(()).unwrap();
        });
        assert!(
            reserved_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err(),
            "processing cannot start during a global engine cancellation"
        );
        release_tx.send(()).unwrap();
        reserved_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        cancelling.join().unwrap();
        processing.join().unwrap();
    }
}

pub fn error() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        native_result(unsafe { handy_meeting_error() }).err()
    }
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        super::capture_portable::error()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    None
}

#[cfg(target_os = "macos")]
fn native_result(pointer: *mut std::ffi::c_char) -> Result<(), String> {
    if pointer.is_null() {
        return Ok(());
    }
    let message = unsafe { std::ffi::CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(pointer.cast()) };
    Err(message)
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn handy_meeting_system_available() -> bool;
    fn handy_meeting_start(
        directory: *const std::ffi::c_char,
        microphone: bool,
        system_audio: bool,
    ) -> *mut std::ffi::c_char;
    fn handy_meeting_stop() -> *mut std::ffi::c_char;
    fn handy_meeting_error() -> *mut std::ffi::c_char;
}
