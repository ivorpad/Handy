//! Corrections learned from edits to history entries.
use super::personalization::{db, validate, LearnedHint};
use super::snippets::word_char;
use super::store;
use crate::managers::history::HistoryManager;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

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

/// The words that changed between a transcript and the user's edit. Leading
/// and trailing punctuation both sides share is left out, so editing
/// "Why Trimarchi?" to "Why Omarchy?" proposes `Trimarchi` → `Omarchy`.
fn correction_span(before: &str, edited: &str) -> Result<(String, String), String> {
    let a = before.split_whitespace().collect::<Vec<_>>();
    let b = edited.split_whitespace().collect::<Vec<_>>();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let old = a[prefix..a.len() - suffix].join(" ");
    let new = b[prefix..b.len() - suffix].join(" ");
    let (old, new) = trim_shared_punctuation(&old, &new);
    validate(old, 120)?;
    validate(new, 120)?;
    if old.split_whitespace().count() > 4 || new.split_whitespace().count() > 4 {
        return Err("Review one correction of up to four words at a time".into());
    }
    if !new.chars().any(char::is_alphabetic) {
        return Err("Vocabulary must contain a word".into());
    }
    Ok((old.to_owned(), new.to_owned()))
}
/// Drops non-word characters that start or end both strings, using the same
/// word characters as replacement matching.
fn trim_shared_punctuation<'a>(mut old: &'a str, mut new: &'a str) -> (&'a str, &'a str) {
    while let (Some(x), Some(y)) = (old.chars().next(), new.chars().next()) {
        if x != y || word_char(x) {
            break;
        }
        old = old[x.len_utf8()..].trim_start();
        new = new[y.len_utf8()..].trim_start();
    }
    while let (Some(x), Some(y)) = (old.chars().next_back(), new.chars().next_back()) {
        if x != y || word_char(x) {
            break;
        }
        old = old[..old.len() - x.len_utf8()].trim_end();
        new = new[..new.len() - y.len_utf8()].trim_end();
    }
    (old, new)
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
    let (old, new) = correction_span(&before, &edited_text)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn span(before: &str, edited: &str) -> (String, String) {
        correction_span(before, edited).unwrap()
    }
    fn pair(old: &str, new: &str) -> (String, String) {
        (old.to_owned(), new.to_owned())
    }

    #[test]
    fn shared_punctuation_stays_out_of_the_rule() {
        assert_eq!(
            span("Why Trimarchi?", "Why Omarchy?"),
            pair("Trimarchi", "Omarchy")
        );
        assert_eq!(
            span("¿Trimarchi?", "¿Omarchy?"),
            pair("Trimarchi", "Omarchy")
        );
        assert_eq!(
            span(
                "Install “Trimarchi”, then reboot.",
                "Install “Omarchy”, then reboot."
            ),
            pair("Trimarchi", "Omarchy")
        );
        // Punctuation inside a multi-word change is kept.
        assert_eq!(
            span("Open cloud, app now", "Open Claude, Code now"),
            pair("cloud, app", "Claude, Code")
        );
    }

    #[test]
    fn punctuation_on_one_side_is_part_of_the_change() {
        assert_eq!(
            span("Use Trimarchi.", "Use Omarchy"),
            pair("Trimarchi.", "Omarchy")
        );
        // Typing a word before existing punctuation adds text but replaces
        // nothing, so there is no rule to propose.
        assert!(correction_span("Why ?", "Why Omarchy?").is_err());
    }
}
