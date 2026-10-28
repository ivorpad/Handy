use super::snippets::{fallback_expansion, protect_with_mode, Snippet};
use super::store;
use crate::managers::history::HistoryManager;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

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

fn init(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS workspace_snippets(id TEXT PRIMARY KEY, trigger TEXT NOT NULL, normalized TEXT NOT NULL UNIQUE, expansion TEXT NOT NULL, enabled INTEGER NOT NULL, revision INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_hints(id TEXT PRIMARY KEY, word TEXT NOT NULL, wrong TEXT, revision INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_corrections(id TEXT PRIMARY KEY, before_text TEXT NOT NULL, after_text TEXT NOT NULL, confirmed INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS workspace_correction_edits(proposal_id TEXT PRIMARY KEY REFERENCES workspace_corrections(id), history_id INTEGER NOT NULL, original_text TEXT NOT NULL, edited_text TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_learning_undo(token TEXT PRIMARY KEY, hint_id TEXT NOT NULL, revision INTEGER NOT NULL);") .map_err(|e| e.to_string())
    ?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS workspace_hint_origins(hint_id TEXT PRIMARY KEY REFERENCES workspace_hints(id) ON DELETE CASCADE, origin TEXT NOT NULL, reviewed INTEGER NOT NULL, created_at INTEGER NOT NULL)").map_err(|e|e.to_string())
}
pub(super) fn db(app: &AppHandle) -> Result<Connection, String> {
    let conn = store::connection(app)?;
    init(&conn)?;
    Ok(conn)
}
pub(super) fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
pub(super) fn validate(s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() || s.chars().count() > max || s.contains('\0') {
        Err("Invalid or oversized text".into())
    } else {
        Ok(())
    }
}

#[tauri::command]
#[specta::specta]
pub fn list_learned_hints(app: AppHandle) -> Result<Vec<LearnedHint>, String> {
    hints(&app)
}
fn hints(app: &AppHandle) -> Result<Vec<LearnedHint>, String> {
    hints_in(&db(app)?)
}
fn hints_in(conn: &Connection) -> Result<Vec<LearnedHint>, String> {
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
/// Adds a dictionary word directly. The word becomes vocabulary, and each
/// `heard_as` spelling becomes an exact replacement for output too far from
/// the word for fuzzy correction, such as "Trimarchi" for "Omarchy".
#[tauri::command]
#[specta::specta]
pub fn add_dictionary_word(
    app: AppHandle,
    word: String,
    heard_as: Vec<String>,
) -> Result<(), String> {
    add_dictionary_word_in(&mut db(&app)?, &word, &heard_as)
}
fn add_dictionary_word_in(
    conn: &mut Connection,
    word: &str,
    heard_as: &[String],
) -> Result<(), String> {
    let word = word.trim();
    validate(word, 120)?;
    if !word.chars().any(char::is_alphabetic) {
        return Err("Vocabulary must contain a word".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut existing = {
        let mut stmt = tx
            .prepare("SELECT word,wrong FROM workspace_hints")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    let key = normalize(word);
    if !existing
        .iter()
        .any(|(w, wrong)| wrong.is_none() && normalize(w) == key)
    {
        tx.execute(
            "INSERT INTO workspace_hints VALUES(?1,?2,NULL,1)",
            params![store::new_id(), word],
        )
        .map_err(|e| e.to_string())?;
    }
    for alias in heard_as {
        let alias = alias.trim();
        validate(alias, 120)?;
        let alias_key = normalize(alias);
        // A different case of the word is already fixed by fuzzy matching.
        if alias_key == key {
            continue;
        }
        let rule = existing
            .iter()
            .find(|(_, wrong)| wrong.as_deref().is_some_and(|w| normalize(w) == alias_key));
        match rule {
            Some((target, _)) if normalize(target) == key => continue,
            Some((target, _)) => {
                return Err(format!("\"{alias}\" is already replaced with \"{target}\""))
            }
            None => {}
        }
        tx.execute(
            "INSERT INTO workspace_hints VALUES(?1,?2,?3,1)",
            params![store::new_id(), word, alias],
        )
        .map_err(|e| e.to_string())?;
        existing.push((word.to_owned(), Some(alias.to_owned())));
    }
    tx.commit().map_err(|e| e.to_string())
}
pub fn apply_replacements(app: &AppHandle, text: &str) -> Result<String, String> {
    replace_confirmed(text, hints(app)?)
}
fn replace_confirmed(text: &str, hints: Vec<LearnedHint>) -> Result<String, String> {
    let rules = hints
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
    fn dictionary_word_replaces_spellings_it_was_heard_as() {
        let mut c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        let heard =
            |spellings: &[&str]| spellings.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        add_dictionary_word_in(
            &mut c,
            " Omarchy ",
            &heard(&["Trimarchi", "tria  marchi", "OMARCHY"]),
        )
        .unwrap();
        // Adding the same entry again changes nothing.
        add_dictionary_word_in(&mut c, "omarchy", &heard(&["TRIMARCHI"])).unwrap();
        let hints = hints_in(&c).unwrap();
        let words = hints
            .iter()
            .filter(|h| h.wrong.is_none())
            .map(|h| h.word.as_str());
        assert_eq!(words.collect::<Vec<_>>(), ["Omarchy"]);
        assert_eq!(hints.len(), 3);
        assert!(hints.iter().all(|h| h.origin == "manual" && h.reviewed));
        // Misrecognitions from a Nemotron streaming dictation.
        for (heard, expected) in [
            ("Why Trimarchi?", "Why Omarchy?"),
            (
                "Tria Marchi is now downloading everything.",
                "Omarchy is now downloading everything.",
            ),
            ("Trimarchic stays.", "Trimarchic stays."),
        ] {
            assert_eq!(replace_confirmed(heard, hints.clone()).unwrap(), expected);
        }
        let conflict = add_dictionary_word_in(&mut c, "Monarchy", &heard(&["trimarchi"]));
        assert!(conflict.unwrap_err().contains("Omarchy"));
        assert!(add_dictionary_word_in(&mut c, "  ", &[]).is_err());
        assert!(add_dictionary_word_in(&mut c, "123", &[]).is_err());
        assert_eq!(hints_in(&c).unwrap().len(), 3);
    }
}
