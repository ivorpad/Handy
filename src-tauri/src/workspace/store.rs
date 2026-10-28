use rusqlite::Connection;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{nanos:x}-{:x}-{:x}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn connection(app: &AppHandle) -> Result<Connection, String> {
    let directory = crate::portable::app_data_dir(app).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    open(&directory.join("workspace.db"))
}

pub(crate) fn open(path: &Path) -> Result<Connection, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_connections_preserve_data_and_enforce_foreign_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("workspace.db");
        {
            let conn = open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE parents(id TEXT PRIMARY KEY);\
                 CREATE TABLE children(parent TEXT REFERENCES parents(id));\
                 INSERT INTO parents VALUES ('saved');",
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM parents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert!(conn
            .execute("INSERT INTO children VALUES ('missing')", [])
            .is_err());
    }
}
