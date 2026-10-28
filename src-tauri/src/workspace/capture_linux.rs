//! PulseAudio's native client protocol also supports PipeWire's Pulse server.
//! No daemon is spawned, and remote servers are rejected before recording.
use super::{
    capture_pcm::{self, Track, RATE},
    capture_portable::Control,
};
use std::{
    ffi::{c_char, c_int, c_void, CStr},
    path::Path,
    ptr,
    sync::{atomic::Ordering, mpsc::SyncSender, Arc},
    time::{Duration, Instant},
};
type Pointer = *mut c_void;
#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}
#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

// Signatures match the stable libpulse public C ABI (pulse/{mainloop,context,stream}.h).
macro_rules! pulse_api {
    ($($name:ident($($arg:ty),*) -> $result:ty;)+) => {
        struct Api { library: Pointer, $( $name: unsafe extern "C" fn($($arg),*) -> $result, )+ }
        impl Api {
            fn load() -> Result<Self, String> {
                let library = unsafe { libc::dlopen(c"libpulse.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if library.is_null() { return Err("libpulse.so.0 is unavailable; install the PulseAudio client library".into()); }
                let result = (|| Ok(Self { library, $( $name: {
                    let address = unsafe { libc::dlsym(library, concat!(stringify!($name), "\0").as_ptr().cast()) };
                    if address.is_null() { return Err(format!("Installed libpulse lacks {}", stringify!($name))); }
                    unsafe { std::mem::transmute::<Pointer, unsafe extern "C" fn($($arg),*) -> $result>(address) }
                }, )+ }))();
                if result.is_err() { unsafe { libc::dlclose(library); } }
                result
            }
        }
        impl Drop for Api { fn drop(&mut self) { unsafe { libc::dlclose(self.library); } } }
    }
}
pulse_api! {
    pa_mainloop_new() -> Pointer;
    pa_mainloop_free(Pointer) -> ();
    pa_mainloop_get_api(Pointer) -> Pointer;
    pa_mainloop_iterate(Pointer, c_int, *mut c_int) -> c_int;
    pa_context_new(Pointer, *const c_char) -> Pointer;
    pa_context_connect(Pointer, *const c_char, u32, Pointer) -> c_int;
    pa_context_disconnect(Pointer) -> ();
    pa_context_unref(Pointer) -> ();
    pa_context_get_state(Pointer) -> c_int;
    pa_context_errno(Pointer) -> c_int;
    pa_context_is_local(Pointer) -> c_int;
    pa_strerror(c_int) -> *const c_char;
    pa_stream_new(Pointer, *const c_char, *const SampleSpec, Pointer) -> Pointer;
    pa_stream_connect_record(Pointer, *const c_char, *const BufferAttr, u32) -> c_int;
    pa_stream_get_state(Pointer) -> c_int;
    pa_stream_readable_size(Pointer) -> usize;
    pa_stream_peek(Pointer, *mut *const c_void, *mut usize) -> c_int;
    pa_stream_drop(Pointer) -> c_int;
    pa_stream_get_latency(Pointer, *mut u64, *mut c_int) -> c_int;
    pa_stream_disconnect(Pointer) -> c_int;
    pa_stream_unref(Pointer) -> ();
}
pub fn available() -> bool {
    Api::load().is_ok()
}
struct Connection {
    api: Api,
    mainloop: Pointer,
    context: Pointer,
    streams: Vec<Pointer>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            for stream in self.streams.drain(..) {
                (self.api.pa_stream_disconnect)(stream);
                (self.api.pa_stream_unref)(stream);
            }
            if !self.context.is_null() {
                (self.api.pa_context_disconnect)(self.context);
                (self.api.pa_context_unref)(self.context);
            }
            if !self.mainloop.is_null() {
                (self.api.pa_mainloop_free)(self.mainloop);
            }
        }
    }
}
impl Connection {
    fn error(&self, action: &str) -> String {
        let description =
            unsafe { (self.api.pa_strerror)((self.api.pa_context_errno)(self.context)) };
        let description = if description.is_null() {
            "unknown PulseAudio error".into()
        } else {
            unsafe { CStr::from_ptr(description) }.to_string_lossy()
        };
        format!("{action}: {description}. Check the local audio service and microphone permissions")
    }
    fn iterate(&self) -> Result<(), String> {
        if unsafe { (self.api.pa_mainloop_iterate)(self.mainloop, 0, ptr::null_mut()) } < 0 {
            return Err(self.error("PulseAudio mainloop stopped"));
        }
        if unsafe { (self.api.pa_context_get_state)(self.context) } >= 5 {
            return Err(self.error("Local audio service disconnected"));
        }
        Ok(())
    }
    fn wait(
        &self,
        control: &Control,
        mut ready: impl FnMut() -> Result<bool, String>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.iterate()?;
            if ready()? {
                return Ok(());
            }
            if control.stop.load(Ordering::Acquire) {
                return Err("Capture setup was cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err(
                    "Local audio service did not finish capture setup within ten seconds".into(),
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn connect(control: &Control) -> Result<Self, String> {
        let api = Api::load()?;
        let mainloop = unsafe { (api.pa_mainloop_new)() };
        if mainloop.is_null() {
            return Err("Cannot allocate PulseAudio mainloop".into());
        }
        let mut connection = Self {
            api,
            mainloop,
            context: ptr::null_mut(),
            streams: Vec::new(),
        };
        connection.context = unsafe {
            (connection.api.pa_context_new)(
                (connection.api.pa_mainloop_get_api)(mainloop),
                c"Handy local meeting capture".as_ptr(),
            )
        };
        if connection.context.is_null() {
            return Err("Cannot allocate PulseAudio context".into());
        }
        // NOAUTOSPAWN: capture never starts an audio daemon or contacts a cloud API.
        if unsafe {
            (connection.api.pa_context_connect)(connection.context, ptr::null(), 1, ptr::null_mut())
        } < 0
        {
            return Err(connection.error("Cannot connect to the local PulseAudio/PipeWire service"));
        }
        connection.wait(control, || {
            Ok(unsafe { (connection.api.pa_context_get_state)(connection.context) } == 4)
        })?;
        if unsafe { (connection.api.pa_context_is_local)(connection.context) } != 1 {
            return Err("Meeting capture requires a local audio server; remote PulseAudio servers are disabled".into());
        }
        Ok(connection)
    }
    fn record(&mut self, monitor: bool, control: &Control) -> Result<(), String> {
        let spec = SampleSpec {
            format: 5,
            rate: RATE as u32,
            channels: 1,
        }; // PA_SAMPLE_FLOAT32LE
        let stream = unsafe {
            (self.api.pa_stream_new)(
                self.context,
                if monitor {
                    c"Handy system audio".as_ptr()
                } else {
                    c"Handy microphone".as_ptr()
                },
                &spec,
                ptr::null_mut(),
            )
        };
        if stream.is_null() {
            return Err(self.error("Cannot allocate capture stream"));
        }
        self.streams.push(stream);
        let buffer = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 1280,
        };
        // INTERPOLATE_TIMING | AUTO_TIMING_UPDATE | ADJUST_LATENCY
        let flags = 0x0002 | 0x0008 | 0x2000;
        let device = if monitor {
            c"@DEFAULT_MONITOR@"
        } else {
            c"@DEFAULT_SOURCE@"
        };
        if unsafe { (self.api.pa_stream_connect_record)(stream, device.as_ptr(), &buffer, flags) }
            < 0
        {
            return Err(self.error("Cannot connect capture source"));
        }
        self.wait(control, || {
            match unsafe { (self.api.pa_stream_get_state)(stream) } {
                2 => Ok(true),
                3 | 4 => Err(self.error("Capture source is unavailable")),
                _ => Ok(false),
            }
        })
    }
}

pub fn run(
    directory: &Path,
    microphone: bool,
    system: bool,
    control: &Arc<Control>,
    ready: &SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let mut connection = Connection::connect(control)?;
    let origin = Instant::now();
    capture_pcm::manifest(directory, "linux_pulse_clock")?;
    let mut tracks = Vec::new();
    for (enabled, name) in [(microphone, "microphone"), (system, "system")] {
        if enabled {
            connection.record(name == "system", control)?;
            tracks.push(Track::create(directory, name)?);
        }
    }
    ready.send(Ok(())).map_err(|e| e.to_string())?;
    let mut timing_missing = vec![None; tracks.len()];
    while !control.stop.load(Ordering::Acquire) {
        connection.iterate()?;
        for (index, stream) in connection.streams.iter().copied().enumerate() {
            if unsafe { (connection.api.pa_stream_get_state)(stream) } != 2 {
                return Err(connection.error("Audio capture source disconnected"));
            }
            let size = unsafe { (connection.api.pa_stream_readable_size)(stream) };
            if size == usize::MAX {
                return Err(connection.error("Cannot read capture source"));
            }
            if size == 0 {
                continue;
            }
            let (mut latency, mut negative) = (0, 0);
            // Record latency includes the buffered unread data. This timestamps
            // the first unread sample rather than when our polling loop woke up.
            if unsafe {
                (connection.api.pa_stream_get_latency)(stream, &mut latency, &mut negative)
            } < 0
            {
                let since = timing_missing[index].get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(5) {
                    return Err(
                        "Audio server supplied no capture timing; synchronized capture stopped"
                            .into(),
                    );
                }
                continue;
            }
            timing_missing[index] = None;
            let seconds = origin.elapsed().as_secs_f64()
                + if negative != 0 {
                    latency as f64 / 1e6
                } else {
                    -(latency as f64 / 1e6)
                };
            let (mut data, mut bytes) = (ptr::null(), 0);
            if unsafe { (connection.api.pa_stream_peek)(stream, &mut data, &mut bytes) } < 0 {
                return Err(connection.error("Cannot obtain capture audio"));
            }
            if bytes == 0 {
                continue;
            }
            if bytes > 4 * 1024 * 1024 || bytes % 4 != 0 {
                return Err("Audio server returned an invalid PCM fragment".into());
            }
            let samples = if data.is_null() {
                vec![0.; bytes / 4]
            } else {
                unsafe { std::slice::from_raw_parts(data.cast::<u8>(), bytes) }
                    .chunks_exact(4)
                    .map(|sample| f32::from_le_bytes(sample.try_into().unwrap()))
                    .collect()
            };
            tracks[index].append(seconds, &samples)?;
            if unsafe { (connection.api.pa_stream_drop)(stream) } < 0 {
                return Err(connection.error("Cannot consume capture audio"));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let end = (origin.elapsed().as_secs_f64() * RATE as f64).round() as u64;
    for track in &mut tracks {
        track.pad(end)?;
    }
    Ok(())
}
