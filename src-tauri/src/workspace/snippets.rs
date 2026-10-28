use super::personalization::{db, normalize, validate};
use super::store;
use regex::Regex;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct Snippet {
    pub id: String,
    pub trigger: String,
    pub expansion: String,
    pub enabled: bool,
    pub revision: i64,
}
#[derive(Clone, Debug)]
pub struct ProtectedSnippets {
    pub text: String,
    literals: Vec<(String, String)>,
}

#[tauri::command]
#[specta::specta]
pub fn list_snippets(app: AppHandle) -> Result<Vec<Snippet>, String> {
    load_snippets(&app)
}
pub fn load_snippets(app: &AppHandle) -> Result<Vec<Snippet>, String> {
    let conn = db(app)?;
    let mut stmt=conn.prepare("SELECT id,trigger,expansion,enabled,revision FROM workspace_snippets ORDER BY trigger,id").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Snippet {
                id: r.get(0)?,
                trigger: r.get(1)?,
                expansion: r.get(2)?,
                enabled: r.get(3)?,
                revision: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}
#[tauri::command]
#[specta::specta]
pub fn save_snippet(
    app: AppHandle,
    id: Option<String>,
    trigger: String,
    expansion: String,
    enabled: bool,
    expected_revision: Option<i64>,
) -> Result<Snippet, String> {
    validate(&trigger, 120)?;
    validate(&expansion, 20000)?;
    let conn = db(&app)?;
    let trigger = trigger.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = normalize(&trigger);
    let (id, revision) = match id {
        Some(id) => {
            let rev = expected_revision.ok_or("Missing revision")?;
            let n=conn.execute("UPDATE workspace_snippets SET trigger=?1,normalized=?2,expansion=?3,enabled=?4,revision=revision+1 WHERE id=?5 AND revision=?6",params![trigger,normalized,expansion,enabled,id,rev]).map_err(|e|e.to_string())?;
            if n != 1 {
                return Err("Snippet changed; reload before saving".into());
            }
            (id, rev + 1)
        }
        None => {
            let id = store::new_id();
            conn.execute(
                "INSERT INTO workspace_snippets VALUES(?1,?2,?3,?4,?5,1)",
                params![id, trigger, normalized, expansion, enabled],
            )
            .map_err(|e| e.to_string())?;
            (id, 1)
        }
    };
    Ok(Snippet {
        id,
        trigger,
        expansion,
        enabled,
        revision,
    })
}
#[tauri::command]
#[specta::specta]
pub fn delete_snippet(app: AppHandle, id: String, expected_revision: i64) -> Result<(), String> {
    let n = db(&app)?
        .execute(
            "DELETE FROM workspace_snippets WHERE id=?1 AND revision=?2",
            params![id, expected_revision],
        )
        .map_err(|e| e.to_string())?;
    if n == 1 {
        Ok(())
    } else {
        Err("Snippet changed; reload before deleting".into())
    }
}

fn word_char(c: char) -> bool {
    static MARK: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
        Regex::new(r"^\p{M}$").expect("constant Unicode mark pattern")
    });
    c.is_alphanumeric() || c == '_' || MARK.is_match(&c.to_string())
}
fn protect(text: &str, snippets: &[Snippet], nonce: &str) -> Result<ProtectedSnippets, String> {
    protect_with_mode(text, snippets, nonce, true)
}
pub(super) fn protect_with_mode(
    text: &str,
    snippets: &[Snippet],
    nonce: &str,
    whole_trigger: bool,
) -> Result<ProtectedSnippets, String> {
    let mut snippets = snippets.iter().filter(|s| s.enabled).collect::<Vec<_>>();
    snippets.sort_by(|a, b| {
        b.trigger
            .chars()
            .count()
            .cmp(&a.trigger.chars().count())
            .then(a.id.cmp(&b.id))
    });
    let patterns = snippets
        .iter()
        .map(|s| {
            let pattern = s
                .trigger
                .split_whitespace()
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(r"\s+");
            Regex::new(&format!("(?i)^(?:{pattern})"))
                .map(|re| (*s, re))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = String::new();
    let mut literals = Vec::new();
    let mut cursor = 0;
    // Dictation punctuation around an otherwise complete trigger is not part of its literal expansion.
    for s in snippets.iter().filter(|_| whole_trigger) {
        let pattern = s
            .trigger
            .split_whitespace()
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(r"\s+");
        let whole = Regex::new(&format!(r"(?i)^\s*\p{{P}}*(?:{pattern})\p{{P}}*\s*$"))
            .map_err(|e| e.to_string())?;
        if whole.is_match(text) {
            let marker = format!("⟦HANDY_{nonce}_0⟧");
            return Ok(ProtectedSnippets {
                text: marker.clone(),
                literals: vec![(marker, s.expansion.clone())],
            });
        }
    }
    while cursor < text.len() {
        let remaining = &text[cursor..];
        let mut found = None;
        for (s, re) in &patterns {
            if let Some(m) = re.find(remaining) {
                let end = cursor + m.end();
                let before = text[..cursor].chars().next_back();
                let after = text[end..].chars().next();
                if !before.is_some_and(word_char) && !after.is_some_and(word_char) {
                    found = Some((end, *s));
                    break;
                }
            }
        }
        if let Some((end, s)) = found {
            let marker = format!("⟦HANDY_{nonce}_{}⟧", literals.len());
            out.push_str(&marker);
            literals.push((marker, s.expansion.clone()));
            cursor = end;
        } else {
            let c = remaining.chars().next().ok_or("Invalid text boundary")?;
            out.push(c);
            cursor += c.len_utf8();
        }
    }
    Ok(ProtectedSnippets {
        text: out,
        literals,
    })
}
pub fn protect_snippets(app: &AppHandle, text: &str) -> Result<ProtectedSnippets, String> {
    protect(text, &load_snippets(app)?, &store::new_id())
}
pub fn restore_snippets(protected: &ProtectedSnippets, output: &str) -> Result<String, String> {
    let mut spans = Vec::new();
    for (marker, literal) in &protected.literals {
        let matches = output.match_indices(marker).collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err("Snippet marker changed during processing".into());
        }
        spans.push((matches[0].0, marker.len(), literal));
    }
    spans.sort_by_key(|span| span.0);
    let mut restored = String::new();
    let mut cursor = 0;
    for (start, len, literal) in spans {
        restored.push_str(&output[cursor..start]);
        restored.push_str(literal);
        cursor = start + len;
    }
    restored.push_str(&output[cursor..]);
    Ok(restored)
}
pub fn fallback_expansion(protected: &ProtectedSnippets) -> String {
    restore_snippets(protected, &protected.text).unwrap_or_else(|_| protected.text.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: trigger.into(),
            trigger: trigger.into(),
            expansion: expansion.into(),
            enabled: true,
            revision: 1,
        }
    }
    #[test]
    fn literal_boundaries_longest_unicode() {
        let p = protect(
            "Hi MY LINK please, my linked and café.",
            &[
                s("my", "short"),
                s("my link", "https://a.test/$1"),
                s("café", "Crème"),
            ],
            "test",
        )
        .unwrap();
        assert_eq!(
            fallback_expansion(&p),
            "Hi https://a.test/$1 please, short linked and Crème."
        );
    }
    #[test]
    fn whole_trigger_and_no_recursion() {
        assert_eq!(
            fallback_expansion(
                &protect("  Address! ", &[s("address", "address next")], "t").unwrap()
            ),
            "address next"
        );
        assert_eq!(
            fallback_expansion(
                &protect(
                    "copy café",
                    &[s("café", "address"), s("address", "recursive")],
                    "t"
                )
                .unwrap()
            ),
            "copy address"
        );
    }
    #[test]
    fn missing_duplicate_markers_fail() {
        let p = protect("address", &[s("address", "literal")], "t").unwrap();
        assert!(restore_snippets(&p, "gone").is_err());
        assert!(restore_snippets(&p, &format!("{} {}", p.text, p.text)).is_err());
        assert_eq!(restore_snippets(&p, &p.text).unwrap(), "literal");
    }
    #[test]
    fn literal_marker_text_is_not_recursively_restored() {
        let p = protect(
            "first then second",
            &[s("first", "⟦HANDY_t_1⟧"), s("second", "literal")],
            "t",
        )
        .unwrap();
        assert_eq!(fallback_expansion(&p), "⟦HANDY_t_1⟧ then literal");
        assert_eq!(
            fallback_expansion(&protect("C++.", &[s("C++", "language")], "t").unwrap()),
            "language"
        );
    }
    #[test]
    fn corrections_preserve_whole_utterance_punctuation() {
        for (input, expected) in [("teh.", "the."), ("(teh)", "(the)"), ("  TEH! ", "  the! ")] {
            assert_eq!(
                fallback_expansion(
                    &protect_with_mode(input, &[s("teh", "the")], "t", false).unwrap()
                ),
                expected
            );
        }
    }
}
