//! Opt-in, local correction learning scoped to the most recent dictation field.
use super::{automation, context, personalization};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Stop observation when a new recording or cancellation supersedes this one.
pub fn cancel() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Called only after a successful paste, using the field captured just before it.
pub fn begin(app: &AppHandle, observation: context::LearningObservation, inserted_text: String) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = observe(&app, &observation, &inserted_text, generation).await;
        let _ = tauri::async_runtime::spawn_blocking(move || {
            context::discard_learning(&observation);
        })
        .await;
        if result.is_err() {
            log::debug!(
                "Local correction observation stopped because its field became unavailable"
            );
        }
    });
}

async fn observe(
    app: &AppHandle,
    observation: &context::LearningObservation,
    inserted: &str,
    generation: u64,
) -> Result<(), String> {
    let span = InsertionSpan::new(
        &observation.before_text,
        observation.selection_start,
        observation.selection_end,
        inserted,
    )?;
    let started = Instant::now();
    let mut verified = false;
    let mut previous: Option<String> = None;
    let mut stable_since = Instant::now();
    // Field reads stay local. Stop immediately when identity, permission, opt-in or insertion scope changes.
    while started.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(750)).await;
        if started.elapsed() >= Duration::from_secs(60)
            || GENERATION.load(Ordering::SeqCst) != generation
            || !automation::settings(app)?.background_learning
        {
            break;
        }
        let snapshot = observation.clone();
        let current =
            tauri::async_runtime::spawn_blocking(move || context::read_learning(&snapshot))
                .await
                .map_err(|_| "Local field observation worker stopped".to_string())??;
        // A slow accessibility call can finish after cancellation, opt-out or expiry.
        if started.elapsed() >= Duration::from_secs(60)
            || GENERATION.load(Ordering::SeqCst) != generation
            || !automation::settings(app)?.background_learning
        {
            break;
        }
        if !verified {
            if current == span.expected {
                verified = true;
                continue;
            }
            // Clipboard paste can be asynchronous. Never infer an insertion from a different value.
            if started.elapsed() > Duration::from_secs(3) {
                break;
            }
            continue;
        }
        let Some(edited) = span.inserted_region(&current) else {
            break;
        };
        if edited == inserted {
            previous = None;
            continue;
        }
        let Some(word) = correction_candidate(inserted, edited) else {
            break;
        };
        if previous.as_deref() != Some(word.as_str()) {
            previous = Some(word);
            stable_since = Instant::now();
            continue;
        }
        if stable_since.elapsed() < Duration::from_millis(2250) {
            continue;
        }
        if started.elapsed() >= Duration::from_secs(60)
            || GENERATION.load(Ordering::SeqCst) != generation
            || !automation::settings(app)?.background_learning
        {
            break;
        }
        if personalization::remember_background_word(app, &word)? {
            let _ = app.emit("workspace-learning-changed", ());
        }
        break;
    }
    Ok(())
}

struct InsertionSpan {
    prefix: String,
    suffix: String,
    expected: String,
}
impl InsertionSpan {
    fn new(before: &str, start: usize, end: usize, inserted: &str) -> Result<Self, String> {
        if start > end
            || end > before.len()
            || !before.is_char_boundary(start)
            || !before.is_char_boundary(end)
            || before.len() > 65536
            || inserted.is_empty()
            || inserted.len() > 20000
        {
            return Err("Invalid insertion observation".into());
        }
        let prefix = before[..start].to_owned();
        let suffix = before[end..].to_owned();
        let expected = format!("{prefix}{inserted}{suffix}");
        Ok(Self {
            prefix,
            suffix,
            expected,
        })
    }
    fn inserted_region<'a>(&self, current: &'a str) -> Option<&'a str> {
        if current.len() > 65536
            || !current.starts_with(&self.prefix)
            || !current.ends_with(&self.suffix)
            || current.len() < self.prefix.len() + self.suffix.len()
        {
            return None;
        }
        current.get(self.prefix.len()..current.len() - self.suffix.len())
    }
}

/// One small spelling edit only. Sentence rewrites, numbers, URLs and deleted/added words are ignored.
fn correction_candidate(before: &str, after: &str) -> Option<String> {
    let a = before.split_whitespace().collect::<Vec<_>>();
    let b = after.split_whitespace().collect::<Vec<_>>();
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let changed = a
        .iter()
        .zip(&b)
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .collect::<Vec<_>>();
    if changed.len() != 1 {
        return None;
    }
    let (index, (old, new)) = changed[0];
    let tokens = regex::Regex::new(r"\S+").ok()?;
    let old_span = tokens.find_iter(before).nth(index)?;
    if format!(
        "{}{}{}",
        &before[..old_span.start()],
        new,
        &before[old_span.end()..]
    ) != after
    {
        return None;
    }
    if old
        .chars()
        .chain(new.chars())
        .any(|c| c.is_numeric() || matches!(c, '/' | '@' | ':'))
    {
        return None;
    }
    let strip = |s: &str| s.trim_matches(|c: char| !c.is_alphabetic()).to_owned();
    let old_word = strip(old);
    let new_word = strip(new);
    if old.replace(&old_word, "") != new.replace(&new_word, "") {
        return None;
    }
    let valid = |s: &str| {
        s.chars().count() >= 2
            && s.chars().count() <= 40
            && s.chars()
                .all(|c| c.is_alphabetic() || c == '\'' || c == '-' || c == '’')
    };
    if !valid(&old_word) || !valid(&new_word) || old_word.to_lowercase() == new_word.to_lowercase()
    {
        return None;
    }
    let distance = strsim::damerau_levenshtein(&old_word.to_lowercase(), &new_word.to_lowercase());
    if distance > 2
        || strsim::normalized_damerau_levenshtein(
            &old_word.to_lowercase(),
            &new_word.to_lowercase(),
        ) < 0.5
    {
        return None;
    }
    Some(new_word)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spelling_correction_only() {
        assert_eq!(
            correction_candidate("teh cat", "the cat"),
            Some("the".into())
        );
        assert_eq!(
            correction_candidate("Hello Marla.", "Hello Maria."),
            Some("Maria".into())
        );
        assert_eq!(
            correction_candidate("cafe team", "café team"),
            Some("café".into())
        );
    }
    #[test]
    fn ignores_unrelated_or_sensitive_shaped_changes() {
        for (a, b) in [
            ("hello there", "goodbye there"),
            ("a link", "a https://site.test"),
            ("code 1234", "code 1235"),
            ("one word", "one new word"),
            ("teh cat", "the dog"),
            ("handy", "Handy"),
            ("name", "name!"),
            ("Marla123", "Maria123"),
            ("@Marla", "@Maria"),
            ("Hello Marla.", "Hello   Maria."),
        ] {
            assert_eq!(correction_candidate(a, b), None, "{a} -> {b}");
        }
    }
    #[test]
    fn insertion_scope_checks_unicode_and_surrounding_edits() {
        let span = InsertionSpan::new("pré OLD suffix", 5, 8, "Hello Marla.").unwrap();
        assert_eq!(span.expected, "pré Hello Marla. suffix");
        assert_eq!(
            span.inserted_region("pré Hello Maria. suffix"),
            Some("Hello Maria.")
        );
        assert_eq!(span.inserted_region("new Hello Maria. suffix"), None);
        assert!(InsertionSpan::new("é", 1, 2, "x").is_err());
    }
    #[test]
    fn observed_insert_must_not_include_outside_changes() {
        let span = InsertionSpan::new("before after", 7, 7, "teh ").unwrap();
        assert!(span.inserted_region("before the after!").is_none());
    }
}
