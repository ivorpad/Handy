use super::{context::Destination, store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct WritingProfile {
    pub id: String,
    pub name: String,
    pub app_id: Option<String>,
    pub domain: Option<String>,
    pub prompt_id: Option<String>,
    pub cleanup_level: String,
    pub priority: i32,
    pub enabled: bool,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct WritingPreference {
    pub id: String,
    pub text: String,
    pub app_id: Option<String>,
    pub domain: Option<String>,
    pub language: Option<String>,
    pub enabled: bool,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

pub(crate) fn init_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_profiles (
        id TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS workspace_preferences (
        id TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS workspace_profile_selection (
        slot INTEGER PRIMARY KEY CHECK(slot = 1),
        profile_id TEXT REFERENCES workspace_profiles(id) ON DELETE SET NULL);",
    )
    .map_err(|e| e.to_string())
}
fn connection(app: &AppHandle) -> Result<Connection, String> {
    let conn = store::connection(app)?;
    init_schema(&conn)?;
    Ok(conn)
}
fn optional(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}
fn normalize_domain(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = optional(value) else {
        return Ok(None);
    };
    let value = value.trim_end_matches('.').to_ascii_lowercase();
    if value.len() > 253
        || value.contains(['/', ':', ' ', '*'])
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err("Enter a hostname such as example.com, without a URL or wildcard.".into());
    }
    Ok(Some(value))
}
fn validate_scope(app_id: &mut Option<String>, domain: &mut Option<String>) -> Result<(), String> {
    *app_id = optional(app_id.take());
    if app_id
        .as_ref()
        .is_some_and(|v| v.len() > 256 || v.chars().any(char::is_control))
    {
        return Err("Application ID is invalid.".into());
    }
    *domain = normalize_domain(domain.take())?;
    Ok(())
}
fn host_matches(rule: &str, host: &str) -> bool {
    host.eq_ignore_ascii_case(rule)
        || host
            .to_ascii_lowercase()
            .strip_suffix(&format!(".{}", rule.to_ascii_lowercase()))
            .is_some()
}
fn scope_matches(app_id: Option<&str>, domain: Option<&str>, destination: &Destination) -> bool {
    app_id.is_none_or(|id| id == destination.app_id)
        && domain.is_none_or(|rule| {
            destination
                .domain
                .as_deref()
                .is_some_and(|host| host_matches(rule, host))
        })
}
fn profiles(conn: &Connection) -> Result<Vec<WritingProfile>, String> {
    let mut query = conn
        .prepare("SELECT payload FROM workspace_profiles ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|row| {
        serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    })
    .collect()
}
fn preferences(conn: &Connection) -> Result<Vec<WritingPreference>, String> {
    let mut query = conn
        .prepare("SELECT payload FROM workspace_preferences ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|row| {
        serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    })
    .collect()
}
fn resolved(mut rows: Vec<WritingProfile>, destination: &Destination) -> Option<WritingProfile> {
    rows.retain(|p| {
        p.enabled && scope_matches(p.app_id.as_deref(), p.domain.as_deref(), destination)
    });
    rows.sort_by(|a, b| {
        let rank = |p: &WritingProfile| {
            if p.domain.is_some() {
                2
            } else if p.app_id.is_some() {
                1
            } else {
                0
            }
        };
        rank(b)
            .cmp(&rank(a))
            .then_with(|| b.priority.cmp(&a.priority))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows.into_iter().next()
}
pub fn resolve(
    app: &AppHandle,
    destination: &Destination,
) -> Result<Option<WritingProfile>, String> {
    resolve_stored(&connection(app)?, destination)
}
fn override_id(conn: &Connection) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT profile_id FROM workspace_profile_selection WHERE slot=1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(|e| e.to_string())
}
fn resolve_stored(
    conn: &Connection,
    destination: &Destination,
) -> Result<Option<WritingProfile>, String> {
    let rows = profiles(conn)?;
    if let Some(id) = override_id(conn)? {
        if let Some(selected) = rows.iter().find(|p| p.enabled && p.id == id) {
            return Ok(Some(selected.clone()));
        }
    }
    Ok(resolved(rows, destination))
}
fn set_override(conn: &mut Connection, id: Option<&str>) -> Result<Option<String>, String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if let Some(id) = id {
        if !profiles(&tx)?.iter().any(|p| p.enabled && p.id == id) {
            return Err("Choose an enabled profile, or use automatic selection.".into());
        }
    }
    tx.execute("INSERT INTO workspace_profile_selection(slot,profile_id) VALUES (1,?1) ON CONFLICT(slot) DO UPDATE SET profile_id=excluded.profile_id", [id]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id.map(str::to_owned))
}
#[tauri::command]
#[specta::specta]
pub fn get_writing_profile_override(app: AppHandle) -> Result<Option<String>, String> {
    let conn = connection(&app)?;
    let selected = override_id(&conn)?;
    let rows = profiles(&conn)?;
    Ok(selected.filter(|id| rows.iter().any(|p| p.enabled && &p.id == id)))
}
#[tauri::command]
#[specta::specta]
pub fn set_writing_profile_override(
    app: AppHandle,
    profile_id: Option<String>,
) -> Result<Option<String>, String> {
    set_override(&mut connection(&app)?, profile_id.as_deref())
}
pub fn preferences_for(
    app: &AppHandle,
    destination: &Destination,
    language: Option<&str>,
) -> Result<Vec<WritingPreference>, String> {
    Ok(preferences(&connection(app)?)?
        .into_iter()
        .filter(|p| preference_matches(p, destination, language))
        .collect())
}
fn preference_matches(
    p: &WritingPreference,
    destination: &Destination,
    language: Option<&str>,
) -> bool {
    p.enabled
        && scope_matches(p.app_id.as_deref(), p.domain.as_deref(), destination)
        && p.language.as_deref().is_none_or(|rule| {
            language.is_some_and(|l| {
                l.eq_ignore_ascii_case(rule)
                    || l.to_ascii_lowercase()
                        .starts_with(&format!("{}-", rule.to_ascii_lowercase()))
            })
        })
}
fn save_payload(
    conn: &Connection,
    table: &str,
    id: &str,
    expected: i64,
    payload: &str,
) -> Result<(), String> {
    // Table names are fixed at the two call sites, never supplied by an invoke argument.
    if expected == 0 {
        conn.execute(
            &format!("INSERT INTO {table}(id,payload,revision) VALUES (?1,?2,1)"),
            params![id, payload],
        )
        .map_err(|e| e.to_string())?;
    } else if conn
        .execute(
            &format!(
                "UPDATE {table} SET payload=?1,revision=revision+1 WHERE id=?2 AND revision=?3"
            ),
            params![payload, id, expected],
        )
        .map_err(|e| e.to_string())?
        != 1
    {
        return Err("This record changed elsewhere. Reload it before saving.".into());
    }
    Ok(())
}
fn delete_payload(conn: &Connection, table: &str, id: &str, revision: i64) -> Result<(), String> {
    if conn
        .execute(
            &format!("DELETE FROM {table} WHERE id=?1 AND revision=?2"),
            params![id, revision],
        )
        .map_err(|e| e.to_string())?
        != 1
    {
        return Err("This record changed elsewhere. Reload it before deleting.".into());
    }
    Ok(())
}
fn original_created(
    conn: &Connection,
    table: &str,
    id: &str,
    revision: i64,
) -> Result<i64, String> {
    if revision == 0 {
        return Ok(store::now_ms());
    }
    let raw: Option<String> = conn
        .query_row(
            &format!("SELECT payload FROM {table} WHERE id=?1 AND revision=?2"),
            params![id, revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_str(
        &raw.ok_or("This record changed elsewhere. Reload it before saving.")?,
    )
    .map_err(|e| e.to_string())?;
    value["created_at"]
        .as_i64()
        .ok_or_else(|| "Stored creation time is invalid.".into())
}

#[tauri::command]
#[specta::specta]
pub fn list_writing_profiles(app: AppHandle) -> Result<Vec<WritingProfile>, String> {
    profiles(&connection(&app)?)
}

#[tauri::command]
#[specta::specta]
pub fn save_writing_profile(
    app: AppHandle,
    mut profile: WritingProfile,
) -> Result<WritingProfile, String> {
    profile.name = profile.name.trim().to_owned();
    if profile.name.is_empty() || profile.name.len() > 160 {
        return Err("Profile name must contain 1 to 160 bytes.".into());
    }
    validate_scope(&mut profile.app_id, &mut profile.domain)?;
    profile.prompt_id = optional(profile.prompt_id);
    if profile.prompt_id.as_ref().is_some_and(|id| {
        !crate::settings::get_settings(&app)
            .post_process_prompts
            .iter()
            .any(|prompt| &prompt.id == id)
    }) {
        return Err("This prompt no longer exists. Choose an available prompt.".into());
    }
    if !["inherit", "none", "light", "medium"].contains(&profile.cleanup_level.as_str()) {
        return Err("Unknown cleanup level.".into());
    }
    if profile.revision < 0 {
        return Err("Invalid revision.".into());
    }
    let conn = connection(&app)?;
    let expected = profile.revision;
    if expected == 0 {
        profile.id = store::new_id();
    }
    profile.created_at = original_created(&conn, "workspace_profiles", &profile.id, expected)?;
    profile.updated_at = store::now_ms();
    profile.revision += 1;
    save_payload(
        &conn,
        "workspace_profiles",
        &profile.id,
        expected,
        &serde_json::to_string(&profile).map_err(|e| e.to_string())?,
    )?;
    Ok(profile)
}

#[tauri::command]
#[specta::specta]
pub fn delete_writing_profile(app: AppHandle, id: String, revision: i64) -> Result<(), String> {
    delete_payload(&connection(&app)?, "workspace_profiles", &id, revision)
}

#[tauri::command]
#[specta::specta]
pub fn resolve_writing_profile(
    app: AppHandle,
    destination: Destination,
) -> Result<Option<WritingProfile>, String> {
    resolve(&app, &destination)
}

#[tauri::command]
#[specta::specta]
pub fn list_writing_preferences(app: AppHandle) -> Result<Vec<WritingPreference>, String> {
    preferences(&connection(&app)?)
}

#[tauri::command]
#[specta::specta]
pub fn save_writing_preference(
    app: AppHandle,
    mut preference: WritingPreference,
) -> Result<WritingPreference, String> {
    preference.text = preference.text.trim().to_owned();
    if preference.text.is_empty() || preference.text.len() > 4000 {
        return Err("Preference must contain 1 to 4000 bytes.".into());
    }
    validate_scope(&mut preference.app_id, &mut preference.domain)?;
    preference.language = optional(preference.language).map(|v| v.to_ascii_lowercase());
    if preference
        .language
        .as_ref()
        .is_some_and(|v| v.len() > 35 || !v.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
    {
        return Err("Use a language code such as en or en-GB.".into());
    }
    if preference.revision < 0 {
        return Err("Invalid revision.".into());
    }
    let conn = connection(&app)?;
    let expected = preference.revision;
    if expected == 0 {
        preference.id = store::new_id();
    }
    preference.created_at =
        original_created(&conn, "workspace_preferences", &preference.id, expected)?;
    preference.updated_at = store::now_ms();
    preference.revision += 1;
    save_payload(
        &conn,
        "workspace_preferences",
        &preference.id,
        expected,
        &serde_json::to_string(&preference).map_err(|e| e.to_string())?,
    )?;
    Ok(preference)
}

#[tauri::command]
#[specta::specta]
pub fn delete_writing_preference(app: AppHandle, id: String, revision: i64) -> Result<(), String> {
    delete_payload(&connection(&app)?, "workspace_preferences", &id, revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile(id: &str, app: Option<&str>, domain: Option<&str>, priority: i32) -> WritingProfile {
        WritingProfile {
            id: id.into(),
            name: id.into(),
            app_id: app.map(str::to_owned),
            domain: domain.map(str::to_owned),
            prompt_id: None,
            cleanup_level: "inherit".into(),
            priority,
            enabled: true,
            revision: 1,
            created_at: 0,
            updated_at: 0,
        }
    }
    fn destination() -> Destination {
        Destination {
            app_id: "browser".into(),
            app_name: "Browser".into(),
            domain: Some("mail.example.com".into()),
            selected_text: None,
            token: "test".into(),
            captured_at: 0,
            context_available: false,
            unavailable_reason: None,
        }
    }
    #[test]
    fn domain_precedes_app_and_priority_ties_are_stable() {
        let rows = vec![
            profile("default", None, None, 999),
            profile("app", Some("browser"), None, 999),
            profile("z", None, Some("example.com"), 1),
            profile("a", None, Some("example.com"), 1),
        ];
        assert_eq!(resolved(rows, &destination()).unwrap().id, "a");
        assert!(!host_matches("example.com", "notexample.com"));
        assert!(!host_matches("example.com", "example.com.evil.test"));
    }
    #[test]
    fn disabled_and_mismatched_app_are_excluded() {
        let mut disabled = profile("disabled", None, Some("example.com"), 999);
        disabled.enabled = false;
        assert_eq!(
            resolved(
                vec![
                    disabled,
                    profile("wrong", Some("other"), Some("example.com"), 99),
                    profile("fallback", None, None, 0)
                ],
                &destination()
            )
            .unwrap()
            .id,
            "fallback"
        );
    }
    #[test]
    fn revision_conflicts_do_not_overwrite_or_delete() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        save_payload(&conn, "workspace_profiles", "a", 0, "first").unwrap();
        save_payload(&conn, "workspace_profiles", "a", 1, "second").unwrap();
        assert!(save_payload(&conn, "workspace_profiles", "a", 1, "stale").is_err());
        assert!(delete_payload(&conn, "workspace_profiles", "a", 1).is_err());
        let saved: String = conn
            .query_row(
                "SELECT payload FROM workspace_profiles WHERE id='a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, "second");
    }
    #[test]
    fn domain_input_rejects_urls_and_wildcards() {
        assert_eq!(
            normalize_domain(Some(" Example.COM. ".into())).unwrap(),
            Some("example.com".into())
        );
        for invalid in [
            "https://example.com",
            "*.example.com",
            "a..com",
            "example.com/path",
            "example.com:80",
        ] {
            assert!(normalize_domain(Some(invalid.into())).is_err());
        }
    }
    #[test]
    fn preferences_require_all_scopes_and_known_language() {
        let mut p = WritingPreference {
            id: "p".into(),
            text: "Use British spelling".into(),
            app_id: Some("browser".into()),
            domain: Some("example.com".into()),
            language: Some("en".into()),
            enabled: true,
            revision: 1,
            created_at: 0,
            updated_at: 0,
        };
        assert!(preference_matches(&p, &destination(), Some("EN-gb")));
        assert!(!preference_matches(&p, &destination(), None));
        assert!(!preference_matches(&p, &destination(), Some("eng")));
        p.app_id = Some("other".into());
        assert!(!preference_matches(&p, &destination(), Some("en")));
    }
    #[test]
    fn manual_selection_persists_and_overrides_scopes_then_returns_to_automatic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profiles.db");
        let mut conn = Connection::open(&path).unwrap();
        init_schema(&conn).unwrap();
        let manual = profile("manual", Some("other-app"), None, 0);
        let automatic = profile("auto", None, Some("example.com"), 99);
        for p in [&manual, &automatic] {
            save_payload(
                &conn,
                "workspace_profiles",
                &p.id,
                0,
                &serde_json::to_string(p).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            resolve_stored(&conn, &destination()).unwrap().unwrap().id,
            "auto"
        );
        set_override(&mut conn, Some("manual")).unwrap();
        drop(conn);
        let mut conn = Connection::open(&path).unwrap();
        assert_eq!(override_id(&conn).unwrap().as_deref(), Some("manual"));
        assert_eq!(
            resolve_stored(&conn, &destination()).unwrap().unwrap().id,
            "manual"
        );
        set_override(&mut conn, None).unwrap();
        assert_eq!(
            resolve_stored(&conn, &destination()).unwrap().unwrap().id,
            "auto"
        );
    }
    #[test]
    fn disabled_deleted_and_unknown_override_fall_back_without_hiding_errors() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        let mut manual = profile("manual", None, None, 0);
        let automatic = profile("auto", None, Some("example.com"), 0);
        for p in [&manual, &automatic] {
            save_payload(
                &conn,
                "workspace_profiles",
                &p.id,
                0,
                &serde_json::to_string(p).unwrap(),
            )
            .unwrap();
        }
        set_override(&mut conn, Some("manual")).unwrap();
        manual.enabled = false;
        save_payload(
            &conn,
            "workspace_profiles",
            "manual",
            1,
            &serde_json::to_string(&manual).unwrap(),
        )
        .unwrap();
        assert_eq!(
            resolve_stored(&conn, &destination()).unwrap().unwrap().id,
            "auto"
        );
        assert!(set_override(&mut conn, Some("manual")).is_err());
        assert!(set_override(&mut conn, Some("unknown")).is_err());
        delete_payload(&conn, "workspace_profiles", "manual", 2).unwrap();
        assert_eq!(
            resolve_stored(&conn, &destination()).unwrap().unwrap().id,
            "auto"
        );
    }
}
