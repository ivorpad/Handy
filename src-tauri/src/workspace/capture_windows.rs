//! WASAPI microphone and render-device loopback, entirely local.
use super::{
    capture_pcm::{self, Resampler, Track, RATE},
    capture_portable::Control,
};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SampleFormat, SizedSample,
};
use std::{
    path::Path,
    sync::{
        atomic::Ordering,
        mpsc::{self, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(counter: *mut i64) -> i32;
    fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
}
fn clock_ns() -> Result<u128, String> {
    let (mut ticks, mut frequency) = (0, 0);
    if unsafe { QueryPerformanceCounter(&mut ticks) } == 0
        || unsafe { QueryPerformanceFrequency(&mut frequency) } == 0
        || ticks < 0
        || frequency <= 0
    {
        return Err("Windows performance clock is unavailable".into());
    }
    Ok(ticks as u128 * 1_000_000_000 / frequency as u128)
}
struct Fragment {
    source: usize,
    seconds: f64,
    samples: Vec<f32>,
}

fn stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    source: usize,
    origin: u128,
    tx: SyncSender<Fragment>,
    control: Arc<Control>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut resampler = Resampler::new(config.sample_rate, config.channels as usize)?;
    let failure = control.clone();
    device.build_input_stream::<T, _, _>(config, move |data, info| {
        if control.stop.load(Ordering::Acquire) { return; }
        // CPAL WASAPI exposes GetBuffer's QPC capture position in nanoseconds.
        let capture = info.timestamp().capture.as_nanos();
            if capture == 0 || capture > info.timestamp().callback.as_nanos() + 1_000_000_000 {
                control.failed("Windows supplied an invalid capture timestamp".into());
                return;
            }
        let seconds = if capture >= origin { (capture-origin) as f64 / 1e9 } else { -((origin-capture) as f64 / 1e9) };
        let mono: Vec<f32> = data.iter().map(|v| (*v).to_sample::<f32>()).collect();
        match resampler.convert(&mono) {
            Ok(samples) => { if tx.try_send(Fragment { source, seconds, samples }).is_err() { control.failed("Audio capture could not keep up with the device; saved audio is recoverable".into()); } }
            Err(error) => control.failed(error),
        }
    }, move |error| failure.failed(format!("Windows audio capture failed: {error}")), Some(Duration::from_secs(5))).map_err(|e| e.to_string())
}
fn open(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    source: usize,
    origin: u128,
    tx: SyncSender<Fragment>,
    control: Arc<Control>,
) -> Result<cpal::Stream, String> {
    macro_rules! build {
        ($type:ty) => {
            stream::<$type>(device, config.config(), source, origin, tx, control)
        };
    }
    match config.sample_format() {
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        SampleFormat::I16 => build!(i16),
        SampleFormat::U16 => build!(u16),
        SampleFormat::I24 => build!(cpal::I24),
        SampleFormat::U24 => build!(cpal::U24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::U32 => build!(u32),
        SampleFormat::I8 => build!(i8),
        SampleFormat::U8 => build!(u8),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U64 => build!(u64),
        format => Err(format!("Unsupported Windows capture format: {format:?}")),
    }
}
pub fn run(
    directory: &Path,
    microphone: bool,
    system: bool,
    control: &Arc<Control>,
    ready: &SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let host = cpal::default_host();
    let origin = clock_ns()?;
    let elapsed = Instant::now();
    capture_pcm::manifest(directory, "windows_capture_clock")?;
    let (tx, rx) = mpsc::sync_channel(128);
    let mut tracks = Vec::new();
    let mut streams = Vec::new();
    for (enabled, name) in [(microphone, "microphone"), (system, "system")] {
        if !enabled {
            continue;
        }
        let device = if name == "microphone" {
            host.default_input_device()
        } else {
            host.default_output_device()
        }
        .ok_or_else(|| format!("Windows has no default {name} audio device"))?;
        let config = if name == "microphone" {
            device.default_input_config()
        } else {
            device.default_output_config()
        }
        .map_err(|e| format!("Cannot open {name} audio: {e}"))?;
        // WASAPI render devices used as input transparently enable LOOPBACK.
        streams.push(open(
            &device,
            config,
            tracks.len(),
            origin,
            tx.clone(),
            control.clone(),
        )?);
        tracks.push(Track::create(directory, name)?);
    }
    drop(tx);
    if control.stop.load(Ordering::Acquire) {
        return Err("Capture setup was cancelled".into());
    }
    for stream in &streams {
        stream.play().map_err(|e| e.to_string())?;
    }
    ready.send(Ok(())).map_err(|e| e.to_string())?;
    while !control.stop.load(Ordering::Acquire) {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(fragment) => tracks[fragment.source].append(fragment.seconds, &fragment.samples)?,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Windows audio streams disconnected".into())
            }
        }
    }
    drop(streams);
    for fragment in rx.try_iter() {
        tracks[fragment.source].append(fragment.seconds, &fragment.samples)?;
    }
    let end = (elapsed.elapsed().as_secs_f64() * RATE as f64).round() as u64;
    for track in &mut tracks {
        track.pad(end)?;
    }
    Ok(())
}
