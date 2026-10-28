use super::store;
use crate::managers::history::HistoryManager;
use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct Snippet {
    pub id: String,
    pub trigger: String,
    pub expansion: String,
    pub enabled: bool,
    pub revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct LearnedHint {
    pub id: String,
    pub word: String,
    pub wrong: Option<String>,
    pub revision: i64,
    pub origin: String,
    pub reviewed: bool,
    pub undo_token: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct CorrectionProposal {
    pub id: String,
    pub before: String,
    pub after: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, specta::Type)]
pub struct LearningReceipt {
    pub hint: LearnedHint,
    pub undo_token: String,
}
#[derive(Clone, Debug)]
pub struct ProtectedSnippets {
    pub text: String,
    literals: Vec<(String, String)>,
}

fn init(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS workspace_snippets(id TEXT PRIMARY KEY, trigger TEXT NOT NULL, normalized TEXT NOT NULL UNIQUE, expansion TEXT NOT NULL, enabled INTEGER NOT NULL, revision INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_hints(id TEXT PRIMARY KEY, word TEXT NOT NULL, wrong TEXT, revision INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_corrections(id TEXT PRIMARY KEY, before_text TEXT NOT NULL, after_text TEXT NOT NULL, confirmed INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS workspace_correction_edits(proposal_id TEXT PRIMARY KEY REFERENCES workspace_corrections(id), history_id INTEGER NOT NULL, original_text TEXT NOT NULL, edited_text TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_learning_undo(token TEXT PRIMARY KEY, hint_id TEXT NOT NULL, revision INTEGER NOT NULL);") .map_err(|e| e.to_string())
    ?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS workspace_hint_origins(hint_id TEXT PRIMARY KEY REFERENCES workspace_hints(id) ON DELETE CASCADE, origin TEXT NOT NULL, reviewed INTEGER NOT NULL, created_at INTEGER NOT NULL)").map_err(|e|e.to_string())
}
fn db(app: &AppHandle) -> Result<Connection, String> {
    let conn = store::connection(app)?;
    init(&conn)?;
    Ok(conn)
}
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn validate(s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() || s.chars().count() > max || s.contains('\0') {
        Err("Invalid or oversized text".into())
    } else {
        Ok(())
    }
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
fn protect_with_mode(
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
#[tauri::command]
#[specta::specta]
pub fn list_learned_hints(app: AppHandle) -> Result<Vec<LearnedHint>, String> {
    hints(&app)
}
fn hints(app: &AppHandle) -> Result<Vec<LearnedHint>, String> {
    let conn = db(app)?;
    let mut stmt = conn
        .prepare("SELECT h.id,h.word,h.wrong,h.revision,COALESCE(o.origin,'manual'),COALESCE(o.reviewed,1),u.token FROM workspace_hints h LEFT JOIN workspace_hint_origins o ON o.hint_id=h.id LEFT JOIN workspace_learning_undo u ON u.hint_id=h.id AND u.revision=h.revision ORDER BY h.word,h.id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LearnedHint {
                id: r.get(0)?,
                word: r.get(1)?,
                wrong: r.get(2)?,
                revision: r.get(3)?,
                origin: r.get(4)?,
                reviewed: r.get(5)?,
                undo_token: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}
pub fn learned_vocabulary(app: &AppHandle) -> Result<Vec<String>, String> {
    Ok(hints(app)?
        .into_iter()
        .filter(|h| h.wrong.is_none())
        .map(|h| h.word)
        .collect())
}
pub fn remember_background_word(app: &AppHandle, word: &str) -> Result<bool, String> {
    let mut conn = db(app)?;
    remember_background_word_in(&mut conn, word)
}
fn remember_background_word_in(conn: &mut Connection, word: &str) -> Result<bool, String> {
    validate(word, 60)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let duplicate = {
        let mut stmt = tx
            .prepare("SELECT word FROM workspace_hints WHERE wrong IS NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let words = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        words.iter().any(|w| normalize(w) == normalize(word))
    };
    if duplicate {
        return Ok(false);
    }
    let id = store::new_id();
    let token = store::new_id();
    tx.execute(
        "INSERT INTO workspace_hints VALUES(?1,?2,NULL,1)",
        params![id, word],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO workspace_learning_undo VALUES(?1,?2,1)",
        params![token, id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO workspace_hint_origins VALUES(?1,'background',0,?2)",
        params![id, store::now_ms()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}
#[tauri::command]
#[specta::specta]
pub fn review_background_learning(
    app: AppHandle,
    id: String,
    expected_revision: i64,
) -> Result<(), String> {
    let n=db(&app)?.execute("UPDATE workspace_hint_origins SET reviewed=1 WHERE hint_id IN (SELECT id FROM workspace_hints WHERE id=?1 AND revision=?2)",params![id,expected_revision]).map_err(|e|e.to_string())?;
    if n == 1 {
        Ok(())
    } else {
        Err("Learning changed; reload before reviewing".into())
    }
}
pub fn apply_replacements(app: &AppHandle, text: &str) -> Result<String, String> {
    let rules = hints(app)?
        .into_iter()
        .filter_map(|h| {
            h.wrong.map(|wrong| Snippet {
                id: h.id,
                trigger: wrong,
                expansion: h.word,
                enabled: true,
                revision: h.revision,
            })
        })
        .collect::<Vec<_>>();
    Ok(fallback_expansion(&protect_with_mode(
        text,
        &rules,
        &store::new_id(),
        false,
    )?))
}

#[tauri::command]
#[specta::specta]
pub async fn propose_correction(
    app: AppHandle,
    history_id: i64,
    edited_text: String,
) -> Result<CorrectionProposal, String> {
    validate(&edited_text, 20000)?;
    let entry = app
        .state::<Arc<HistoryManager>>()
        .get_entry_by_id(history_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("History entry no longer exists")?;
    let before = entry
        .post_processed_text
        .unwrap_or(entry.transcription_text);
    if before == edited_text {
        return Err("Make an edit before reviewing a correction".into());
    }
    let a = before.split_whitespace().collect::<Vec<_>>();
    let b = edited_text.split_whitespace().collect::<Vec<_>>();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let old = a[prefix..a.len() - suffix].join(" ");
    let new = b[prefix..b.len() - suffix].join(" ");
    validate(&old, 120)?;
    validate(&new, 120)?;
    if old.split_whitespace().count() > 4 || new.split_whitespace().count() > 4 {
        return Err("Review one correction of up to four words at a time".into());
    }
    if !new.chars().any(char::is_alphabetic) {
        return Err("Vocabulary must contain a word".into());
    }
    let id = store::new_id();
    let mut conn = db(&app)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO workspace_corrections(id,before_text,after_text) VALUES(?1,?2,?3)",
        params![id, old, new],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO workspace_correction_edits VALUES(?1,?2,?3,?4)",
        params![id, history_id, before, edited_text],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(CorrectionProposal {
        id,
        before: old,
        after: new,
    })
}
#[tauri::command]
#[specta::specta]
pub fn confirm_correction(
    app: AppHandle,
    proposal_id: String,
    replacement: bool,
) -> Result<LearningReceipt, String> {
    let mut conn = db(&app)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let (before, after): (String, String) = tx
        .query_row(
            "SELECT before_text,after_text FROM workspace_corrections WHERE id=?1 AND confirmed=0",
            [&proposal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "Correction is unavailable or already confirmed".to_string())?;
    let hint = LearnedHint {
        id: store::new_id(),
        word: after,
        wrong: replacement.then_some(before),
        revision: 1,
        origin: "manual".into(),
        reviewed: true,
        undo_token: None,
    };
    let token = store::new_id();
    tx.execute(
        "INSERT INTO workspace_hints VALUES(?1,?2,?3,1)",
        params![hint.id, hint.word, hint.wrong],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO workspace_learning_undo VALUES(?1,?2,1)",
        params![token, hint.id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE workspace_corrections SET confirmed=1 WHERE id=?1",
        [proposal_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(LearningReceipt {
        hint,
        undo_token: token,
    })
}
#[tauri::command]
#[specta::specta]
pub fn update_learned_hint(
    app: AppHandle,
    id: String,
    word: String,
    wrong: Option<String>,
    expected_revision: i64,
) -> Result<(), String> {
    validate(&word, 120)?;
    if let Some(w) = &wrong {
        validate(w, 120)?
    }
    let n=db(&app)?.execute("UPDATE workspace_hints SET word=?1,wrong=?2,revision=revision+1 WHERE id=?3 AND revision=?4",params![word,wrong,id,expected_revision]).map_err(|e|e.to_string())?;
    if n == 1 {
        Ok(())
    } else {
        Err("Learning changed; reload before saving".into())
    }
}
#[tauri::command]
#[specta::specta]
pub fn delete_learned_hint(
    app: AppHandle,
    id: String,
    expected_revision: i64,
) -> Result<(), String> {
    let n = db(&app)?
        .execute(
            "DELETE FROM workspace_hints WHERE id=?1 AND revision=?2",
            params![id, expected_revision],
        )
        .map_err(|e| e.to_string())?;
    if n == 1 {
        Ok(())
    } else {
        Err("Learning changed; reload before deleting".into())
    }
}
#[tauri::command]
#[specta::specta]
pub fn undo_correction(app: AppHandle, undo_token: String) -> Result<(), String> {
    let mut conn = db(&app)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let n=tx.execute("DELETE FROM workspace_hints WHERE (id,revision) IN (SELECT hint_id,revision FROM workspace_learning_undo WHERE token=?1)",[&undo_token]).map_err(|e|e.to_string())?;
    if n != 1 {
        return Err("Learning changed; undo cannot erase newer edits".into());
    }
    tx.execute(
        "DELETE FROM workspace_learning_undo WHERE token=?1",
        [undo_token],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
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
    fn guard_undo_after_edit() {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c.execute_batch("INSERT INTO workspace_hints VALUES('h','word',NULL,2); INSERT INTO workspace_learning_undo VALUES('u','h',1);").unwrap();
        assert_eq!(c.execute("DELETE FROM workspace_hints WHERE (id,revision) IN (SELECT hint_id,revision FROM workspace_learning_undo WHERE token='u')",[]).unwrap(),0);
    }
    #[test]
    fn background_vocabulary_records_origin_review_and_undo_without_rules() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        init(&c).unwrap();
        assert!(remember_background_word_in(&mut c, "Maria").unwrap());
        assert!(!remember_background_word_in(&mut c, "MARIA").unwrap());
        let (origin,reviewed,wrong,token):(String,bool,Option<String>,String)=c.query_row("SELECT o.origin,o.reviewed,h.wrong,u.token FROM workspace_hints h JOIN workspace_hint_origins o ON o.hint_id=h.id JOIN workspace_learning_undo u ON u.hint_id=h.id",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(origin, "background");
        assert!(!reviewed);
        assert!(wrong.is_none());
        assert_eq!(c.execute("DELETE FROM workspace_hints WHERE (id,revision) IN (SELECT hint_id,revision FROM workspace_learning_undo WHERE token=?1)",[token]).unwrap(),1);
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM workspace_hint_origins", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
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
