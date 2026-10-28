# Voice workspace

Implement the feature gaps identified in the Wispr comparison as original
Handy code: spoken snippets, destination profiles, correction learning,
selected-text commands, scoped writing preferences, history lookup,
scratchpad revisions, meeting capture/notes and read-only connectors.

The follow-up [local expansion contract](voice-workspace-local-expansion.md)
adds opt-in automation, speakers and platform adapters, and restricts all
generation and connectors to local processing.

Existing dictation, local models, history and configurable local AI providers remain
the foundations. New records live in a separate local `workspace.db`. Existing
history migrations are unchanged. No account or external service is required
for snippets, profiles, vocabulary, notes or search. Generation uses the
local provider explicitly configured by the person. Unavailable models, permissions,
platform capabilities and services return visible errors.

## Contracts

- Snippets expand by exact normalized phrase boundaries, longest match first.
  Expansions are literal and non-recursive. Preserve their contents through
  AI cleanup. Rich text must fall back visibly or safely to plain text.
- Profiles resolve against a destination snapshot taken at recording start.
  Domain rules precede app rules; priority and stable IDs resolve ties. The
  default profile handles no match. Reading field text is separate from app
  identity and is opt-in. Sensitive fields are excluded.
- Manual learning records a proposed vocabulary word or an explicit replacement
  rule after confirmation. Opt-in background learning adds only conservative
  spelling corrections as vocabulary. Both support undo without erasing
  intervening edits. Corrections must originate from an actual edit.
- Commands keep instruction and selected text distinct. Results are previewed;
  replacement checks the original target and selection, and never submits a
  message. Cancellation/stale sessions cannot paste late output.
- Writing preferences have app/domain/language scopes and remain editable.
  History answers cite actual local entries; missing evidence is explicit.
- Notes autosave with optimistic revision checks. Document versions survive
  reopening. Restore creates a new head; conflicts preserve the local draft.
- Meetings start only from an explicit record action, keep recoverable local
  audio, expose available input sources and preserve failure states. Imported
  recordings use the same transcription/summary pipeline. Action items are
  drafts and require confirmation. Interrupted recordings remain recoverable.
- Connections are explicitly configured local read-only data sources.
  Disconnect removes cached source content. Import failures must not be
  represented as successful syncs. Legacy remote entries are disabled without
  deleting cached content.

## Shared implementation interfaces

Root owns `workspace/mod.rs`, `store.rs`, `generation.rs`, `lib.rs` command
registration, the workspace navigation shell and i18n registration.
Feature workers own their named workspace Rust modules, React panels and
English translation fragments. Every command uses `#[tauri::command]` and
`#[specta::specta]`; public payloads derive Serialize, Deserialize and Type.

`store::connection(&AppHandle) -> Result<rusqlite::Connection, String>` opens
the local database with foreign keys and a busy timeout. Feature modules own
their namespaced tables and idempotent initialization. `store::new_id()`
returns a local unique string; `store::now_ms()` returns i64.

`generation::generate(&AppHandle, instruction: &str, input: &str) ->
Result<String, String>` invokes the configured provider and rejects empty
output. Feature-specific structured output is validated by its caller.
Do not hold a SQLite connection/transaction across await points.

Frontend panels are default exports under `src/components/workspace/` and use
typed `invoke` calls. Each feature owns `src/i18n/locales/en/workspace-NAME.json`
with its direct key object, mounted at `workspace.NAME`. Root wires navigation.

## Dependencies and checks

Shared storage/generation precede feature integration. Snippets and profiles
precede dictation pipeline integration; native target capture precedes safe
selection replacement. Notes and connectors precede cross-source search.
Meeting capture and recovery precede generation of notes and draft tasks.

Run focused Rust tests for boundaries, precedence, revision conflicts,
stale/cancelled results, retry recovery and secret redaction. Build and lint
the frontend, check Rust compilation/formatting and inspect running UI journeys
with synthetic records, including reopen and a failure case. Real microphone,
system audio, AI quality and external account behavior require available
runtime prerequisites; report any unverified path precisely.

Rollback removes the new navigation and processing integrations. The separate
database and recording directory can remain intact for recovery; rollback
never deletes user records. No production deployment or account activation
is part of this implementation.
