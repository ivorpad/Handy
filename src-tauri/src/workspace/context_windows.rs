//! UI Automation lives on one MTA thread. COM objects never cross apartments.
//! Replacement is supported only for plain ValuePattern fields whose complete
//! value and TextPattern selection can both be verified. No input is submitted.
use super::{Destination, LearningObservation};
use crate::workspace::store;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, BSTR, PWSTR},
    Win32::{
        Foundation::CloseHandle,
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
        UI::{
            Accessibility::{
                CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement,
                IUIAutomationLegacyIAccessiblePattern, IUIAutomationTextPattern,
                IUIAutomationTextRange, IUIAutomationValuePattern, TextPatternRangeEndpoint_End,
                TextPatternRangeEndpoint_Start, UIA_DocumentControlTypeId,
                UIA_LegacyIAccessiblePatternId, UIA_TextPatternId, UIA_ValuePatternId,
            },
            WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        },
    },
};

const FIELD_LIMIT: usize = 65_536;
const SELECTION_TTL: i64 = 600_000;
const LEARNING_TTL: i64 = 90_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);

struct Target {
    element: IUIAutomationElement,
    range: IUIAutomationTextRange,
    app_id: String,
    pid: u32,
    before: String,
    prefix: String,
    selected: String,
    suffix: String,
    captured: Instant,
    learning: bool,
}
enum Request {
    Capture(bool),
    Check(Destination),
    Replace(Destination, String),
    CaptureLearning,
    ReadLearning(LearningObservation),
    DiscardLearning(String),
}
enum Response {
    Destination(Destination),
    Learning(LearningObservation),
    Text(String),
    Empty,
}
struct Envelope {
    request: Request,
    reply: mpsc::Sender<Result<Response, String>>,
    expires: Instant,
    cancelled: Arc<AtomicBool>,
}
static WORKER: OnceLock<Result<SyncSender<Envelope>, String>> = OnceLock::new();
fn worker() -> Result<&'static SyncSender<Envelope>, String> {
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel::<Envelope>(8);
            std::thread::Builder::new()
                .name("handy-uia-context".into())
                .spawn(move || {
                    // Microsoft recommends an independent MTA for desktop UIA clients.
                    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() };
                    let automation: Result<IUIAutomation, String> = initialized
                        .as_ref()
                        .map_err(|e| format!("UI Automation COM initialization failed: {e}"))
                        .and_then(|_| {
                            unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
                                .map_err(|e| format!("UI Automation is unavailable: {e}"))
                        })
                        .and_then(|automation: IUIAutomation| {
                            let bounded: IUIAutomation2 = automation.cast().map_err(|e| {
                                format!("Bounded UI Automation is unavailable: {e}")
                            })?;
                            unsafe {
                                bounded
                                    .SetConnectionTimeout(500)
                                    .map_err(|e| e.to_string())?;
                                bounded
                                    .SetTransactionTimeout(500)
                                    .map_err(|e| e.to_string())?;
                                bounded.SetAutoSetFocus(false).map_err(|e| e.to_string())?;
                            }
                            Ok(automation)
                        });
                    let mut targets = HashMap::new();
                    loop {
                        prune(&mut targets);
                        let envelope = match rx.recv_timeout(Duration::from_secs(1)) {
                            Ok(envelope) => envelope,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        let result = match &automation {
                            Ok(automation) => process(automation, &mut targets, &envelope),
                            Err(error) => Err(error.clone()),
                        };
                        let _ = envelope.reply.send(result);
                    }
                    drop(targets);
                    drop(automation);
                    if initialized.is_ok() {
                        unsafe {
                            CoUninitialize();
                        }
                    }
                })
                .map_err(|e| format!("Could not start UI Automation worker: {e}"))?;
            Ok(tx)
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn request(request: Request) -> Result<Response, String> {
    let replacing = matches!(&request, Request::Replace(_, _));
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    worker()?
        .try_send(Envelope {
            request,
            reply: tx,
            expires: Instant::now() + REQUEST_TIMEOUT,
            cancelled: cancelled.clone(),
        })
        .map_err(|_| "UI Automation is busy or unavailable. Try again.".to_owned())?;
    match rx.recv_timeout(REQUEST_TIMEOUT) {
        Ok(result) => result,
        Err(_) => {
            cancelled.store(true, Ordering::Release);
            if replacing {
                Err("Replacement could not be confirmed. Check the original field before retrying with a new capture.".into())
            } else {
                Err("The destination did not respond to UI Automation in time.".into())
            }
        }
    }
}
fn active(envelope: &Envelope) -> Result<(), String> {
    if envelope.cancelled.load(Ordering::Acquire) || Instant::now() >= envelope.expires {
        Err("The accessibility request expired.".into())
    } else {
        Ok(())
    }
}
fn identity() -> Result<(String, String, u32), String> {
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return Err("No foreground application is available.".into());
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| format!("The foreground process cannot be inspected: {e}"))?;
        let mut path = vec![0u16; 32_768];
        let mut length = path.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        result.map_err(|e| format!("The foreground executable identity is unavailable: {e}"))?;
        let path = String::from_utf16_lossy(&path[..length as usize]);
        let name = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("Application")
            .trim_end_matches(".exe")
            .to_owned();
        Ok((path.to_lowercase(), name, pid))
    }
}
fn guard(element: &IUIAutomationElement) -> Result<(), String> {
    unsafe {
        // Failure to retrieve security metadata blocks text access.
        if element
            .CurrentIsPassword()
            .map_err(|_| "The target does not expose password protection metadata.")?
            .as_bool()
        {
            return Err("Password and secure fields are excluded from text access.".into());
        }
        if !element
            .CurrentIsEnabled()
            .map_err(|e| e.to_string())?
            .as_bool()
        {
            return Err("The destination field is disabled.".into());
        }
    }
    Ok(())
}
fn browser_domain(automation: &IUIAutomation, app_id: &str, pid: u32) -> Option<String> {
    // MSAA browser document values expose the page URL. Do not read edit values
    // or search arbitrary application trees for something resembling a URL.
    if ![
        "chrome.exe",
        "msedge.exe",
        "firefox.exe",
        "brave.exe",
        "opera.exe",
        "vivaldi.exe",
    ]
    .iter()
    .any(|exe| app_id.rsplit('\\').next() == Some(*exe))
    {
        return None;
    }
    unsafe {
        let mut element = automation.GetFocusedElement().ok()?;
        let walker = automation.ControlViewWalker().ok()?;
        for _ in 0..12 {
            if element.CurrentProcessId().ok()? as u32 != pid {
                return None;
            }
            if element.CurrentControlType().ok()? == UIA_DocumentControlTypeId {
                let legacy: IUIAutomationLegacyIAccessiblePattern = element
                    .GetCurrentPatternAs(UIA_LegacyIAccessiblePatternId)
                    .ok()?;
                let raw = legacy.CurrentValue().ok()?.to_string();
                if raw.len() > 8192 {
                    return None;
                }
                let url = reqwest::Url::parse(&raw).ok()?;
                return if ["http", "https"].contains(&url.scheme()) {
                    url.host_str().map(str::to_ascii_lowercase)
                } else {
                    None
                };
            }
            element = walker.GetParentElement(&element).ok()?;
        }
    }
    None
}
fn bounded(text: BSTR) -> Result<String, String> {
    let text = String::from_utf16(&text)
        .map_err(|_| "The field contains invalid Unicode; text access is blocked.".to_owned())?;
    if text.len() > FIELD_LIMIT {
        return Err("The field exceeds the 64 KiB observation limit.".into());
    }
    Ok(text)
}
fn text_pattern(element: &IUIAutomationElement) -> Result<IUIAutomationTextPattern, String> {
    unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }
        .map_err(|_| "This field does not expose a UI Automation TextPattern selection.".into())
}
fn value_pattern(element: &IUIAutomationElement) -> Result<IUIAutomationValuePattern, String> {
    unsafe { element.GetCurrentPatternAs(UIA_ValuePatternId) }
        .map_err(|_| "This field does not support safe ValuePattern text replacement.".into())
}
fn selection(pattern: &IUIAutomationTextPattern) -> Result<IUIAutomationTextRange, String> {
    unsafe {
        let ranges = pattern.GetSelection().map_err(|e| e.to_string())?;
        if ranges.Length().map_err(|e| e.to_string())? != 1 {
            return Err("Select one contiguous text range first.".into());
        }
        ranges.GetElement(0).map_err(|e| e.to_string())
    }
}
fn capture_target(
    automation: &IUIAutomation,
    app_id: String,
    pid: u32,
    learning: bool,
) -> Result<Target, String> {
    unsafe {
        let element = automation
            .GetFocusedElement()
            .map_err(|e| format!("No accessible focused field is available: {e}"))?;
        guard(&element)?;
        if element.CurrentProcessId().map_err(|e| e.to_string())? as u32 != pid {
            return Err("The focused field changed during capture.".into());
        }
        let pattern = text_pattern(&element)?;
        let range = selection(&pattern)?;
        let document = pattern.DocumentRange().map_err(|e| e.to_string())?;
        let before = bounded(
            document
                .GetText(FIELD_LIMIT as i32 + 1)
                .map_err(|e| e.to_string())?,
        )?;
        let selected = bounded(
            range
                .GetText(FIELD_LIMIT as i32 + 1)
                .map_err(|e| e.to_string())?,
        )?;
        if !learning && selected.is_empty() {
            return Err("Select text in a supported editable field first.".into());
        }
        let prefix_range = document.Clone().map_err(|e| e.to_string())?;
        prefix_range
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &range,
                TextPatternRangeEndpoint_Start,
            )
            .map_err(|e| e.to_string())?;
        let suffix_range = document.Clone().map_err(|e| e.to_string())?;
        suffix_range
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                &range,
                TextPatternRangeEndpoint_End,
            )
            .map_err(|e| e.to_string())?;
        let prefix = bounded(
            prefix_range
                .GetText(FIELD_LIMIT as i32 + 1)
                .map_err(|e| e.to_string())?,
        )?;
        let suffix = bounded(
            suffix_range
                .GetText(FIELD_LIMIT as i32 + 1)
                .map_err(|e| e.to_string())?,
        )?;
        if format!("{prefix}{selected}{suffix}") != before {
            return Err("The field exposes inconsistent selection offsets.".into());
        }
        Ok(Target {
            element,
            range,
            app_id,
            pid,
            before,
            prefix,
            selected,
            suffix,
            captured: Instant::now(),
            learning,
        })
    }
}
fn verify(target: &Target) -> Result<(), String> {
    guard(&target.element)?;
    unsafe {
        if target
            .element
            .CurrentProcessId()
            .map_err(|e| e.to_string())? as u32
            != target.pid
        {
            return Err("The original target application changed.".into());
        }
        let pattern = text_pattern(&target.element)?;
        let current = selection(&pattern)?;
        if current
            .CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &target.range,
                TextPatternRangeEndpoint_Start,
            )
            .map_err(|e| e.to_string())?
            != 0
            || current
                .CompareEndpoints(
                    TextPatternRangeEndpoint_End,
                    &target.range,
                    TextPatternRangeEndpoint_End,
                )
                .map_err(|e| e.to_string())?
                != 0
            || bounded(
                current
                    .GetText(FIELD_LIMIT as i32 + 1)
                    .map_err(|e| e.to_string())?,
            )? != target.selected
            || bounded(
                pattern
                    .DocumentRange()
                    .map_err(|e| e.to_string())?
                    .GetText(FIELD_LIMIT as i32 + 1)
                    .map_err(|e| e.to_string())?,
            )? != target.before
        {
            return Err("The original field or selection changed. Capture it again.".into());
        }
    }
    Ok(())
}
fn process(
    automation: &IUIAutomation,
    targets: &mut HashMap<String, Target>,
    envelope: &Envelope,
) -> Result<Response, String> {
    active(envelope)?;
    let now = store::now_ms();
    prune(targets);
    match &envelope.request {
        Request::Capture(include_text) => {
            let (app_id, app_name, pid) = identity()?;
            let mut destination = Destination {
                app_id: app_id.clone(),
                app_name,
                domain: browser_domain(automation, &app_id, pid),
                selected_text: None,
                token: store::new_id(),
                captured_at: now,
                context_available: false,
                unavailable_reason: None,
            };
            if *include_text {
                if targets.len() >= 64 {
                    return Err(
                        "Too many pending text targets. Wait for a target to expire.".into(),
                    );
                }
                match capture_target(automation, app_id, pid, false) {
                    Ok(target) => {
                        destination.selected_text = Some(target.selected.clone());
                        destination.context_available = true;
                        targets.insert(destination.token.clone(), target);
                    }
                    Err(e) => destination.unavailable_reason = Some(e),
                }
            }
            Ok(Response::Destination(destination))
        }
        Request::Check(destination) | Request::Replace(destination, _) => {
            let target = targets
                .get(&destination.token)
                .filter(|t| !t.learning && t.app_id == destination.app_id)
                .ok_or("This selection has expired. Capture it again.")?;
            verify(target)?;
            if let Request::Replace(_, replacement) = &envelope.request {
                let value = value_pattern(&target.element)?;
                unsafe {
                    if value
                        .CurrentIsReadOnly()
                        .map_err(|e| e.to_string())?
                        .as_bool()
                    {
                        return Err("The original field is read-only.".into());
                    }
                    if bounded(value.CurrentValue().map_err(|e| e.to_string())?)? != target.before {
                        return Err("The field value disagrees with its text selection. Replacement is blocked.".into());
                    }
                    let updated = format!("{}{replacement}{}", target.prefix, target.suffix);
                    if updated.len() > FIELD_LIMIT {
                        return Err("The replacement exceeds the field limit.".into());
                    }
                    // A timed-out or queued request must never write later.
                    active(envelope)?;
                    verify(target)?;
                    active(envelope)?;
                    // Dispatch cannot be rolled back if a provider fails or hangs.
                    // Consume first, so an ambiguous result cannot reuse this token.
                    targets.remove(&destination.token);
                    value
                        .SetValue(&BSTR::from(updated.as_str()))
                        .map_err(|e| format!("Replacement could not be confirmed: {e}. Check the original field before retrying with a new capture."))?;
                    if bounded(value.CurrentValue().map_err(|e| format!("Replacement was dispatched but its field value could not be confirmed: {e}. Check the original field."))?)? != updated {
                        return Err("The destination did not retain the expected replacement. Check the original field before retrying with a new capture.".into());
                    }
                }
            }
            Ok(Response::Empty)
        }
        Request::CaptureLearning => {
            if targets.len() >= 64 {
                return Err("Too many pending text targets.".into());
            }
            let (app_id, _, pid) = identity()?;
            let target = capture_target(automation, app_id, pid, true)?;
            let token = store::new_id();
            let observation = LearningObservation {
                token: token.clone(),
                before_text: target.before.clone(),
                selection_start: target.prefix.len(),
                selection_end: target.prefix.len() + target.selected.len(),
                captured_at: now,
            };
            targets.insert(token, target);
            Ok(Response::Learning(observation))
        }
        Request::ReadLearning(observation) => {
            let target = targets
                .get(&observation.token)
                .filter(|t| t.learning)
                .ok_or("Correction observation has expired.")?;
            let (app_id, _, pid) = identity()?;
            if app_id != target.app_id || pid != target.pid {
                return Err("The destination application changed; observation stopped.".into());
            }
            let focused = unsafe { automation.GetFocusedElement() }.map_err(|e| e.to_string())?;
            if !unsafe { automation.CompareElements(&focused, &target.element) }
                .map_err(|e| e.to_string())?
                .as_bool()
            {
                return Err("The destination field changed; observation stopped.".into());
            }
            guard(&focused)?;
            let pattern = text_pattern(&focused)?;
            let text = unsafe {
                bounded(
                    pattern
                        .DocumentRange()
                        .map_err(|e| e.to_string())?
                        .GetText(FIELD_LIMIT as i32 + 1)
                        .map_err(|e| e.to_string())?,
                )
            }?;
            Ok(Response::Text(text))
        }
        Request::DiscardLearning(token) => {
            targets.remove(token);
            Ok(Response::Empty)
        }
    }
}
fn prune(targets: &mut HashMap<String, Target>) {
    targets.retain(|_, t| {
        t.captured.elapsed().as_millis()
            < if t.learning {
                LEARNING_TTL as u128
            } else {
                SELECTION_TTL as u128
            }
    });
}
pub fn capture(include_text: bool) -> Result<Destination, String> {
    match request(Request::Capture(include_text))? {
        Response::Destination(d) => Ok(d),
        _ => Err("Invalid accessibility response.".into()),
    }
}
pub fn check(destination: &Destination) -> Result<(), String> {
    request(Request::Check(destination.clone())).map(|_| ())
}
pub fn replace(destination: &Destination, text: &str) -> Result<(), String> {
    request(Request::Replace(destination.clone(), text.to_owned())).map(|_| ())
}
pub fn capture_learning() -> Result<LearningObservation, String> {
    match request(Request::CaptureLearning)? {
        Response::Learning(o) => Ok(o),
        _ => Err("Invalid accessibility response.".into()),
    }
}
pub fn read_learning(observation: &LearningObservation) -> Result<String, String> {
    match request(Request::ReadLearning(observation.clone()))? {
        Response::Text(t) => Ok(t),
        _ => Err("Invalid accessibility response.".into()),
    }
}
pub fn discard_learning(observation: &LearningObservation) {
    let _ = request(Request::DiscardLearning(observation.token.clone()));
}
