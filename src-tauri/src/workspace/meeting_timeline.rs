//! Capture tracks use one monotonic origin. Padding in the native writer makes
//! a persisted sample's index its meeting time, including after interruption.
use super::store;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize, specta::Type)]
pub struct MeetingSegment {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker_id: Option<String>,
    #[serde(default)]
    pub model_speaker_id: Option<String>,
    #[serde(default)]
    pub model_speaker_ids: Vec<String>,
    pub text: String,
    pub confidence: Option<f32>,
    pub overlap: bool,
    pub corrected: bool,
}

#[derive(Clone, Serialize, Deserialize, specta::Type)]
pub struct MeetingParticipant {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Default, Serialize, Deserialize, specta::Type)]
pub struct MeetingTimeline {
    pub segments: Vec<MeetingSegment>,
    pub participants: Vec<MeetingParticipant>,
    pub alignment: String,
    pub max_speakers: Option<i32>,
    pub model: Option<String>,
    #[serde(default)]
    pub notes_stale: bool,
}

#[derive(Serialize, Deserialize)]
struct CaptureTimeline {
    version: u32,
    alignment: String,
    synchronized: bool,
    sample_rate: u32,
    started_at_ms: i64,
    clock: String,
}

pub fn mark_import(directory: &Path) -> Result<(), String> {
    let marker = CaptureTimeline {
        version: 1,
        alignment: "monotonic_pcm_v1".into(),
        synchronized: true,
        sample_rate: 16000,
        started_at_ms: store::now_ms(),
        clock: "import".into(),
    };
    let path = directory.join("capture-timeline.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, &marker).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

pub fn alignment(directory: &Path) -> Result<String, String> {
    let file = std::fs::File::open(directory.join("capture-timeline.json")).map_err(|_|"This older recording has no synchronized clock metadata. Import a single mixed WAV to identify speakers with an accurate timeline.".to_string())?;
    let marker: CaptureTimeline = serde_json::from_reader(file.take(16384))
        .map_err(|e| format!("Invalid capture clock metadata: {e}"))?;
    if marker.version != 1
        || marker.sample_rate != 16000
        || marker.alignment != "monotonic_pcm_v1"
        || !marker.synchronized
    {
        return Err("These audio tracks do not have verified synchronization. Import a single mixed WAV to identify speakers on a shared timeline.".into());
    }
    Ok(marker.alignment)
}

pub fn mixed_pcm(directory: &Path, sources: &[String]) -> Result<Vec<f32>, String> {
    alignment(directory)?;
    if sources.is_empty() || sources.len() > 3 {
        return Err("Invalid audio sources".into());
    }
    let mut tracks = Vec::new();
    let mut count = 0usize;
    for source in sources {
        if !["microphone", "system", "import"].contains(&source.as_str()) {
            return Err("Invalid audio source".into());
        }
        let file = std::fs::File::open(directory.join(format!("{source}.pcm")))
            .map_err(|e| format!("Cannot read {source} recording: {e}"))?;
        let samples = file.metadata().map_err(|e| e.to_string())?.len() / 4;
        if samples > 16000 * 60 * 60 * 4 {
            return Err("Speaker identification supports up to four hours per recording".into());
        }
        count = count.max(samples as usize);
        tracks.push(file);
    }
    if count == 0 {
        return Err("No saved audio is available".into());
    }
    let mut mixed = Vec::new();
    mixed
        .try_reserve_exact(count)
        .map_err(|_| "Not enough memory to identify speakers in this recording")?;
    mixed.resize(count, 0f32);
    let scale = 1.0 / tracks.len() as f32;
    for mut track in tracks {
        let mut cursor = 0;
        loop {
            let mut bytes = Vec::new();
            (&mut track)
                .take(16000 * 4 * 60)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() < 4 {
                break;
            }
            for bytes in bytes.chunks_exact(4) {
                let sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if !sample.is_finite() {
                    return Err("Recording contains non-finite samples".into());
                }
                let target = mixed
                    .get_mut(cursor)
                    .ok_or("Recording changed while reading its timeline")?;
                *target += sample.clamp(-1.0, 1.0) * scale;
                cursor += 1;
            }
        }
    }
    Ok(mixed)
}

pub fn render(timeline: &MeetingTimeline) -> String {
    timeline
        .segments
        .iter()
        .map(|segment| {
            let speaker = segment
                .speaker_id
                .as_ref()
                .and_then(|id| timeline.participants.iter().find(|p| &p.id == id))
                .map(|p| p.label.as_str())
                .filter(|s| !s.is_empty())
                .or(segment.speaker_id.as_deref())
                .unwrap_or("Unassigned speaker");
            format!(
                "[{:02}:{:02}.{:03}–{:02}:{:02}.{:03}] {speaker}{}: {}\n\n",
                segment.start_ms / 60000,
                (segment.start_ms / 1000) % 60,
                segment.start_ms % 1000,
                segment.end_ms / 60000,
                (segment.end_ms / 1000) % 60,
                segment.end_ms % 1000,
                if segment.overlap {
                    " (overlapping speech)"
                } else {
                    ""
                },
                segment.text
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aligned_tracks_mix_at_sample_indices_and_preserve_partial_tail() {
        let dir = tempfile::tempdir().unwrap();
        mark_import(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("microphone.pcm"),
            [0f32, 0.5, 0.5, 0.0]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut bytes = [0f32, 0.0, 0.25]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        bytes.push(3);
        std::fs::write(dir.path().join("system.pcm"), bytes).unwrap();
        assert_eq!(
            mixed_pcm(dir.path(), &["microphone".into(), "system".into()]).unwrap(),
            vec![0.0, 0.25, 0.375, 0.0]
        );
    }
    #[test]
    fn legacy_recordings_never_claim_synchronization() {
        let dir = tempfile::tempdir().unwrap();
        assert!(alignment(dir.path()).is_err());
        std::fs::write(dir.path().join("capture-timeline.json"),r#"{"version":1,"alignment":"monotonic_pcm_v1","synchronized":false,"sample_rate":16000,"started_at_ms":0,"clock":"unknown"}"#).unwrap();
        assert!(alignment(dir.path()).is_err());
    }
}
