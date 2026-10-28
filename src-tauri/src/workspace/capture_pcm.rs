//! Recoverable 16 kHz PCM on a shared monotonic timeline.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};

fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| e.to_string())
}

pub const RATE: u64 = 16_000;
pub struct Track {
    file: File,
    frames: u64,
    created: Instant,
}
impl Track {
    pub fn create(directory: &Path, name: &str) -> Result<Self, String> {
        Ok(Self {
            file: private_file(&directory.join(format!("{name}.pcm")))?,
            frames: 0,
            created: Instant::now(),
        })
    }
    pub fn append(&mut self, seconds: f64, samples: &[f32]) -> Result<(), String> {
        if !seconds.is_finite() || seconds < -60. || samples.iter().any(|v| !v.is_finite()) {
            return Err("Capture supplied an invalid timestamp or sample".into());
        }
        let start = (seconds * RATE as f64).round() as i64;
        let skipped = (self.frames as i128 - start as i128).max(0) as usize;
        if start > self.frames as i64 {
            self.pad(start as u64)?;
        }
        for sample in samples.iter().skip(skipped) {
            self.file
                .write_all(&sample.to_le_bytes())
                .map_err(|e| e.to_string())?;
            self.frames += 1;
        }
        // Each delivered fragment survives a process crash independently.
        self.file.sync_data().map_err(|e| e.to_string())
    }
    pub fn pad(&mut self, end: u64) -> Result<(), String> {
        // A quiet render device may deliver no buffers for many minutes. Allow
        // that real elapsed silence, but reject timestamps ahead of our clock.
        if end > (self.created.elapsed().as_secs() + 60) * RATE {
            return Err("Capture clock jumped more than one minute; recording stopped".into());
        }
        let silence = [0_u8; 4096];
        while self.frames < end {
            let frames = (end - self.frames).min(1024);
            self.file
                .write_all(&silence[..frames as usize * 4])
                .map_err(|e| e.to_string())?;
            self.frames += frames;
        }
        self.file.sync_data().map_err(|e| e.to_string())
    }
}

pub fn manifest(directory: &Path, clock: &str) -> Result<(), String> {
    let temporary = directory.join("capture-timeline.json.tmp");
    if directory.join("capture-timeline.json").exists() {
        return Err("A capture timeline already exists in this recording directory".into());
    }
    let mut file = private_file(&temporary)?;
    serde_json::to_writer(&mut file, &serde_json::json!({
        "version": 1, "alignment": "monotonic_pcm_v1", "synchronized": true,
        "sample_rate": RATE, "started_at_ms": chrono::Utc::now().timestamp_millis(), "clock": clock
    })).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, directory.join("capture-timeline.json"))
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Rational phase is retained across device fragments, including 44.1 kHz input.
pub struct Resampler {
    rate: u32,
    channels: usize,
    input: u64,
    output: u64,
    previous: f32,
}
impl Resampler {
    pub fn new(rate: u32, channels: usize) -> Result<Self, String> {
        if rate == 0 || channels == 0 {
            return Err("Invalid device audio format".into());
        }
        Ok(Self {
            rate,
            channels,
            input: 0,
            output: 0,
            previous: 0.,
        })
    }
    pub fn convert(&mut self, data: &[f32]) -> Result<Vec<f32>, String> {
        if data.len() % self.channels != 0 || data.iter().any(|v| !v.is_finite()) {
            return Err("Invalid device audio fragment".into());
        }
        let mut result = Vec::new();
        for frame in data.chunks_exact(self.channels) {
            let mono = frame.iter().sum::<f32>() / self.channels as f32;
            while self.output as u128 * (self.rate as u128) <= self.input as u128 * RATE as u128 {
                let position = self.output as f64 * self.rate as f64 / RATE as f64;
                let fraction = if self.input == 0 {
                    1.
                } else {
                    (position - (self.input - 1) as f64) as f32
                };
                result.push(self.previous + (mono - self.previous) * fraction);
                self.output += 1;
            }
            self.previous = mono;
            self.input += 1;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_resampling_retains_phase_and_downmixes() {
        let mut resampler = Resampler::new(44_100, 2).unwrap();
        let mut output = Vec::new();
        for _ in 0..100 {
            output.extend(resampler.convert(&vec![1., -1.].repeat(441)).unwrap());
        }
        assert_eq!(output.len(), 16_000);
        assert!(output.iter().all(|sample| *sample == 0.));
        assert!(resampler.convert(&[f32::NAN, 0.]).is_err());
    }
    #[test]
    fn delayed_tracks_and_overlap_keep_shared_indices() {
        let dir = std::env::temp_dir().join(format!(
            "handy-capture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let mut track = Track::create(&dir, "system").unwrap();
        assert!(
            Track::create(&dir, "system").is_err(),
            "saved audio must never be overwritten"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.join("system.pcm"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        track.append(0.2, &[1.; 1600]).unwrap();
        track.append(0.25, &[2.; 1600]).unwrap();
        let bytes = std::fs::read(dir.join("system.pcm")).unwrap();
        let pcm: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(pcm.len(), 5600);
        assert!(pcm[..3200].iter().all(|v| *v == 0.));
        assert!(pcm[3200..4800].iter().all(|v| *v == 1.));
        assert!(pcm[4800..].iter().all(|v| *v == 2.));
        assert!(track.append(1000., &[0.]).is_err());
        assert!(track.append(-1000., &[0.]).is_err());
        // A render device may stop delivering packets while the output is quiet.
        track.created -= std::time::Duration::from_secs(120);
        track.append(120., &[3.]).unwrap();
        assert_eq!(track.frames, 120 * RATE + 1);
        drop(track);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
