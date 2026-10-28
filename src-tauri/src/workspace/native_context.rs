//! macOS Accessibility access, bounded and opt-in. No clipboard or key events.
use super::{Destination, LearningObservation, LEARNING_LIFETIME_MS, MAX_LEARNING_TEXT_BYTES};
use crate::workspace::store;
use objc2::{
    msg_send,
    runtime::{AnyClass, AnyObject},
};
use std::{
    collections::HashMap,
    ffi::{c_char, c_void, CStr, CString},
    ptr,
    sync::{LazyLock, Mutex, OnceLock},
    time::{Duration, Instant},
};

type CF = *const c_void;
const UTF8: u32 = 0x08000100;
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CF;
    fn AXUIElementCopyAttributeValue(element: CF, attr: CF, out: *mut CF) -> i32;
    fn AXUIElementSetAttributeValue(element: CF, attr: CF, value: CF) -> i32;
    fn AXUIElementIsAttributeSettable(element: CF, attr: CF, out: *mut u8) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CF, seconds: f32) -> i32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetType(value: CF) -> u32;
    fn AXValueGetValue(value: CF, kind: u32, out: *mut c_void) -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CF);
    fn CFEqual(a: CF, b: CF) -> u8;
    fn CFGetTypeID(value: CF) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFURLGetTypeID() -> usize;
    fn CFURLGetString(value: CF) -> CF;
    fn CFStringCreateWithCString(allocator: CF, text: *const c_char, encoding: u32) -> CF;
    fn CFStringGetLength(value: CF) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(value: CF, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
}

struct Owned(CF);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
fn string(value: &str) -> Result<Owned, String> {
    let c = CString::new(value).map_err(|_| "Text contains a null character.")?;
    let value = unsafe { CFStringCreateWithCString(ptr::null(), c.as_ptr(), UTF8) };
    if value.is_null() {
        return Err("Could not allocate accessibility text.".into());
    }
    Ok(Owned(value))
}
fn attr(element: CF, key: &str) -> Result<Owned, String> {
    // Timeouts apply to this exact reference, not other equal AX objects.
    unsafe {
        AXUIElementSetMessagingTimeout(element, 0.2);
    }
    let name = string(key)?;
    let mut value = ptr::null();
    let code = unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut value) };
    if code != 0 || value.is_null() {
        return Err(format!(
            "The target does not expose {key} (accessibility error {code})."
        ));
    }
    Ok(Owned(value))
}
fn text(value: CF) -> Option<String> {
    unsafe {
        if CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), UTF8) + 1;
        if !(1..=2_000_001).contains(&size) {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if CFStringGetCString(value, buffer.as_mut_ptr().cast(), size, UTF8) == 0 {
            return None;
        }
        Some(
            CStr::from_ptr(buffer.as_ptr().cast())
                .to_string_lossy()
                .into_owned(),
        )
    }
}
fn text_attr(element: CF, key: &str) -> Option<String> {
    attr(element, key).ok().and_then(|v| text(v.0))
}
fn guard_field(element: CF) -> Result<(), String> {
    // Inspect only metadata before asking for any text. Password fields are never read.
    if text_attr(element, "AXRole").is_none() {
        return Err("The target does not expose field metadata; text access is blocked.".into());
    }
    for key in ["AXRole", "AXSubrole", "AXRoleDescription"] {
        if let Some(value) = text_attr(element, key) {
            let value = value.to_lowercase();
            if value.contains("secure") || value.contains("password") {
                return Err("Secure and password fields are excluded from text access.".into());
            }
        }
    }
    Ok(())
}
struct Target {
    element: usize,
    range: usize,
    pid: i32,
    app_id: String,
    selection: String,
    started: Instant,
}
impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.element as CF);
            CFRelease(self.range as CF);
        }
    }
}
// The AX reference is retained here and used only under this mutex. AX calls are IPC,
// not AppKit UI mutations; NSWorkspace access happens only during capture.
static TARGETS: LazyLock<Mutex<HashMap<String, Target>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct ObservationTarget {
    element: usize,
    pid: i32,
    app_id: String,
    started: Instant,
}
impl Drop for ObservationTarget {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.element as CF);
        }
    }
}
static OBSERVATIONS: LazyLock<Mutex<HashMap<String, ObservationTarget>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static EXPIRY_WORKER: OnceLock<Result<(), String>> = OnceLock::new();
fn expiry_worker() -> Result<(), String> {
    EXPIRY_WORKER
        .get_or_init(|| {
            std::thread::Builder::new()
                .name("handy-context-expiry".into())
                .spawn(|| loop {
                    // Release retained targets even when no further capture is requested.
                    // This worker reads no application content.
                    std::thread::sleep(Duration::from_secs(1));
                    if let Ok(mut targets) = TARGETS.lock() {
                        targets.retain(|_, t| t.started.elapsed() < Duration::from_secs(600));
                    }
                    if let Ok(mut observations) = OBSERVATIONS.lock() {
                        observations.retain(|_, t| {
                            t.started.elapsed() < Duration::from_millis(LEARNING_LIFETIME_MS as u64)
                        });
                    }
                })
                .map(|_| ())
                .map_err(|e| format!("Could not start local context expiry: {e}"))
        })
        .clone()
}

#[repr(C)]
struct CFRange {
    location: isize,
    length: isize,
}
fn learning_field_value(element: CF) -> Result<String, String> {
    guard_field(element)?;
    let raw = attr(element, "AXValue")?;
    unsafe {
        if CFGetTypeID(raw.0) != CFStringGetTypeID()
            || CFStringGetLength(raw.0) > MAX_LEARNING_TEXT_BYTES as isize
        {
            return Err("This field is not a supported bounded text field.".into());
        }
    }
    let value = text(raw.0).ok_or("The destination does not expose its text value.")?;
    if value.len() > MAX_LEARNING_TEXT_BYTES {
        return Err("The field exceeds the local correction observation limit.".into());
    }
    Ok(value)
}
fn learning_selection(element: CF, value: &str) -> Result<(usize, usize), String> {
    let raw = attr(element, "AXSelectedTextRange")?;
    let mut range = CFRange {
        location: 0,
        length: 0,
    };
    unsafe {
        if CFGetTypeID(raw.0) != AXValueGetTypeID()
            || AXValueGetType(raw.0) != 4
            || !AXValueGetValue(raw.0, 4, (&mut range as *mut CFRange).cast())
        {
            return Err("The destination does not expose a verifiable selection range.".into());
        }
    }
    if range.location < 0 || range.length < 0 {
        return Err("The selection range is invalid.".into());
    }
    let end = (range.location as usize)
        .checked_add(range.length as usize)
        .ok_or("The selection range is invalid.")?;
    Ok((
        super::utf16_offset_to_byte(value, range.location as usize)?,
        super::utf16_offset_to_byte(value, end)?,
    ))
}
pub fn capture_learning() -> Result<LearningObservation, String> {
    expiry_worker()?;
    if !unsafe { AXIsProcessTrusted() } {
        return Err("Accessibility permission is required for local correction learning.".into());
    }
    let (app_id, _, pid) = identity()?;
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return Err("The destination is unavailable.".into());
    }
    let element = attr(app.0, "AXFocusedUIElement")?;
    let before_text = learning_field_value(element.0)?;
    let (selection_start, selection_end) = learning_selection(element.0, &before_text)?;
    // A second read prevents attributing a moving field to the old selection.
    if learning_field_value(element.0)? != before_text {
        return Err("The destination changed while capturing the insertion point.".into());
    }
    let captured_at = store::now_ms();
    let token = store::new_id();
    let mut observations = OBSERVATIONS
        .lock()
        .map_err(|_| "Correction observation storage is unavailable.")?;
    observations
        .retain(|_, target| target.started.elapsed().as_millis() < LEARNING_LIFETIME_MS as u128);
    if observations.len() >= 8 {
        return Err("Too many local correction observations are pending.".into());
    }
    observations.insert(
        token.clone(),
        ObservationTarget {
            element: element.0 as usize,
            pid,
            app_id,
            started: Instant::now(),
        },
    );
    std::mem::forget(element);
    Ok(LearningObservation {
        token,
        before_text,
        selection_start,
        selection_end,
        captured_at,
    })
}
pub fn read_learning(observation: &LearningObservation) -> Result<String, String> {
    let observations = OBSERVATIONS
        .lock()
        .map_err(|_| "Correction observation storage is unavailable.")?;
    let target = observations
        .get(&observation.token)
        .ok_or("The correction observation has expired.")?;
    if target.started.elapsed().as_millis() >= LEARNING_LIFETIME_MS as u128 {
        return Err("The correction observation has expired.".into());
    }
    if !unsafe { AXIsProcessTrusted() } {
        return Err("Accessibility permission is no longer available.".into());
    }
    let (app_id, _, pid) = identity()?;
    if pid != target.pid || app_id != target.app_id {
        return Err("The destination application changed. Correction observation stopped.".into());
    }
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return Err("The destination is unavailable.".into());
    }
    let focused = attr(app.0, "AXFocusedUIElement")?;
    if unsafe { CFEqual(focused.0, target.element as CF) } == 0 {
        return Err("The destination field changed. Correction observation stopped.".into());
    }
    learning_field_value(focused.0)
}
pub fn discard_learning(observation: &LearningObservation) {
    if let Ok(mut observations) = OBSERVATIONS.lock() {
        observations.remove(&observation.token);
    }
}

unsafe fn ns_text(object: *mut AnyObject) -> Option<String> {
    if object.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![object, UTF8String];
    if utf8.is_null() {
        None
    } else {
        Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }
}
fn identity() -> Result<(String, String, i32), String> {
    unsafe {
        let class = AnyClass::get(c"NSWorkspace").ok_or("NSWorkspace is unavailable.")?;
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return Err("No foreground application is available.".into());
        }
        let bundle: *mut AnyObject = msg_send![app, bundleIdentifier];
        let name: *mut AnyObject = msg_send![app, localizedName];
        let pid: i32 = msg_send![app, processIdentifier];
        let id = ns_text(bundle).ok_or("The foreground application has no bundle identifier.")?;
        Ok((
            id,
            ns_text(name).unwrap_or_else(|| "Application".into()),
            pid,
        ))
    }
}
fn domain(app: CF, element: CF) -> Option<String> {
    let window = attr(app, "AXFocusedWindow").ok();
    [Some(element), window.as_ref().map(|v| v.0)]
        .into_iter()
        .flatten()
        .find_map(|node| {
            ["AXURL", "AXDocument"].into_iter().find_map(|key| {
                let value = attr(node, key).ok()?;
                let raw = unsafe {
                    if CFGetTypeID(value.0) == CFURLGetTypeID() {
                        text(CFURLGetString(value.0))
                    } else {
                        text(value.0)
                    }
                }?;
                let url = reqwest::Url::parse(&raw).ok()?;
                if !["http", "https"].contains(&url.scheme()) {
                    return None;
                }
                url.host_str().map(|host| host.to_ascii_lowercase())
            })
        })
}
pub fn capture(include_text: bool) -> Result<Destination, String> {
    let (app_id, app_name, pid) = identity()?;
    let mut destination = Destination {
        app_id,
        app_name,
        domain: None,
        selected_text: None,
        token: store::new_id(),
        captured_at: store::now_ms(),
        context_available: false,
        unavailable_reason: None,
    };
    if !unsafe { AXIsProcessTrusted() } {
        destination.unavailable_reason = Some("Allow Handy in System Settings > Privacy & Security > Accessibility to read destination metadata or selected text.".into());
        return Ok(destination);
    }
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return Err("Could not open the destination accessibility application.".into());
    }
    unsafe {
        AXUIElementSetMessagingTimeout(app.0, 0.5);
    }
    let element = match attr(app.0, "AXFocusedUIElement") {
        Ok(v) => v,
        Err(e) => {
            destination.unavailable_reason = Some(e);
            return Ok(destination);
        }
    };
    destination.domain = domain(app.0, element.0);
    if !include_text {
        return Ok(destination);
    }
    if let Err(e) = guard_field(element.0) {
        destination.unavailable_reason = Some(e);
        return Ok(destination);
    }
    let selection = match text_attr(element.0, "AXSelectedText") {
        Some(text) if !text.is_empty() => text,
        _ => {
            destination.unavailable_reason =
                Some("Select text in a supported editable field first.".into());
            return Ok(destination);
        }
    };
    let range = match attr(element.0, "AXSelectedTextRange") {
        Ok(range) => range,
        Err(_) => {
            destination.unavailable_reason =
                Some("This application does not expose a verifiable selection range.".into());
            return Ok(destination);
        }
    };
    expiry_worker()?;
    let mut targets = TARGETS
        .lock()
        .map_err(|_| "Destination storage is unavailable.")?;
    targets.retain(|_, target| target.started.elapsed() < Duration::from_secs(600));
    if targets.len() >= 64 {
        return Err("Too many pending selections. Wait for an older selection to expire.".into());
    }
    let target = Target {
        element: element.0 as usize,
        range: range.0 as usize,
        pid,
        app_id: destination.app_id.clone(),
        selection: selection.clone(),
        started: Instant::now(),
    };
    std::mem::forget(element); // ownership transfers to Target, which releases it
    std::mem::forget(range);
    targets.insert(destination.token.clone(), target);
    destination.selected_text = Some(selection);
    destination.context_available = true;
    Ok(destination)
}
fn checked_target<'a>(
    targets: &'a HashMap<String, Target>,
    destination: &Destination,
) -> Result<&'a Target, String> {
    let target = targets
        .get(&destination.token)
        .ok_or("This selection has expired. Capture it again.")?;
    if target.started.elapsed() >= Duration::from_secs(600) || target.app_id != destination.app_id {
        return Err("This selection has expired. Capture it again.".into());
    }
    let app = Owned(unsafe { AXUIElementCreateApplication(target.pid) });
    unsafe {
        AXUIElementSetMessagingTimeout(app.0, 0.5);
    }
    let focused = attr(app.0, "AXFocusedUIElement")?;
    if unsafe { CFEqual(focused.0, target.element as CF) } == 0 {
        return Err("The original field changed. Capture the selection again.".into());
    }
    guard_field(focused.0)?;
    let range = attr(focused.0, "AXSelectedTextRange")?;
    if unsafe { CFEqual(range.0, target.range as CF) } == 0 {
        return Err("The selection range changed. Capture it again.".into());
    }
    if text_attr(focused.0, "AXSelectedText").as_deref() != Some(target.selection.as_str()) {
        return Err("The original selection changed. Capture it again.".into());
    }
    Ok(target)
}
pub fn check(destination: &Destination) -> Result<(), String> {
    let targets = TARGETS
        .lock()
        .map_err(|_| "Destination storage is unavailable.")?;
    checked_target(&targets, destination).map(|_| ())
}
pub fn replace(destination: &Destination, replacement: &str) -> Result<(), String> {
    let mut targets = TARGETS
        .lock()
        .map_err(|_| "Destination storage is unavailable.")?;
    let target = checked_target(&targets, destination)?;
    let name = string("AXSelectedText")?;
    let mut settable = 0;
    let code =
        unsafe { AXUIElementIsAttributeSettable(target.element as CF, name.0, &mut settable) };
    if code != 0 || settable == 0 {
        return Err("This application does not support safe selection replacement.".into());
    }
    let replacement = string(replacement)?;
    // An IPC timeout cannot prove that the destination did not apply a write.
    // Consume before dispatch so an ambiguous result cannot replay this token.
    let target = targets
        .remove(&destination.token)
        .ok_or("This selection has expired. Capture it again.")?;
    let code = unsafe { AXUIElementSetAttributeValue(target.element as CF, name.0, replacement.0) };
    if code != 0 {
        return Err(format!(
            "The destination did not confirm the replacement (accessibility error {code}). Check the original field before capturing it again."
        ));
    }
    Ok(())
}
