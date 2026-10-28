//! AT-SPI2 access for X11 and Wayland applications that expose accessibility.
//! All foreign calls and retained GObjects stay on one worker thread.
use super::{Destination, LearningObservation};
use crate::workspace::store;
use std::{
    collections::HashMap,
    ffi::{c_char, c_void, CStr, CString},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, OnceLock,
    },
    time::{Duration, Instant},
};

type Obj = *mut c_void;
type ErrorOut = *mut Obj;
const MAX_FIELD: usize = 65_536;
const MAX_TARGETS: usize = 64;
const ACTIVE: i32 = 1;
const FOCUSED: i32 = 12;
const PASSWORD_TEXT: i32 = 40;
const DEFUNCT: i32 = 6;

#[repr(C)]
struct Range {
    start: i32,
    end: i32,
}

macro_rules! api {
    ($($name:ident : $ty:ty = $symbol:literal),+ $(,)?) => {
        struct Api { $($name: $ty),+ }
        impl Api {
            fn load() -> Result<Self, String> {
                // Keep the library loaded for the process lifetime. No pointer escapes
                // this worker, and no symbol is called after unloading.
                let handle = unsafe { libc::dlopen(c"libatspi.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if handle.is_null() { return Err("AT-SPI2 is unavailable. Install libatspi2.0, enable desktop accessibility, then restart Handy. Wayland apps must expose the AT-SPI accessibility bus.".into()); }
                $(let $name: $ty = unsafe {
                    let symbol = libc::dlsym(handle, concat!($symbol, "\0").as_ptr().cast());
                    if symbol.is_null() { return Err(concat!("AT-SPI2 lacks required API: ", $symbol, ". Update libatspi2.0, then restart Handy.").into()); }
                    std::mem::transmute::<*mut c_void, $ty>(symbol)
                };)+
                Ok(Self { $($name),+ })
            }
        }
    };
}
api! {
    init: unsafe extern "C" fn() -> i32 = "atspi_init",
    timeout: unsafe extern "C" fn(i32,i32) = "atspi_set_timeout",
    desktop: unsafe extern "C" fn(i32) -> Obj = "atspi_get_desktop",
    clear: unsafe extern "C" fn(Obj) = "atspi_accessible_clear_cache_single",
    child_count: unsafe extern "C" fn(Obj,ErrorOut)->i32 = "atspi_accessible_get_child_count",
    child: unsafe extern "C" fn(Obj,i32,ErrorOut)->Obj = "atspi_accessible_get_child_at_index",
    parent: unsafe extern "C" fn(Obj,ErrorOut)->Obj = "atspi_accessible_get_parent",
    application: unsafe extern "C" fn(Obj,ErrorOut)->Obj = "atspi_accessible_get_application",
    name: unsafe extern "C" fn(Obj,ErrorOut)->*mut c_char = "atspi_accessible_get_name",
    pid: unsafe extern "C" fn(Obj,ErrorOut)->u32 = "atspi_accessible_get_process_id",
    role: unsafe extern "C" fn(Obj,ErrorOut)->i32 = "atspi_accessible_get_role",
    states: unsafe extern "C" fn(Obj)->Obj = "atspi_accessible_get_state_set",
    contains: unsafe extern "C" fn(Obj,i32)->i32 = "atspi_state_set_contains",
    text: unsafe extern "C" fn(Obj)->Obj = "atspi_accessible_get_text_iface",
    editable: unsafe extern "C" fn(Obj)->Obj = "atspi_accessible_get_editable_text_iface",
    document: unsafe extern "C" fn(Obj)->Obj = "atspi_accessible_get_document_iface",
    document_attr: unsafe extern "C" fn(Obj,*const c_char,ErrorOut)->*mut c_char = "atspi_document_get_document_attribute_value",
    char_count: unsafe extern "C" fn(Obj,ErrorOut)->i32 = "atspi_text_get_character_count",
    get_text: unsafe extern "C" fn(Obj,i32,i32,ErrorOut)->*mut c_char = "atspi_text_get_text",
    selection_count: unsafe extern "C" fn(Obj,ErrorOut)->i32 = "atspi_text_get_n_selections",
    selection: unsafe extern "C" fn(Obj,i32,ErrorOut)->*mut Range = "atspi_text_get_selection",
    caret: unsafe extern "C" fn(Obj,ErrorOut)->i32 = "atspi_text_get_caret_offset",
    set_contents: unsafe extern "C" fn(Obj,*const c_char,ErrorOut)->i32 = "atspi_editable_text_set_text_contents",
    free: unsafe extern "C" fn(Obj) = "g_free",
    error_free: unsafe extern "C" fn(Obj) = "g_error_free",
    unref: unsafe extern "C" fn(Obj) = "g_object_unref",
    ref_object: unsafe extern "C" fn(Obj)->Obj = "g_object_ref",
}

struct Ref<'a> {
    api: &'a Api,
    object: Obj,
}
impl Drop for Ref<'_> {
    fn drop(&mut self) {
        unsafe { (self.api.unref)(self.object) }
    }
}
impl Api {
    fn object(&self, object: Obj) -> Result<Ref<'_>, String> {
        if object.is_null() {
            Err("The application does not expose the required accessibility interface.".into())
        } else {
            Ok(Ref { api: self, object })
        }
    }
    fn call<T>(&self, f: impl FnOnce(ErrorOut) -> T) -> Result<T, String> {
        let mut error = ptr::null_mut();
        let result = f(&mut error);
        if !error.is_null() {
            unsafe { (self.error_free)(error) };
            return Err("The application did not answer its accessibility request. Enable its accessibility support and try again.".into());
        }
        Ok(result)
    }
    fn owned_string(&self, value: *mut c_char) -> Result<String, String> {
        if value.is_null() {
            return Err("The accessibility text value is unavailable.".into());
        }
        let result = unsafe {
            CStr::from_ptr(value)
                .to_str()
                .map(str::to_owned)
                .map_err(|_| "The application returned invalid UTF-8.".to_owned())
        };
        unsafe { (self.free)(value.cast()) };
        result
    }
    fn state(&self, object: Obj, state: i32) -> Result<bool, String> {
        unsafe { (self.clear)(object) };
        let states = self.object(unsafe { (self.states)(object) })?;
        Ok(unsafe { (self.contains)(states.object, state) } != 0)
    }
    fn guard(&self, object: Obj) -> Result<(), String> {
        let role = self.call(|error| unsafe { (self.role)(object, error) })?;
        if role == 0 || role == PASSWORD_TEXT {
            return Err("Password and unidentified fields are excluded from text access.".into());
        }
        if self.state(object, DEFUNCT)? {
            return Err("The original accessibility field no longer exists.".into());
        }
        Ok(())
    }
    fn focused(&self) -> Result<Ref<'_>, String> {
        let desktop = self.object(unsafe { (self.desktop)(0) })?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut budget = 1500usize;
        self.walk(desktop.object, false, 0, &mut budget, deadline)?.ok_or_else(|| "No focused AT-SPI field is available. Enable desktop and application accessibility. Some Wayland compositors and sandboxed apps do not expose the active field.".into())
    }
    fn walk(
        &self,
        node: Obj,
        active: bool,
        depth: usize,
        budget: &mut usize,
        deadline: Instant,
    ) -> Result<Option<Ref<'_>>, String> {
        if depth > 32 || *budget == 0 || Instant::now() > deadline {
            return Ok(None);
        }
        *budget -= 1;
        let active = active || self.state(node, ACTIVE).unwrap_or(false);
        if active && self.state(node, FOCUSED).unwrap_or(false) {
            // get_child_at_index and get_parent return retained proxy references.
            // Retain this node explicitly while the recursion releases its owner.
            unsafe {
                (self.ref_object)(node);
            }
            return self.object(node).map(Some);
        }
        let count = self
            .call(|e| unsafe { (self.child_count)(node, e) })
            .unwrap_or(0)
            .clamp(0, 500);
        for index in 0..count {
            if Instant::now() > deadline || *budget == 0 {
                break;
            }
            let child = match self
                .call(|e| unsafe { (self.child)(node, index, e) })
                .and_then(|p| self.object(p))
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Some(found) = self.walk(child.object, active, depth + 1, budget, deadline)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    fn field(&self, object: Obj, require_selection: bool) -> Result<(String, i32, i32), String> {
        self.guard(object)?;
        let text = self.object(unsafe { (self.text)(object) })?;
        let count = self.call(|e| unsafe { (self.char_count)(text.object, e) })?;
        if count < 0 || count as usize > MAX_FIELD {
            return Err("The field exceeds the 64 KiB local text limit.".into());
        }
        let selections = self.call(|e| unsafe { (self.selection_count)(text.object, e) })?;
        let (start, end) = if selections == 1 {
            let range = self.call(|e| unsafe { (self.selection)(text.object, 0, e) })?;
            if range.is_null() {
                return Err("The selected text range is unavailable.".into());
            }
            let result = unsafe { ((*range).start, (*range).end) };
            unsafe { (self.free)(range.cast()) };
            result
        } else if selections == 0 && !require_selection {
            let caret = self.call(|e| unsafe { (self.caret)(text.object, e) })?;
            (caret, caret)
        } else {
            return Err("Select one text range in a supported editable field.".into());
        };
        if start < 0 || end < start || end > count || (require_selection && start == end) {
            return Err("The application returned an invalid selection.".into());
        }
        let full = self
            .owned_string(self.call(|e| unsafe { (self.get_text)(text.object, 0, count, e) })?)?;
        if full.len() > MAX_FIELD {
            return Err("The field exceeds the 64 KiB local text limit.".into());
        }
        offsets(&full, start, end)?;
        Ok((full, start, end))
    }
    fn identity(&self, object: Obj) -> Result<(String, String), String> {
        let app = self.object(self.call(|e| unsafe { (self.application)(object, e) })?)?;
        let pid = self.call(|e| unsafe { (self.pid)(app.object, e) })?;
        if pid == 0 {
            return Err("The destination process identity is unavailable.".into());
        }
        let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map_err(|_| "The destination executable identity is unavailable.".to_owned())?;
        let name = self
            .owned_string(self.call(|e| unsafe { (self.name)(app.object, e) })?)
            .unwrap_or_else(|_| {
                executable
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        Ok((
            format!("linux-executable:{}", executable.to_string_lossy()),
            name,
        ))
    }
    fn domain(&self, object: Obj) -> Option<String> {
        unsafe { (self.ref_object)(object) };
        let mut current = self.object(object).ok()?;
        for _ in 0..24 {
            if let Ok(document) = self.object(unsafe { (self.document)(current.object) }) {
                for key in [c"DocURL", c"DocumentURL", c"URI"] {
                    if let Ok(raw) = self
                        .call(|e| unsafe { (self.document_attr)(document.object, key.as_ptr(), e) })
                        .and_then(|s| self.owned_string(s))
                    {
                        if let Ok(url) = reqwest::Url::parse(&raw) {
                            if ["http", "https"].contains(&url.scheme()) {
                                if let Some(host) = url.host_str() {
                                    return Some(host.to_ascii_lowercase());
                                }
                            }
                        }
                    }
                }
            }
            current = self
                .object(
                    self.call(|e| unsafe { (self.parent)(current.object, e) })
                        .ok()?,
                )
                .ok()?;
        }
        None
    }
}

fn offsets(text: &str, start: i32, end: i32) -> Result<(usize, usize), String> {
    let byte = |offset: i32| {
        if offset < 0 {
            None
        } else {
            text.char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
                .nth(offset as usize)
        }
    };
    let start = byte(start).ok_or("Invalid Unicode selection offset.")?;
    let end = byte(end).ok_or("Invalid Unicode selection offset.")?;
    if end < start {
        return Err("Invalid Unicode selection range.".into());
    }
    Ok((start, end))
}
fn replacement_value(
    original: &str,
    start: i32,
    end: i32,
    replacement: &str,
) -> Result<CString, String> {
    let (a, b) = offsets(original, start, end)?;
    let value = format!("{}{}{}", &original[..a], replacement, &original[b..]);
    if value.len() > MAX_FIELD {
        return Err("The replacement would exceed the 64 KiB field limit.".into());
    }
    CString::new(value).map_err(|_| "Replacement contains a null character.".into())
}
fn validate_selection(
    original: &str,
    start: i32,
    end: i32,
    current: &str,
    current_start: i32,
    current_end: i32,
) -> Result<(), String> {
    if original != current || start != current_start || end != current_end {
        return Err("The original field or selection changed. Capture it again.".into());
    }
    Ok(())
}

struct Target {
    object: Obj,
    app_id: String,
    text: String,
    start: i32,
    end: i32,
    expires: Instant,
    learning: bool,
}
enum Request {
    Capture(bool, mpsc::Sender<Result<Destination, String>>),
    Check(Destination, mpsc::Sender<Result<(), String>>),
    Replace(Destination, String, mpsc::Sender<Result<(), String>>),
    Learn(mpsc::Sender<Result<LearningObservation, String>>),
    Read(LearningObservation, mpsc::Sender<Result<String, String>>),
    Discard(String),
}
struct Envelope {
    request: Request,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
static WORKER: OnceLock<mpsc::SyncSender<Envelope>> = OnceLock::new();
fn sender() -> &'static mpsc::SyncSender<Envelope> {
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(16);
        std::thread::spawn(move || worker(rx));
        tx
    })
}
fn request<T>(make: impl FnOnce(mpsc::Sender<Result<T, String>>) -> Request) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    sender()
        .try_send(Envelope {
            request: make(tx),
            cancelled: cancelled.clone(),
            deadline: Instant::now() + Duration::from_secs(8),
        })
        .map_err(|_| "The accessibility worker is busy or unavailable.".to_owned())?;
    // Every IPC request has a short timeout, and tree discovery has a deadline.
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(result) => result,
        Err(_) => {
            cancelled.store(true, Ordering::Release);
            Err("The accessibility request timed out; no fallback paste was attempted.".into())
        }
    }
}
fn worker(rx: mpsc::Receiver<Envelope>) {
    let api = Api::load().and_then(|api| {
        unsafe { (api.timeout)(350, 350) };
        if unsafe { (api.init)() } != 0 {
            Err(
                "Could not connect to the AT-SPI accessibility bus. Enable desktop accessibility, then restart Handy to reconnect."
                    .into(),
            )
        } else {
            Ok(api)
        }
    });
    let mut targets: HashMap<String, Target> = HashMap::new();
    loop {
        if let Ok(api) = &api {
            targets.retain(|_, t| {
                let keep = Instant::now() < t.expires;
                if !keep {
                    unsafe { (api.unref)(t.object) }
                }
                keep
            });
        }
        let Envelope {
            request,
            cancelled,
            deadline,
        } = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(envelope) => envelope,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            continue;
        }
        let api = match &api {
            Ok(api) => api,
            Err(error) => {
                match request {
                    Request::Capture(_, tx) => {
                        let _ = tx.send(Err(error.clone()));
                    }
                    Request::Check(_, tx) | Request::Replace(_, _, tx) => {
                        let _ = tx.send(Err(error.clone()));
                    }
                    Request::Learn(tx) => {
                        let _ = tx.send(Err(error.clone()));
                    }
                    Request::Read(_, tx) => {
                        let _ = tx.send(Err(error.clone()));
                    }
                    Request::Discard(_) => {}
                }
                continue;
            }
        };
        match request {
            Request::Capture(include, tx) => {
                let _ = tx.send(capture_inner(api, &mut targets, include));
            }
            Request::Check(d, tx) => {
                let _ = tx.send(checked(api, &targets, &d.token, false).map(|_| ()));
            }
            Request::Replace(d, text, tx) => {
                let _ = tx.send(replace_inner(
                    api,
                    &mut targets,
                    &d,
                    &text,
                    &cancelled,
                    deadline,
                ));
            }
            Request::Learn(tx) => {
                let _ = tx.send(learning_inner(api, &mut targets));
            }
            Request::Read(o, tx) => {
                let _ = tx.send(
                    checked(api, &targets, &o.token, true)
                        .and_then(|t| api.field(t.object, false).map(|f| f.0)),
                );
            }
            Request::Discard(token) => {
                if let Some(t) = targets.remove(&token) {
                    unsafe { (api.unref)(t.object) }
                }
            }
        }
    }
}
fn capture_inner(
    api: &Api,
    targets: &mut HashMap<String, Target>,
    include: bool,
) -> Result<Destination, String> {
    let focused = api.focused()?;
    let (app_id, app_name) = api.identity(focused.object)?;
    let mut d = Destination {
        app_id,
        app_name,
        domain: api.domain(focused.object),
        selected_text: None,
        token: store::new_id(),
        captured_at: store::now_ms(),
        context_available: false,
        unavailable_reason: None,
    };
    if !include {
        return Ok(d);
    }
    let (full, start, end) = match api.field(focused.object, true) {
        Ok(value) => value,
        Err(e) => {
            d.unavailable_reason = Some(e);
            return Ok(d);
        }
    };
    if targets.len() >= MAX_TARGETS {
        return Err("Too many pending accessibility selections.".into());
    }
    let (a, b) = offsets(&full, start, end)?;
    d.selected_text = Some(full[a..b].into());
    d.context_available = true;
    unsafe { (api.ref_object)(focused.object) };
    targets.insert(
        d.token.clone(),
        Target {
            object: focused.object,
            app_id: d.app_id.clone(),
            text: full,
            start,
            end,
            expires: Instant::now() + Duration::from_secs(600),
            learning: false,
        },
    );
    Ok(d)
}
fn checked<'a>(
    api: &Api,
    targets: &'a HashMap<String, Target>,
    token: &str,
    learning: bool,
) -> Result<&'a Target, String> {
    let target = targets
        .get(token)
        .filter(|t| t.learning == learning && Instant::now() < t.expires)
        .ok_or("This accessibility session expired. Capture the field again.")?;
    if learning {
        let focused = api.focused()?;
        if focused.object != target.object || api.identity(focused.object)?.0 != target.app_id {
            return Err("The foreground destination changed. Capture the field again.".into());
        }
    } else if api.identity(target.object)?.0 != target.app_id {
        return Err("The original destination changed. Capture the field again.".into());
    }
    api.guard(target.object)?;
    if !learning {
        let (full, start, end) = api.field(target.object, true)?;
        validate_selection(&target.text, target.start, target.end, &full, start, end)?;
    }
    Ok(target)
}
fn replace_inner(
    api: &Api,
    targets: &mut HashMap<String, Target>,
    d: &Destination,
    replacement: &str,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<(), String> {
    let target = checked(api, targets, &d.token, false)?;
    if d.app_id != target.app_id {
        return Err("The destination identity does not match this session.".into());
    }
    let value = replacement_value(&target.text, target.start, target.end, replacement)?;
    let editable = api.object(unsafe { (api.editable)(target.object) })?;
    if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err("This replacement request expired before it could be applied.".into());
    }
    if api.call(|e| unsafe { (api.set_contents)(editable.object, value.as_ptr(), e) })? == 0 {
        return Err("The application rejected text replacement.".into());
    }
    if let Some(t) = targets.remove(&d.token) {
        unsafe { (api.unref)(t.object) }
    }
    Ok(())
}
fn learning_inner(
    api: &Api,
    targets: &mut HashMap<String, Target>,
) -> Result<LearningObservation, String> {
    let focused = api.focused()?;
    let (full, start, end) = api.field(focused.object, false)?;
    let (app_id, _) = api.identity(focused.object)?;
    let (a, b) = offsets(&full, start, end)?;
    if targets.len() >= MAX_TARGETS {
        return Err("Too many pending accessibility sessions.".into());
    }
    let observation = LearningObservation {
        token: store::new_id(),
        before_text: full.clone(),
        selection_start: a,
        selection_end: b,
        captured_at: store::now_ms(),
    };
    unsafe { (api.ref_object)(focused.object) };
    targets.insert(
        observation.token.clone(),
        Target {
            object: focused.object,
            app_id,
            text: full,
            start,
            end,
            expires: Instant::now() + Duration::from_secs(90),
            learning: true,
        },
    );
    Ok(observation)
}
pub fn capture(include: bool) -> Result<Destination, String> {
    request(|tx| Request::Capture(include, tx))
}
pub fn check(d: &Destination) -> Result<(), String> {
    request(|tx| Request::Check(d.clone(), tx))
}
pub fn replace(d: &Destination, text: &str) -> Result<(), String> {
    request(|tx| Request::Replace(d.clone(), text.to_owned(), tx))
}
pub fn capture_learning() -> Result<LearningObservation, String> {
    request(Request::Learn)
}
pub fn read_learning(o: &LearningObservation) -> Result<String, String> {
    request(|tx| Request::Read(o.clone(), tx))
}
pub fn discard_learning(o: &LearningObservation) {
    let _ = sender().try_send(Envelope {
        request: Request::Discard(o.token.clone()),
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + Duration::from_secs(8),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_ranges_use_character_offsets_not_bytes() {
        assert_eq!(offsets("Aé🙂Z", 1, 3).unwrap(), (1, 7));
        assert!(offsets("é", 0, 2).is_err());
        assert!(offsets("abc", 2, 1).is_err());
    }
    #[test]
    fn replacement_preserves_surrounding_unicode_and_rejects_invalid_output() {
        assert_eq!(
            replacement_value("pré 🙂 suffix", 4, 5, "ready")
                .unwrap()
                .to_str()
                .unwrap(),
            "pré ready suffix"
        );
        assert!(replacement_value("text", 0, 4, "null\0text").is_err());
        assert!(replacement_value("text", 0, 4, &"é".repeat(MAX_FIELD)).is_err());
    }
    #[test]
    fn edits_outside_selection_and_moved_identical_selection_are_stale() {
        assert!(validate_selection("cat dog", 0, 3, "cat changed", 0, 3).is_err());
        assert!(validate_selection("cat cat", 0, 3, "cat cat", 4, 7).is_err());
        assert!(validate_selection("cat dog", 0, 3, "cat dog", 0, 3).is_ok());
    }
}
