//! Destination identity is independent of permission to read a text field.
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct Destination {
    pub app_id: String,
    pub app_name: String,
    pub domain: Option<String>,
    pub selected_text: Option<String>,
    pub token: String,
    pub captured_at: i64,
    pub context_available: bool,
    pub unavailable_reason: Option<String>,
}

/// A short-lived local observation of the field receiving this dictation.
/// Deliberately absent from command payloads and persistence.
#[derive(Clone, Debug)]
pub struct LearningObservation {
    pub token: String,
    pub before_text: String,
    /// UTF-8 byte offsets, always character boundaries in before_text.
    pub selection_start: usize,
    pub selection_end: usize,
    pub captured_at: i64,
}
pub(crate) const MAX_LEARNING_TEXT_BYTES: usize = 65_536;
pub(crate) const LEARNING_LIFETIME_MS: i64 = 90_000;

#[cfg(target_os = "macos")]
#[path = "native_context.rs"]
mod native;
#[cfg(target_os = "windows")]
#[path = "context_windows.rs"]
mod native;
#[cfg(target_os = "linux")]
#[path = "context_linux.rs"]
mod native;

pub fn capture(include_text: bool) -> Result<Destination, String> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    return native::capture(include_text);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = include_text;
        Err("Destination capture is unavailable on this operating system.".into())
    }
}

pub fn check(destination: &Destination) -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    return native::check(destination);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = destination;
        Err("Selected-text access is unavailable on this operating system.".into())
    }
}

pub fn replace(destination: &Destination, text: &str) -> Result<(), String> {
    if text.len() > 500_000 {
        return Err("Replacement exceeds the text limit.".into());
    }
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    return native::replace(destination, text);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (destination, text);
        Err("Selected-text replacement is unavailable on this operating system.".into())
    }
}

pub fn capture_learning() -> Result<LearningObservation, String> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let observation = native::capture_learning()?;
        if let Err(error) = validate_learning_observation(&observation) {
            native::discard_learning(&observation);
            return Err(error);
        }
        Ok(observation)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    Err("Correction observation is unavailable on this operating system.".into())
}

pub fn read_learning(observation: &LearningObservation) -> Result<String, String> {
    if crate::workspace::store::now_ms() - observation.captured_at >= LEARNING_LIFETIME_MS {
        discard_learning(observation);
        return Err("The correction observation has expired.".into());
    }
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let value = native::read_learning(observation)?;
        if value.len() > MAX_LEARNING_TEXT_BYTES {
            discard_learning(observation);
            return Err("The field exceeds the local correction observation limit.".into());
        }
        Ok(value)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    Err("Correction observation is unavailable on this operating system.".into())
}

fn validate_learning_observation(observation: &LearningObservation) -> Result<(), String> {
    if observation.before_text.len() > MAX_LEARNING_TEXT_BYTES
        || observation.selection_start > observation.selection_end
        || !observation
            .before_text
            .is_char_boundary(observation.selection_start)
        || !observation
            .before_text
            .is_char_boundary(observation.selection_end)
    {
        return Err("The destination does not expose a bounded, valid text selection.".into());
    }
    Ok(())
}

pub fn discard_learning(observation: &LearningObservation) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    native::discard_learning(observation);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let _ = observation;
}

/// Native APIs count UTF-16 units; never slice a UTF-8 string at that number.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) fn utf16_offset_to_byte(text: &str, offset: usize) -> Result<usize, String> {
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units == offset {
            return Ok(index);
        }
        units += ch.len_utf16();
        if units > offset {
            return Err("The selection divides a Unicode character.".into());
        }
    }
    if units == offset {
        Ok(text.len())
    } else {
        Err("The selection lies outside the field.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_offsets_preserve_unicode_boundaries() {
        let value = "a🦀é";
        assert_eq!(utf16_offset_to_byte(value, 0).unwrap(), 0);
        assert_eq!(utf16_offset_to_byte(value, 1).unwrap(), 1);
        assert!(utf16_offset_to_byte(value, 2).is_err());
        assert_eq!(utf16_offset_to_byte(value, 3).unwrap(), 5);
        assert_eq!(utf16_offset_to_byte(value, 4).unwrap(), 7);
        assert!(utf16_offset_to_byte(value, 5).is_err());
    }
    #[test]
    fn observation_rejects_invalid_ranges_and_oversized_fields() {
        let mut observation = LearningObservation {
            token: "test".into(),
            before_text: "a🦀é".into(),
            selection_start: 1,
            selection_end: 5,
            captured_at: 0,
        };
        assert!(validate_learning_observation(&observation).is_ok());
        observation.selection_start = 2;
        assert!(validate_learning_observation(&observation).is_err());
        observation.selection_start = 6;
        observation.selection_end = 5;
        assert!(validate_learning_observation(&observation).is_err());
        observation.selection_start = 0;
        observation.selection_end = 8;
        assert!(validate_learning_observation(&observation).is_err());
        observation.before_text = "x".repeat(MAX_LEARNING_TEXT_BYTES + 1);
        observation.selection_end = 0;
        assert!(validate_learning_observation(&observation).is_err());
    }
}

#[tauri::command]
#[specta::specta]
pub fn capture_destination(include_text: bool) -> Result<Destination, String> {
    capture(include_text)
}
