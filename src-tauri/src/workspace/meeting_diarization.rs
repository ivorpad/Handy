//! Local model-backed speaker turns. Native inference stays in an isolated
//! CPU worker; capture sources never stand in for speaker identity.
use crate::engine_supervisor::{diarize_local, DiarizationOutput};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const SAMPLE_RATE: usize = 16000;
const MAX_SAMPLES: usize = SAMPLE_RATE * 60 * 60 * 4;

#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct DiarizationModelInfo {
    pub max_speakers: i32,
    pub arch: String,
    pub variant: String,
    pub backend: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct DiarizedTurn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker_id: String,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct DiarizationResult {
    pub turns: Vec<DiarizedTurn>,
    pub max_speakers: i32,
    pub model: DiarizationModelInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpeechRegion {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker_ids: Vec<String>,
    pub confidence: Option<f32>,
}

fn conservative_confidence(left: Option<f32>, right: Option<f32>) -> Option<f32> {
    match (left, right) {
        (Some(left), Some(right)) if left.is_finite() && right.is_finite() => Some(left.min(right)),
        _ => None,
    }
}

/// Partition speaker activity into disjoint audio regions. Shared speech is
/// decoded once with every active model ID, never duplicated as separate words
/// confidently attributed to each person. Silence remains a gap.
pub fn speech_regions(turns: &[DiarizedTurn]) -> Vec<SpeechRegion> {
    let mut endpoints: BTreeMap<i64, Vec<(usize, bool)>> = BTreeMap::new();
    for (index, turn) in turns.iter().enumerate() {
        if turn.start_ms < 0 || turn.end_ms <= turn.start_ms || turn.speaker_id.is_empty() {
            continue;
        }
        endpoints
            .entry(turn.start_ms)
            .or_default()
            .push((index, true));
        endpoints
            .entry(turn.end_ms)
            .or_default()
            .push((index, false));
    }
    let mut active = BTreeSet::new();
    let mut regions: Vec<SpeechRegion> = Vec::new();
    let mut endpoints = endpoints.into_iter().peekable();
    while let Some((start_ms, changes)) = endpoints.next() {
        for (index, starts) in changes {
            if starts {
                active.insert(index);
            } else {
                active.remove(&index);
            }
        }
        let Some((end_ms, _)) = endpoints.peek() else {
            break;
        };
        if active.is_empty() {
            continue;
        }
        let mut speaker_ids = BTreeSet::new();
        let mut confidence = Some(1.0f32);
        for index in &active {
            let turn = &turns[*index];
            speaker_ids.insert(turn.speaker_id.clone());
            confidence = conservative_confidence(confidence, turn.confidence);
        }
        let speaker_ids = speaker_ids.into_iter().collect::<Vec<_>>();
        if let Some(previous) = regions.last_mut() {
            if previous.end_ms == start_ms && previous.speaker_ids == speaker_ids {
                previous.end_ms = *end_ms;
                previous.confidence = conservative_confidence(previous.confidence, confidence);
                continue;
            }
        }
        regions.push(SpeechRegion {
            start_ms,
            end_ms: *end_ms,
            speaker_ids,
            confidence,
        });
    }
    regions
}

fn check_model_path(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err("Speaker identification needs a local DIARIZE model. Download the Sortformer speaker model in Meetings, or choose an existing compatible GGUF file.".into());
    }
    Ok(())
}

fn model_info(output: &DiarizationOutput) -> Result<DiarizationModelInfo, String> {
    if output.max_speakers < 1 || output.max_speakers > 1000 || output.arch.is_empty() {
        return Err("The speaker model returned invalid capabilities".into());
    }
    Ok(DiarizationModelInfo {
        max_speakers: output.max_speakers,
        arch: output.arch.clone(),
        variant: output.variant.clone(),
        backend: output.backend.clone(),
    })
}

/// Validate a local model's role without recording or reading any audio.
pub fn inspect(model_path: &Path) -> Result<DiarizationModelInfo, String> {
    check_model_path(model_path)?;
    model_info(&diarize_local(model_path.to_path_buf(), &[], true)?)
}

/// Identify stable, anonymous speaker IDs across one entire aligned recording.
/// These IDs are local to this recording; participant names require a person.
pub fn diarize(model_path: &Path, pcm: &[f32]) -> Result<DiarizationResult, String> {
    check_model_path(model_path)?;
    if pcm.is_empty() || pcm.len() > MAX_SAMPLES {
        return Err("Speaker identification accepts nonempty recordings up to four hours".into());
    }
    if pcm.iter().any(|sample| !sample.is_finite()) {
        return Err("Meeting audio contains invalid samples".into());
    }
    let duration_ms = ((pcm.len() as u64 * 1000).div_ceil(SAMPLE_RATE as u64)) as i64;
    let output = diarize_local(model_path.to_path_buf(), pcm, false)?;
    validate_output(output, duration_ms)
}

fn validate_output(
    output: DiarizationOutput,
    duration_ms: i64,
) -> Result<DiarizationResult, String> {
    let model = model_info(&output)?;
    let mut turns = Vec::with_capacity(output.segments.len());
    for segment in output.segments {
        if segment.speaker_id < 1
            || segment.speaker_id > model.max_speakers
            || segment.t0_ms < 0
            || segment.t1_ms <= segment.t0_ms
            || segment.t0_ms >= duration_ms
            || segment.t1_ms > duration_ms.saturating_add(1000)
            || segment.p.is_infinite()
            || (segment.p.is_finite() && !(0.0..=1.0).contains(&segment.p))
        {
            return Err(
                "The speaker model returned an invalid turn. Audio is preserved for retry.".into(),
            );
        }
        turns.push(DiarizedTurn {
            start_ms: segment.t0_ms,
            // Models may include their padded final analysis frame.
            end_ms: segment.t1_ms.min(duration_ms),
            speaker_id: format!("speaker-{}", segment.speaker_id),
            confidence: segment.p.is_finite().then_some(segment.p),
        });
    }
    turns.sort_by(|left, right| {
        left.start_ms
            .cmp(&right.start_ms)
            .then_with(|| left.end_ms.cmp(&right.end_ms))
            .then_with(|| left.speaker_id.cmp(&right.speaker_id))
    });
    if turns.is_empty() {
        return Err("The local speaker model found no speech turns. Audio is preserved; try another compatible model or recording.".into());
    }
    Ok(DiarizationResult {
        max_speakers: model.max_speakers,
        model,
        turns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use transcribe_cpp::SpeakerSegment;

    fn output(segments: Vec<SpeakerSegment>) -> DiarizationOutput {
        DiarizationOutput {
            segments,
            max_speakers: 4,
            arch: "sortformer".into(),
            variant: "synthetic".into(),
            backend: "cpu".into(),
        }
    }
    fn turn(start: i64, end: i64, speaker: i32) -> SpeakerSegment {
        SpeakerSegment {
            t0_ms: start,
            t1_ms: end,
            speaker_id: speaker,
            p: f32::NAN,
        }
    }
    #[test]
    fn speaker_turns_sort_by_time_keep_overlap_and_never_infer_names() {
        let result =
            validate_output(output(vec![turn(500, 1500, 2), turn(0, 1000, 1)]), 2000).unwrap();
        assert_eq!(result.turns[0].speaker_id, "speaker-1");
        assert_eq!(result.turns[1].start_ms, 500);
        assert!(result.turns[0].end_ms > result.turns[1].start_ms);
        assert_eq!(result.turns[0].confidence, None);
        assert_eq!(result.max_speakers, 4);
    }
    #[test]
    fn rejects_invalid_ids_times_probabilities_and_empty_results() {
        for segment in [
            turn(0, 1000, 0),
            turn(0, 1000, 5),
            turn(-1, 1000, 1),
            turn(1000, 1000, 1),
            turn(0, 4000, 1),
            SpeakerSegment {
                p: f32::INFINITY,
                ..turn(0, 1000, 1)
            },
        ] {
            assert!(validate_output(output(vec![segment]), 2000).is_err());
        }
        assert!(validate_output(output(vec![]), 2000).is_err());
    }
    #[test]
    fn allows_only_bounded_model_padding_at_recording_end() {
        assert_eq!(
            validate_output(output(vec![turn(0, 2100, 1)]), 2000)
                .unwrap()
                .turns[0]
                .end_ms,
            2000
        );
    }
    #[test]
    fn missing_model_is_actionable_without_spawning_native_worker() {
        assert!(inspect(Path::new("/nonexistent/speaker-model.gguf"))
            .unwrap_err()
            .contains("Download the Sortformer"));
    }

    fn identified(
        start_ms: i64,
        end_ms: i64,
        speaker: &str,
        confidence: Option<f32>,
    ) -> DiarizedTurn {
        DiarizedTurn {
            start_ms,
            end_ms,
            speaker_id: speaker.into(),
            confidence,
        }
    }

    #[test]
    fn overlap_is_one_shared_region_between_exclusive_regions() {
        let regions = speech_regions(&[
            identified(500, 1500, "B", Some(0.8)),
            identified(0, 1000, "A", Some(0.9)),
        ]);
        assert_eq!(
            regions,
            vec![
                SpeechRegion {
                    start_ms: 0,
                    end_ms: 500,
                    speaker_ids: vec!["A".into()],
                    confidence: Some(0.9)
                },
                SpeechRegion {
                    start_ms: 500,
                    end_ms: 1000,
                    speaker_ids: vec!["A".into(), "B".into()],
                    confidence: Some(0.8)
                },
                SpeechRegion {
                    start_ms: 1000,
                    end_ms: 1500,
                    speaker_ids: vec!["B".into()],
                    confidence: Some(0.8)
                },
            ]
        );
        assert_eq!(
            regions
                .iter()
                .map(|region| region.end_ms - region.start_ms)
                .sum::<i64>(),
            1500
        );
    }

    #[test]
    fn same_speaker_overlaps_and_adjacent_turns_merge_but_silence_stays_a_gap() {
        let regions = speech_regions(&[
            identified(0, 1000, "A", Some(0.9)),
            identified(500, 1500, "A", Some(0.7)),
            identified(1500, 2000, "A", Some(0.8)),
            identified(2500, 3000, "A", None),
        ]);
        assert_eq!(
            regions,
            vec![
                SpeechRegion {
                    start_ms: 0,
                    end_ms: 2000,
                    speaker_ids: vec!["A".into()],
                    confidence: Some(0.7)
                },
                SpeechRegion {
                    start_ms: 2500,
                    end_ms: 3000,
                    speaker_ids: vec!["A".into()],
                    confidence: None
                },
            ]
        );
    }

    #[test]
    fn shared_regions_deduplicate_ids_and_keep_missing_confidence_unknown() {
        let regions = speech_regions(&[
            identified(0, 1000, "A", Some(0.9)),
            identified(0, 500, "A", None),
            identified(0, 500, "B", Some(0.8)),
            identified(500, 1000, "B", Some(0.7)),
        ]);
        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0].speaker_ids,
            vec!["A".to_string(), "B".to_string()]
        );
        assert_eq!(regions[0].start_ms, 0);
        assert_eq!(regions[0].end_ms, 1000);
        assert_eq!(regions[0].confidence, None);
        assert!(speech_regions(&[]).is_empty());
    }

    /// Runs actual local native inference, without recording ambient audio.
    /// Requires a built Handy executable, a DIARIZE model and a synthetic WAV.
    #[test]
    #[ignore = "requires HANDY_TRANSCRIBE_WORKER_EXE, HANDY_TEST_DIARIZATION_MODEL and HANDY_TEST_DIARIZATION_WAV"]
    fn local_model_worker_inspects_and_diarizes_synthetic_wav() {
        let model = std::env::var_os("HANDY_TEST_DIARIZATION_MODEL")
            .expect("set HANDY_TEST_DIARIZATION_MODEL to a local DIARIZE GGUF");
        let wav = std::env::var_os("HANDY_TEST_DIARIZATION_WAV")
            .expect("set HANDY_TEST_DIARIZATION_WAV to a synthetic 16 kHz mono WAV");
        assert!(
            std::env::var_os("HANDY_TRANSCRIBE_WORKER_EXE").is_some(),
            "set HANDY_TRANSCRIBE_WORKER_EXE to the built Handy binary"
        );
        let mut reader = hound::WavReader::open(wav).expect("read synthetic fixture");
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, SAMPLE_RATE as u32);
        assert_eq!(spec.channels, 1);
        let pcm = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .expect("read float fixture samples"),
            hound::SampleFormat::Int => {
                let scale = 2f32.powi(spec.bits_per_sample as i32 - 1);
                reader
                    .samples::<i32>()
                    .map(|value| value.map(|sample| sample as f32 / scale))
                    .collect::<Result<Vec<_>, _>>()
                    .expect("read integer fixture samples")
            }
        };
        assert!(!pcm.is_empty());
        let duration_ms = ((pcm.len() as u64 * 1000).div_ceil(SAMPLE_RATE as u64)) as i64;
        let info = inspect(Path::new(&model))
            .expect("inspect actual local speaker model in isolated worker");
        assert!(info.max_speakers > 0);
        assert!(!info.arch.is_empty());
        let result = diarize(Path::new(&model), &pcm)
            .expect("diarize synthetic fixture in separate local CPU worker");
        assert_eq!(result.max_speakers, info.max_speakers);
        assert_eq!(result.model.arch, info.arch);
        assert!(!result.turns.is_empty());
        let mut previous_start = 0;
        for turn in &result.turns {
            assert!(turn.start_ms >= previous_start);
            assert!(turn.start_ms >= 0 && turn.end_ms > turn.start_ms);
            assert!(turn.end_ms <= duration_ms);
            let speaker: i32 = turn
                .speaker_id
                .strip_prefix("speaker-")
                .expect("anonymous model speaker ID")
                .parse()
                .expect("numeric speaker ID");
            assert!((1..=info.max_speakers).contains(&speaker));
            assert!(turn
                .confidence
                .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value)));
            previous_start = turn.start_ms;
        }
        eprintln!(
            "Local diarization turns (speaker ID, start ms, end ms): {:?}",
            result
                .turns
                .iter()
                .map(|turn| (&turn.speaker_id, turn.start_ms, turn.end_ms))
                .collect::<Vec<_>>()
        );
    }
}
