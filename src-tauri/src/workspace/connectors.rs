use super::store;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;
use tauri::AppHandle;

const MAX_BYTES: usize = 1_048_576;
const LOCAL_ONLY_ERROR: &str =
    "Remote connections are disabled. This workspace imports local files only.";

#[derive(Clone, Serialize, Deserialize, Type)]
pub struct Connector {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub source: String,
    pub status: String,
    pub last_synced_at: Option<i64>,
    pub last_error: Option<String>,
    pub revision: i64,
    pub document_count: i64,
}

#[derive(Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ConnectorInput {
    pub name: String,
    pub kind: String,
    pub source: String,
}

pub(crate) fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS workspace_connectors(
        id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, source TEXT NOT NULL,
        status TEXT NOT NULL, last_synced_at INTEGER, last_error TEXT, revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS workspace_connector_documents(
        id TEXT PRIMARY KEY, connector_id TEXT NOT NULL REFERENCES workspace_connectors(id) ON DELETE CASCADE,
        title TEXT NOT NULL, content TEXT NOT NULL, updated_at INTEGER NOT NULL);")
        .map_err(|_| "Could not initialize connections.".to_string())?;
    // Disable legacy remote sources without deleting their locally cached data.
    // Bumping the revision also invalidates any older import reservation.
    conn.execute("UPDATE workspace_connectors SET status='disabled',last_error=?1,revision=revision+1 WHERE kind NOT IN ('local_file','local_folder','calendar') AND status != 'disabled'", [LOCAL_ONLY_ERROR]).map_err(|_| "Could not disable remote connections.".to_string())?;
    Ok(())
}

fn list(conn: &Connection) -> Result<Vec<Connector>, String> {
    init_schema(conn)?;
    let mut statement = conn.prepare("SELECT c.id,c.kind,c.name,c.source,c.status,c.last_synced_at,c.last_error,c.revision,(SELECT count(*) FROM workspace_connector_documents d WHERE d.connector_id=c.id) FROM workspace_connectors c ORDER BY c.name,c.id").map_err(|_| "Could not read connections.")?;
    let rows = statement
        .query_map([], |r| {
            let id: String = r.get(0)?;
            Ok(Connector {
                id,
                kind: r.get(1)?,
                name: r.get(2)?,
                source: r.get(3)?,
                status: r.get(4)?,
                last_synced_at: r.get(5)?,
                last_error: r.get(6)?,
                revision: r.get(7)?,
                document_count: r.get(8)?,
            })
        })
        .map_err(|_| "Could not read connections.")?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read connections.".into())
}

#[tauri::command]
#[specta::specta]
pub fn workspace_list_connectors(app: AppHandle) -> Result<Vec<Connector>, String> {
    list(&store::connection(&app)?)
}

fn validate(input: &ConnectorInput) -> Result<(), String> {
    if input.name.trim().is_empty()
        || input.name.len() > 200
        || input.source.is_empty()
        || input.source.len() > 4096
    {
        return Err("Provide a name and a valid source.".into());
    }
    match input.kind.as_str() {
        "local_file" | "local_folder" | "calendar" => {
            if !Path::new(&input.source).is_absolute() {
                return Err("Choose an absolute local path.".into());
            }
        }
        _ => return Err(LOCAL_ONLY_ERROR.into()),
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn workspace_save_connector(
    app: AppHandle,
    input: ConnectorInput,
) -> Result<Connector, String> {
    validate(&input)?;
    let conn = store::connection(&app)?;
    init_schema(&conn)?;
    let id = store::new_id();
    conn.execute("INSERT INTO workspace_connectors(id,kind,name,source,status,revision) VALUES(?1,?2,?3,?4,'idle',1)",params![id,input.kind,input.name.trim(),input.source]).map_err(|_|"Could not save connection.")?;
    list(&conn)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or("Connection was removed.".into())
}

#[tauri::command]
#[specta::specta]
pub fn workspace_disconnect_connector(app: AppHandle, id: String) -> Result<(), String> {
    let conn = store::connection(&app)?;
    init_schema(&conn)?;
    conn.execute("DELETE FROM workspace_connectors WHERE id=?1", [&id])
        .map_err(|_| "Could not disconnect source.")?;
    Ok(())
}

struct Document {
    title: String,
    content: String,
}

fn read_text(path: &Path, budget: &mut usize) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "Source is unavailable.")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > *budget as u64 {
        return Err("Source exceeds the import limit or is not a regular file.".into());
    }
    // Read through a bounded reader even if the file grows after metadata was read.
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "Could not open source.")?
        .take((*budget + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read source.")?;
    if bytes.len() > *budget || bytes.contains(&0) {
        return Err("Source is too large or contains binary data.".into());
    }
    *budget -= bytes.len();
    String::from_utf8(bytes).map_err(|_| "Source must contain UTF-8 text.".into())
}

fn text_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("md" | "markdown" | "txt")
    )
}

fn folder(
    path: &Path,
    depth: usize,
    budget: &mut usize,
    visited: &mut usize,
    docs: &mut Vec<Document>,
) -> Result<(), String> {
    if depth > 3 {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(path)
        .map_err(|_| "Could not read folder.")?
        .take(1001)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read folder.")?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        *visited += 1;
        if *visited > 1000 {
            return Err("Folder contains too many entries. Select a smaller folder.".into());
        }
        let kind = entry
            .file_type()
            .map_err(|_| "Could not inspect folder entry.")?;
        let p = entry.path();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            folder(&p, depth + 1, budget, visited, docs)?;
        } else if kind.is_file() && text_extension(&p) {
            if docs.len() >= 50 {
                return Err("Select a folder containing at most 50 text files.".into());
            }
            docs.push(Document {
                title: entry.file_name().to_string_lossy().into_owned(),
                content: read_text(&p, budget)?,
            });
        }
    }
    Ok(())
}

fn calendar(text: &str) -> Result<Vec<Document>, String> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(&line[1..]);
            } else {
                return Err("Invalid calendar continuation.".into());
            }
        } else {
            lines.push(line.trim_end_matches('\r').to_owned());
        }
    }
    let mut docs = Vec::new();
    let mut event: Option<Vec<String>> = None;
    for line in lines {
        match line.as_str() {
            "BEGIN:VEVENT" => {
                if event.is_some() {
                    return Err("Invalid nested calendar event.".into());
                }
                event = Some(Vec::new());
            }
            "END:VEVENT" => {
                let fields = event.take().ok_or("Invalid calendar event.")?;
                let title = fields
                    .iter()
                    .find_map(|s| s.strip_prefix("SUMMARY:"))
                    .unwrap_or("Calendar event")
                    .to_owned();
                docs.push(Document {
                    title,
                    content: fields.join("\n"),
                });
                if docs.len() > 200 {
                    return Err("Calendar exceeds 200 events.".into());
                }
            }
            _ => {
                if let Some(fields) = event.as_mut() {
                    if let Some((key, value)) = line.split_once(':') {
                        if [
                            "UID",
                            "SUMMARY",
                            "DESCRIPTION",
                            "LOCATION",
                            "DTSTART",
                            "DTEND",
                        ]
                        .contains(&key.split(';').next().unwrap_or(""))
                        {
                            fields.push(format!(
                                "{key}:{}",
                                value
                                    .replace("\\n", "\n")
                                    .replace("\\,", ",")
                                    .replace("\\;", ";")
                                    .replace("\\\\", "\\")
                            ));
                        }
                    }
                }
            }
        }
    }
    if event.is_some() || !text.contains("BEGIN:VCALENDAR") || !text.contains("END:VCALENDAR") {
        return Err("Invalid or incomplete ICS calendar.".into());
    }
    Ok(docs)
}

#[tauri::command]
#[specta::specta]
pub async fn workspace_sync_connector(app: AppHandle, id: String) -> Result<Connector, String> {
    let (connector, revision) = {
        let conn = store::connection(&app)?;
        let c = list(&conn)?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or("Connection was removed.")?;
        // Only completed outcomes are durable. A crash during an import retains
        // the last known state rather than leaving a permanent 'syncing' record.
        let revision = reserve_sync(&conn, &id)?;
        (c, revision)
    };
    let result = {
        let source = connector.source.clone();
        let kind = connector.kind.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let path = Path::new(&source);
            let mut budget = MAX_BYTES;
            if std::fs::symlink_metadata(path)
                .map_err(|_| "Source is unavailable.")?
                .file_type()
                .is_symlink()
            {
                return Err("Symbolic links cannot be imported.".into());
            }
            match kind.as_str() {
                "calendar" => calendar(&read_text(path, &mut budget)?),
                "local_file" => {
                    if !text_extension(path) {
                        return Err("Choose a .txt or Markdown file.".into());
                    }
                    Ok(vec![Document {
                        title: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        content: read_text(path, &mut budget)?,
                    }])
                }
                "local_folder" => {
                    let mut docs = Vec::new();
                    folder(path, 0, &mut budget, &mut 0, &mut docs)?;
                    Ok(docs)
                }
                _ => Err("Unsupported connection type.".into()),
            }
        })
        .await
        .unwrap_or_else(|_| Err("Import worker stopped unexpectedly.".into()))
    };
    let mut conn = store::connection(&app)?;
    let tx = conn
        .transaction()
        .map_err(|_| "Could not save sync result.")?;
    let current: Option<i64> = tx
        .query_row(
            "SELECT revision FROM workspace_connectors WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "Could not read sync status.")?;
    if current != Some(revision) {
        return Err("Connection changed while syncing. Retry if still connected.".into());
    }
    match result {
        Ok(docs) => {
            tx.execute(
                "DELETE FROM workspace_connector_documents WHERE connector_id=?1",
                [&id],
            )
            .map_err(|_| "Could not replace cached source.")?;
            for doc in docs {
                tx.execute(
                    "INSERT INTO workspace_connector_documents VALUES(?1,?2,?3,?4,?5)",
                    params![store::new_id(), id, doc.title, doc.content, store::now_ms()],
                )
                .map_err(|_| "Could not cache source.")?;
            }
            tx.execute("UPDATE workspace_connectors SET status='ready',last_error=NULL,last_synced_at=?2 WHERE id=?1",params![id,store::now_ms()]).map_err(|_|"Could not save sync status.")?;
        }
        Err(error) => {
            tx.execute(
                "UPDATE workspace_connectors SET status='error',last_error=?2 WHERE id=?1",
                params![id, error],
            )
            .map_err(|_| "Could not save sync failure.")?;
            tx.commit().map_err(|_| "Could not save sync failure.")?;
            return Err(error);
        }
    }
    tx.commit().map_err(|_| "Could not save imported source.")?;
    list(&conn)?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or("Connection was removed.".into())
}
use rusqlite::OptionalExtension;

fn reserve_sync(conn: &Connection, id: &str) -> Result<i64, String> {
    conn.query_row(
        "UPDATE workspace_connectors SET revision=revision+1 WHERE id=?1 AND kind IN ('local_file','local_folder','calendar') RETURNING revision",
        [id],
        |r| r.get::<_, i64>(0),
    )
    .map_err(|_| LOCAL_ONLY_ERROR)
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_unfolds_and_rejects_incomplete_events() {
        let docs=calendar("BEGIN:VCALENDAR\nBEGIN:VEVENT\nSUMMARY:Team\n sync\nDESCRIPTION:One\\nTwo\nEND:VEVENT\nEND:VCALENDAR").unwrap();
        assert_eq!(docs[0].title, "Teamsync");
        assert!(docs[0].content.contains("One\nTwo"));
        assert!(calendar("BEGIN:VCALENDAR\nBEGIN:VEVENT\nEND:VCALENDAR").is_err());
    }
    #[test]
    fn rejects_remote_connections_even_with_valid_sources() {
        for source in [
            "a/b/c",
            "../repo",
            "a/b?x=1",
            "a/",
            "https://github.com/a/b",
            "valid-owner/valid-repo",
        ] {
            assert!(validate(&ConnectorInput {
                name: "test".into(),
                kind: "github".into(),
                source: source.into(),
            })
            .is_err());
        }
        for kind in ["github", "google_calendar", "oauth", "https"] {
            assert_eq!(
                validate(&ConnectorInput {
                    name: "remote".into(),
                    kind: kind.into(),
                    source: "/tmp/plausible-local-path".into()
                })
                .unwrap_err(),
                LOCAL_ONLY_ERROR
            );
        }
        assert!(serde_json::from_str::<ConnectorInput>(r#"{"name":"local","kind":"local_file","source":"/tmp/a.md","token":"synthetic-unused-token"}"#).is_err());
    }
    #[test]
    fn disconnect_cascades_cache() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch("INSERT INTO workspace_connectors VALUES('a','local_file','test','/tmp/a','ready',NULL,NULL,1);INSERT INTO workspace_connector_documents VALUES('d','a','title','private content',0);DELETE FROM workspace_connectors WHERE id='a';").unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM workspace_connector_documents",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn file_budget_rejects_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        std::fs::write(&path, "12345").unwrap();
        assert!(read_text(&path, &mut 4).is_err());
    }
    #[test]
    fn legacy_remote_sources_are_disabled_without_erasing_cached_content() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch("INSERT INTO workspace_connectors VALUES('old','github','Old repo','owner/repo','ready',100,NULL,7); INSERT INTO workspace_connector_documents VALUES('cached','old','README','Saved local text',100)").unwrap();
        let migrated = list(&conn).unwrap();
        assert_eq!(migrated[0].status, "disabled");
        assert_eq!(migrated[0].last_synced_at, Some(100));
        assert_eq!(migrated[0].document_count, 1);
        assert_eq!(migrated[0].revision, 8);
        assert_eq!(reserve_sync(&conn, "old").unwrap_err(), LOCAL_ONLY_ERROR);
        assert_eq!(list(&conn).unwrap()[0].revision, 8);
        assert_eq!(
            conn.query_row(
                "SELECT content FROM workspace_connector_documents WHERE id='cached'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "Saved local text"
        );
        conn.execute("DELETE FROM workspace_connectors WHERE id='old'", [])
            .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM workspace_connector_documents",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    #[test]
    fn overlapping_syncs_reserve_distinct_revisions_without_erasing_last_outcome() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute_batch("INSERT INTO workspace_connectors VALUES('a','local_file','test','/tmp/test.md','error',100,'Import failed',1)").unwrap();
        let first = reserve_sync(&conn, "a").unwrap();
        let latest = reserve_sync(&conn, "a").unwrap();
        assert_eq!(first, 2);
        assert_eq!(latest, 3);
        let saved: (i64, String, String) = conn
            .query_row(
                "SELECT revision,status,last_error FROM workspace_connectors WHERE id='a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_ne!(saved.0, first);
        assert_eq!(saved.0, latest);
        assert_eq!(saved.1, "error");
        assert_eq!(saved.2, "Import failed");
        conn.execute("DELETE FROM workspace_connectors WHERE id='a'", [])
            .unwrap();
        assert!(reserve_sync(&conn, "a").is_err());
    }
}
