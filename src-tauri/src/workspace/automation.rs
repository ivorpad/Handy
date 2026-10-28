use super::store;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Type, PartialEq)]
pub struct AutomationSettings {
    pub background_learning: bool,
    pub automatic_commands: bool,
}

#[derive(Default, Deserialize, Serialize, Type)]
#[serde(deny_unknown_fields)]
pub struct AutomationChanges {
    pub background_learning: Option<bool>,
    pub automatic_commands: Option<bool>,
}

fn initialize(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_automation (
        id INTEGER PRIMARY KEY CHECK(id=1),
        background_learning INTEGER NOT NULL DEFAULT 0 CHECK(background_learning IN (0,1)),
        automatic_commands INTEGER NOT NULL DEFAULT 0 CHECK(automatic_commands IN (0,1))
    ); INSERT OR IGNORE INTO workspace_automation(id) VALUES(1);",
    )
    .map_err(|e| e.to_string())
}

fn read(db: &Connection) -> Result<AutomationSettings, String> {
    db.query_row(
        "SELECT background_learning,automatic_commands FROM workspace_automation WHERE id=1",
        [],
        |row| {
            Ok(AutomationSettings {
                background_learning: row.get(0)?,
                automatic_commands: row.get(1)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

pub fn settings(app: &AppHandle) -> Result<AutomationSettings, String> {
    let db = store::connection(app)?;
    initialize(&db)?;
    read(&db)
}

fn update(db: &mut Connection, changes: AutomationChanges) -> Result<AutomationSettings, String> {
    initialize(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute("UPDATE workspace_automation SET background_learning=COALESCE(?1,background_learning),automatic_commands=COALESCE(?2,automatic_commands) WHERE id=1",
        params![changes.background_learning, changes.automatic_commands]).map_err(|e| e.to_string())?;
    let result = read(&tx)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn workspace_get_automation(app: AppHandle) -> Result<AutomationSettings, String> {
    settings(&app)
}

#[tauri::command]
#[specta::specta]
pub fn workspace_save_automation(
    app: AppHandle,
    changes: AutomationChanges,
) -> Result<AutomationSettings, String> {
    let result = update(&mut store::connection(&app)?, changes)?;
    let _ = app.emit("workspace-automation-changed", result);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_independent_updates_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workspace.db");
        let mut first = store::open(&path).unwrap();
        initialize(&first).unwrap();
        assert_eq!(read(&first).unwrap(), AutomationSettings::default());
        update(
            &mut first,
            AutomationChanges {
                background_learning: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let mut second = store::open(&path).unwrap();
        let changed = update(
            &mut second,
            AutomationChanges {
                automatic_commands: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(changed.background_learning && changed.automatic_commands);
        drop(first);
        drop(second);
        let mut reopened = store::open(&path).unwrap();
        let disabled = update(
            &mut reopened,
            AutomationChanges {
                background_learning: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!disabled.background_learning);
        assert!(disabled.automatic_commands);
    }
}
