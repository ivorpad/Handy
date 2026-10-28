//! Explicit local diarizer configuration and a pinned, verified model download.
use super::{meeting_diarization, store};
use futures_util::StreamExt;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::AppHandle;

pub const MODEL_URL:&str="https://huggingface.co/handy-computer/diar_streaming_sortformer_4spk-v2.1-gguf/resolve/main/diar_streaming_sortformer_4spk-v2.1-Q8_0.gguf";
const MODEL_BYTES: u64 = 139310336;
const MODEL_SHA: &str = "a5dacdc650790266c7a362e54e6bf51952015487edaa606c4e11632bc32442a9";
static DOWNLOADING: AtomicBool = AtomicBool::new(false);
struct DownloadGuard;
impl Drop for DownloadGuard {
    fn drop(&mut self) {
        DOWNLOADING.store(false, Ordering::Release);
    }
}
struct PartialFile(PathBuf);
impl Drop for PartialFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Clone, Serialize, Deserialize, specta::Type)]
pub struct SpeakerModelConfig {
    pub path: Option<String>,
    pub available: bool,
    pub download_bytes: u64,
    pub download_url: String,
}
fn connection(app: &AppHandle) -> Result<rusqlite::Connection, String> {
    let conn = store::connection(app)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS workspace_meeting_speaker_config (singleton INTEGER PRIMARY KEY CHECK(singleton=1), path TEXT NOT NULL);").map_err(|e|e.to_string())?;
    Ok(conn)
}
pub fn configured_path(app: &AppHandle) -> Result<PathBuf, String> {
    let path=connection(app)?.query_row("SELECT path FROM workspace_meeting_speaker_config WHERE singleton=1",[],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())?.ok_or("Install a local Sortformer diarization model in Meetings to identify speakers. No audio will be uploaded.")?;
    if !Path::new(&path).is_file() {
        return Err("The configured speaker model is missing. Select its GGUF file or download it again in Meetings.".into());
    }
    Ok(PathBuf::from(path))
}
fn save(app: &AppHandle, path: &Path) -> Result<SpeakerModelConfig, String> {
    let path = path.to_str().ok_or("Model path is not valid UTF-8")?;
    connection(app)?.execute("INSERT INTO workspace_meeting_speaker_config(singleton,path) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET path=excluded.path",[path]).map_err(|e|e.to_string())?;
    meeting_speaker_model(app.clone())
}
#[tauri::command]
#[specta::specta]
pub fn meeting_speaker_model(app: AppHandle) -> Result<SpeakerModelConfig, String> {
    let path = connection(&app)?
        .query_row(
            "SELECT path FROM workspace_meeting_speaker_config WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(SpeakerModelConfig {
        available: path.as_ref().is_some_and(|path| Path::new(path).is_file()),
        path,
        download_bytes: MODEL_BYTES,
        download_url: MODEL_URL.into(),
    })
}
#[tauri::command]
#[specta::specta]
pub async fn meeting_set_speaker_model(
    app: AppHandle,
    path: String,
) -> Result<SpeakerModelConfig, String> {
    let validated = PathBuf::from(path);
    let candidate = validated.clone();
    tauri::async_runtime::spawn_blocking(move || meeting_diarization::inspect(&candidate))
        .await
        .map_err(|e| e.to_string())??;
    save(&app, &validated)
}

fn verified(path: &Path) -> Result<bool, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != MODEL_BYTES {
        return Ok(false);
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()) == MODEL_SHA)
}
#[tauri::command]
#[specta::specta]
pub async fn meeting_download_speaker_model(app: AppHandle) -> Result<SpeakerModelConfig, String> {
    if DOWNLOADING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("The speaker model is already downloading".into());
    }
    let _download = DownloadGuard;
    let directory = crate::portable::app_data_dir(&app)
        .map_err(|e| e.to_string())?
        .join("meeting-models");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let destination = directory.join("sortformer-4spk-v2.1-q8_0.gguf");
    if destination.is_file() && verified(&destination)? {
        return save(&app, &destination);
    }
    let partial = PartialFile(directory.join(format!("{}.partial", store::new_id())));
    let client = reqwest::Client::builder()
        .user_agent("Handy/0.9")
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(MODEL_URL)
        .send()
        .await
        .map_err(|e| format!("Speaker model download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Speaker model download failed: {e}"))?;
    if response
        .content_length()
        .is_some_and(|length| length != MODEL_BYTES)
    {
        return Err("Model download size does not match the pinned release".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&partial.0).map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            format!("Speaker model download was interrupted; retry the download: {e}")
        })?;
        bytes = bytes
            .checked_add(chunk.len() as u64)
            .ok_or("Model download is too large")?;
        if bytes > MODEL_BYTES {
            return Err("Model download exceeded the pinned release size".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    if bytes != MODEL_BYTES || format!("{:x}", hash.finalize()) != MODEL_SHA {
        return Err(
            "Speaker model download failed size/SHA-256 verification; retry the download".into(),
        );
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let validated = partial.0.clone();
    tauri::async_runtime::spawn_blocking(move || meeting_diarization::inspect(&validated))
        .await
        .map_err(|e| e.to_string())??;
    std::fs::rename(&partial.0, &destination).map_err(|e| e.to_string())?;
    save(&app, &destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncated_model_without_loading_native_code() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("partial.gguf");
        std::fs::write(&file, b"GGUF").unwrap();
        assert!(!verified(&file).unwrap());
    }
    #[test]
    fn abandoned_download_removes_only_its_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("download.partial");
        let existing = dir.path().join("saved.gguf");
        std::fs::write(&file, b"partial").unwrap();
        std::fs::write(&existing, b"saved").unwrap();
        {
            let _partial = PartialFile(file.clone());
        }
        assert!(!file.exists());
        assert!(existing.exists());
    }
}
