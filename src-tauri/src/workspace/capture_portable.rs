//! Worker ownership keeps platform audio objects on their creating thread.
use super::meeting_capture::CaptureCapabilities;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};
pub struct Control {
    pub stop: Arc<AtomicBool>,
    pub failure: Arc<Mutex<Option<String>>>,
}
impl Control {
    pub fn failed(&self, message: String) {
        if let Ok(mut failure) = self.failure.lock() {
            if failure.is_none() {
                *failure = Some(message);
            }
        }
        self.stop.store(true, Ordering::Release);
    }
}
struct Session {
    control: Arc<Control>,
    worker: JoinHandle<Result<(), String>>,
}
static ACTIVE: Mutex<Option<Session>> = Mutex::new(None);

pub fn capabilities() -> CaptureCapabilities {
    #[cfg(target_os = "windows")]
    let available = true;
    #[cfg(target_os = "linux")]
    let available = super::capture_linux::available();
    CaptureCapabilities { microphone: available, system_audio: available, reason: (!available).then(|| "Install libpulse and run a local PulseAudio or PipeWire Pulse server to capture meetings".into()) }
}
pub fn start(directory: &Path, microphone: bool, system: bool) -> Result<(), String> {
    let mut active = ACTIVE.lock().map_err(|e| e.to_string())?;
    if active.is_some() {
        return Err("A meeting capture worker is already active".into());
    }
    let control = Arc::new(Control {
        stop: Arc::new(AtomicBool::new(false)),
        failure: Arc::new(Mutex::new(None)),
    });
    let worker_control = control.clone();
    let directory = directory.to_owned();
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        let result =
            super::capture_windows::run(&directory, microphone, system, &worker_control, &tx);
        #[cfg(target_os = "linux")]
        let result =
            super::capture_linux::run(&directory, microphone, system, &worker_control, &tx);
        if let Err(message) = &result {
            worker_control.failed(message.clone());
            let _ = tx.try_send(Err(message.clone()));
        }
        result
    });
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(())) => {
            *active = Some(Session { control, worker });
            Ok(())
        }
        result => {
            control.stop.store(true, Ordering::Release);
            let _ = worker.join();
            match result {
                Ok(Err(message)) => Err(message),
                _ => Err("Native audio capture setup timed out".into()),
            }
        }
    }
}
pub fn stop() -> Result<(), String> {
    let session = ACTIVE
        .lock()
        .map_err(|e| e.to_string())?
        .take()
        .ok_or("No meeting capture worker is active")?;
    session.control.stop.store(true, Ordering::Release);
    session
        .worker
        .join()
        .map_err(|_| "Native audio capture worker panicked".to_string())??;
    let failure = session
        .control
        .failure
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    failure.map_or(Ok(()), Err)
}
pub fn error() -> Option<String> {
    ACTIVE
        .lock()
        .ok()?
        .as_ref()?
        .control
        .failure
        .lock()
        .ok()?
        .clone()
}
