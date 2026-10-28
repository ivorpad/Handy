import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { Button } from "@/components/ui/Button";

type Note = {
  id: string;
  title: string;
  content: string;
  revision: number;
  updated_at: number;
};
type Summary = Omit<Note, "content"> & { preview: string };
type Version = {
  revision: number;
  title: string;
  content: string;
  source: string;
  created_at: number;
};
type Message = {
  id: string;
  role: string;
  content: string;
  note_revision: number;
};
type Generated = {
  note_id: string;
  based_on_revision: number;
  content: string;
};
type Draft = { title: string; content: string };
type Recovery = { note: Note | null; draft: Draft };
const draftStorageKey = "handy.workspace.notes.draft.v1";
function readRecovery(): {
  value: Recovery | null;
  raw: string | null;
  failed: boolean;
} {
  try {
    const raw = localStorage.getItem(draftStorageKey);
    if (!raw) return { value: null, raw: null, failed: false };
    const candidate: unknown = JSON.parse(raw);
    if (
      !candidate ||
      typeof candidate !== "object" ||
      !("draft" in candidate) ||
      !("note" in candidate)
    )
      throw new Error("Invalid draft");
    const { draft, note } = candidate;
    if (
      !draft ||
      typeof draft !== "object" ||
      !("title" in draft) ||
      typeof draft.title !== "string" ||
      !("content" in draft) ||
      typeof draft.content !== "string"
    )
      throw new Error("Invalid draft");
    if (
      note !== null &&
      (!note ||
        typeof note !== "object" ||
        !("id" in note) ||
        typeof note.id !== "string" ||
        !("revision" in note) ||
        typeof note.revision !== "number" ||
        !Number.isSafeInteger(note.revision) ||
        note.revision < 1 ||
        !("title" in note) ||
        typeof note.title !== "string" ||
        !("content" in note) ||
        typeof note.content !== "string" ||
        !("updated_at" in note) ||
        typeof note.updated_at !== "number")
    )
      throw new Error("Invalid draft");
    return {
      value: {
        note: note as Note | null,
        draft: { title: draft.title, content: draft.content },
      },
      raw,
      failed: false,
    };
  } catch {
    return { value: null, raw: null, failed: true };
  }
}
const field =
  "w-full rounded-lg border border-line bg-background p-2 text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-logo-primary";

export default function NotesPanel() {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const tr = (key: string, options?: Record<string, unknown>) =>
    t(`workspace.notes.${key}`, options);
  const [recovery] = useState(readRecovery);
  const [recovered, setRecovered] = useState(Boolean(recovery.value));
  const [backupFailed, setBackupFailed] = useState(recovery.failed);
  const [notes, setNotes] = useState<Summary[]>([]);
  const [search, setSearch] = useState("");
  const [note, setNote] = useState<Note | null>(recovery.value?.note ?? null);
  const [draft, setDraft] = useState<Draft>(
    recovery.value?.draft ?? { title: "", content: "" },
  );
  const [dirty, setDirty] = useState(Boolean(recovery.value));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [blocked, setBlocked] = useState(Boolean(recovery.value));
  const [versions, setVersions] = useState<Version[] | null>(null);
  const [version, setVersion] = useState<Version | null>(null);
  const [promptId, setPromptId] = useState("");
  const [instruction, setInstruction] = useState("");
  const [generated, setGenerated] = useState<Generated | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [question, setQuestion] = useState("");
  const [busy, setBusy] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const noteRef = useRef<Note | null>(note);
  const draftRef = useRef(draft);
  const dirtyRef = useRef(dirty);
  const backupRef = useRef(recovery.raw);
  const epoch = useRef(0);
  const pending = useRef<Promise<Note | null> | null>(null);
  const queryRef = useRef("");
  const fileRef = useRef<HTMLInputElement>(null);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const refresh = useCallback(async () => {
    const query = queryRef.current;
    const result = await invoke<Summary[]>("workspace_list_notes", { query });
    if (query === queryRef.current) setNotes(result);
  }, []);
  const report = (cause: unknown) => setError(String(cause));
  const backup = (head: Note | null, next: Draft) => {
    try {
      const raw = JSON.stringify({ note: head, draft: next });
      localStorage.setItem(draftStorageKey, raw);
      backupRef.current = raw;
      setBackupFailed(false);
    } catch {
      setBackupFailed(true);
    }
  };
  const clearBackup = (expected: string | null) => {
    try {
      // An older in-flight save must not erase a newer editor's backup.
      if (
        expected !== null &&
        localStorage.getItem(draftStorageKey) === expected
      )
        localStorage.removeItem(draftStorageKey);
    } catch {
      setBackupFailed(true);
    }
  };
  const save = useCallback(
    async (source = "typed"): Promise<Note | null> => {
      while (pending.current) {
        const result = await pending.current;
        if (!result) return null;
      }
      if (!dirtyRef.current) return noteRef.current;
      const snapshot = { ...draftRef.current };
      if (
        !snapshot.title.trim() &&
        !snapshot.content.trim() &&
        !noteRef.current
      )
        return null;
      const selected = epoch.current;
      const head = noteRef.current;
      const savedBackup = backupRef.current;
      setSaving(true);
      const operation = invoke<Note>("workspace_save_note", {
        request: {
          id: head?.id ?? null,
          expected_revision: head?.revision ?? null,
          ...snapshot,
          source,
        },
      })
        .then((saved) => {
          if (selected === epoch.current) {
            noteRef.current = saved;
            setNote(saved);
            const changed =
              draftRef.current.title !== snapshot.title ||
              draftRef.current.content !== snapshot.content;
            dirtyRef.current = changed;
            setDirty(changed);
            if (!changed) {
              clearBackup(savedBackup);
              setRecovered(false);
            }
            setBlocked(false);
            setError("");
          }
          void refresh().catch(report);
          return saved;
        })
        .catch((cause: unknown) => {
          if (selected === epoch.current) {
            report(cause);
            setBlocked(true);
          }
          return null;
        })
        .finally(() => {
          pending.current = null;
          setSaving(false);
        });
      pending.current = operation;
      return operation;
    },
    [refresh],
  );
  const install = (saved: Note | null) => {
    clearBackup(backupRef.current);
    backupRef.current = null;
    setRecovered(false);
    epoch.current += 1;
    noteRef.current = saved;
    setNote(saved);
    const next = { title: saved?.title ?? "", content: saved?.content ?? "" };
    draftRef.current = next;
    setDraft(next);
    dirtyRef.current = false;
    setDirty(false);
    setBlocked(false);
    setError("");
    setVersions(null);
    setVersion(null);
    setGenerated(null);
    setMessages([]);
    setDeleting(false);
    if (saved) {
      const selected = epoch.current;
      void invoke<Message[]>("workspace_note_messages", { noteId: saved.id })
        .then((result) => {
          if (selected === epoch.current) setMessages(result);
        })
        .catch(report);
    }
  };
  const flush = async () => {
    if (dirtyRef.current) {
      const saved = await save();
      if (!saved || dirtyRef.current) return false;
    }
    return true;
  };
  const select = async (id?: string) => {
    setBusy(true);
    try {
      if (!(await flush())) return;
      install(id ? await invoke<Note>("workspace_get_note", { id }) : null);
    } catch (cause) {
      report(cause);
    } finally {
      setBusy(false);
    }
  };
  const edit = (key: keyof Draft, value: string) => {
    const next = { ...draftRef.current, [key]: value };
    draftRef.current = next;
    setDraft(next);
    dirtyRef.current = true;
    setDirty(true);
    backup(noteRef.current, next);
  };
  useEffect(() => {
    queryRef.current = search;
    const timer = setTimeout(() => void refresh().catch(report), 200);
    return () => clearTimeout(timer);
  }, [search, refresh]);
  useEffect(() => {
    if (!dirty || blocked) return;
    const timer = setTimeout(() => void save(), 1500);
    return () => clearTimeout(timer);
  }, [draft, dirty, blocked, save]);
  useEffect(() => {
    const warn = (event: BeforeUnloadEvent) => {
      if (dirtyRef.current && backupFailed) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", warn);
    return () => {
      window.removeEventListener("beforeunload", warn);
    };
  }, [backupFailed]);
  const run = async (operation: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await operation();
    } catch (cause) {
      report(cause);
    } finally {
      setBusy(false);
    }
  };
  const showHistory = () =>
    run(async () => {
      if (!(await flush()) || !noteRef.current) return;
      setVersions(
        await invoke<Version[]>("workspace_note_versions", {
          noteId: noteRef.current.id,
        }),
      );
    });
  const transform = () =>
    run(async () => {
      if (!(await flush()) || !noteRef.current) return;
      const selected = epoch.current;
      const head = noteRef.current;
      const result = await invoke<Generated>("workspace_note_transform", {
        noteId: head.id,
        expectedRevision: head.revision,
        promptId: promptId || null,
        instruction,
      });
      if (selected === epoch.current) setGenerated(result);
    });
  const apply = () =>
    run(async () => {
      const head = noteRef.current;
      if (
        !generated ||
        !head ||
        dirtyRef.current ||
        generated.note_id !== head.id ||
        generated.based_on_revision !== head.revision
      )
        throw new Error(tr("stalePreview"));
      install(
        await invoke<Note>("workspace_save_note", {
          request: {
            id: head.id,
            expected_revision: head.revision,
            title: head.title,
            content: generated.content,
            source: "transform",
          },
        }),
      );
      await refresh();
    });
  const ask = () =>
    run(async () => {
      if (!(await flush()) || !noteRef.current) return;
      const head = noteRef.current;
      const selected = epoch.current;
      const result = await invoke<Message[]>("workspace_note_chat", {
        noteId: head.id,
        expectedRevision: head.revision,
        message: question,
      });
      if (selected === epoch.current) {
        setMessages(result);
        setQuestion("");
      }
    });
  const exportNote = () =>
    run(async () => {
      if (!(await flush()) || !noteRef.current) return;
      const result = await invoke<{ filename: string; markdown: string }>(
        "workspace_export_note",
        { noteId: noteRef.current.id },
      );
      const url = URL.createObjectURL(
        new Blob([result.markdown], { type: "text/markdown;charset=utf-8" }),
      );
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = result.filename;
      anchor.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    });
  const importNote = (file?: File) =>
    run(async () => {
      if (!file || !(await flush())) return;
      if (file.size > 2_000_000) throw new Error(tr("importTooLarge"));
      const markdown = await file.text();
      install(
        await invoke<Note>("workspace_import_note", {
          title: file.name.replace(/\.[^.]+$/, "").slice(0, 160),
          markdown,
        }),
      );
      await refresh();
    });
  return (
    <section className="space-y-4" aria-label={tr("title")}>
      <h2 className="text-lg font-semibold">{tr("title")}</h2>
      <p className="text-sm text-text/70">{tr("dictationHelp")}</p>
      {recovered && (
        <p role="status" className="rounded-lg border border-line p-3 text-sm">
          {tr("recoveredDraft")}
        </p>
      )}
      {backupFailed && (
        <p
          role="alert"
          className="rounded-lg border border-error p-3 text-error"
        >
          {tr("backupFailed")}
        </p>
      )}
      {error && (
        <div
          role="alert"
          className="rounded-lg border border-error p-3 text-error"
        >
          {error}
        </div>
      )}
      {blocked && (
        <div className="flex flex-wrap gap-2">
          <p className="text-sm">{tr("conflictHelp")}</p>
          <Button
            variant="secondary"
            disabled={!note || busy}
            onClick={() =>
              run(async () => {
                const head = noteRef.current;
                if (!head) return;
                const saved = await invoke<Note>("workspace_get_note", {
                  id: head.id,
                });
                noteRef.current = saved;
                setNote(saved);
                backup(saved, draftRef.current);
                setBlocked(true);
                setError("");
              })
            }
          >
            {tr("rebaseDraft")}
          </Button>
          <Button
            variant="secondary"
            onClick={() =>
              run(async () => {
                install(
                  await invoke<Note>("workspace_import_note", {
                    ...draftRef.current,
                    markdown: draftRef.current.content,
                  }),
                );
                await refresh();
              })
            }
          >
            {tr("saveCopy")}
          </Button>
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        <Button onClick={() => void select()} disabled={busy || saving}>
          {tr("new")}
        </Button>
        <Button
          variant="secondary"
          onClick={() => fileRef.current?.click()}
          disabled={busy}
        >
          {tr("import")}
        </Button>
        <input
          ref={fileRef}
          type="file"
          accept=".md,.markdown,.txt,text/markdown,text/plain"
          className="hidden"
          aria-label={tr("import")}
          onChange={(event) => {
            void importNote(event.target.files?.[0]);
            event.target.value = "";
          }}
        />
        <Button
          variant="secondary"
          onClick={() => void exportNote()}
          disabled={busy || (!note && !dirty)}
        >
          {tr("export")}
        </Button>
      </div>
      <div className="grid gap-4 lg:grid-cols-[200px_1fr]">
        <aside className="space-y-2">
          <label className="block text-sm" htmlFor="note-search">
            {tr("search")}
          </label>
          <input
            id="note-search"
            type="search"
            className={field}
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
          <ul className="max-h-96 space-y-1 overflow-y-auto">
            {notes.map((item) => (
              <li key={item.id}>
                <button
                  className={`w-full rounded-lg border p-2 text-left ${item.id === note?.id ? "border-logo-primary bg-wash" : "border-line"}`}
                  aria-current={item.id === note?.id ? "true" : undefined}
                  disabled={busy || saving}
                  onClick={() => void select(item.id)}
                >
                  <span className="block truncate font-medium">
                    {item.title || tr("untitled")}
                  </span>
                  <span className="block truncate text-xs text-text/70">
                    {item.preview}
                  </span>
                </button>
              </li>
            ))}
          </ul>
          {!notes.length && (
            <p className="text-sm text-text/70">{tr("empty")}</p>
          )}
        </aside>
        <div className="space-y-3">
          <label className="block text-sm" htmlFor="note-title">
            {tr("noteTitle")}
          </label>
          <input
            id="note-title"
            className={field}
            disabled={busy}
            maxLength={160}
            value={draft.title}
            onChange={(event) => edit("title", event.target.value)}
          />
          <label className="block text-sm" htmlFor="note-content">
            {tr("content")}
          </label>
          <textarea
            id="note-content"
            ref={editorRef}
            disabled={busy}
            className={`${field} min-h-72 resize-y`}
            value={draft.content}
            onChange={(event) => edit("content", event.target.value)}
            spellCheck
          />
          <div className="flex flex-wrap items-center gap-2">
            <span role="status" className="text-xs text-text/70">
              {saving
                ? tr("saving")
                : dirty
                  ? tr("unsaved")
                  : note
                    ? tr("saved", { revision: note.revision })
                    : tr("newDraft")}
            </span>
            <Button
              variant="secondary"
              size="sm"
              disabled={saving || !dirty}
              onClick={() => {
                setBlocked(false);
                void save();
              }}
            >
              {tr("save")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={busy || !note}
              onClick={() => void showHistory()}
            >
              {tr("history")}
            </Button>
            <Button
              variant="danger-ghost"
              size="sm"
              disabled={busy || !note}
              onClick={() => setDeleting(true)}
            >
              {tr("delete")}
            </Button>
          </div>
          {deleting && (
            <div className="rounded-lg border border-error p-3 space-y-2">
              <p>{tr("deleteConfirm")}</p>
              <Button
                variant="danger"
                onClick={() =>
                  run(async () => {
                    if (!(await flush()) || !noteRef.current) return;
                    await invoke("workspace_delete_note", {
                      noteId: noteRef.current.id,
                      expectedRevision: noteRef.current.revision,
                    });
                    install(null);
                    await refresh();
                  })
                }
                disabled={busy}
              >
                {tr("delete")}
              </Button>
              <Button variant="ghost" onClick={() => setDeleting(false)}>
                {tr("cancel")}
              </Button>
            </div>
          )}
          {versions && (
            <section
              className="rounded-lg border border-line p-3 space-y-2"
              aria-label={tr("history")}
            >
              <h3 className="font-medium">{tr("history")}</h3>
              <p className="text-xs text-text/70">{tr("historyLimit")}</p>
              <div className="flex flex-wrap gap-2">
                {versions.map((item) => (
                  <Button
                    key={item.revision}
                    variant="secondary"
                    size="sm"
                    onClick={() => setVersion(item)}
                  >
                    {tr("revision", { revision: item.revision })}
                  </Button>
                ))}
              </div>
              {version && (
                <>
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-wash p-2 text-sm">
                    {version.title}
                    {"\n\n"}
                    {version.content}
                  </pre>
                  <Button
                    disabled={busy}
                    onClick={() =>
                      run(async () => {
                        if (!(await flush()) || !noteRef.current) return;
                        install(
                          await invoke<Note>("workspace_restore_note", {
                            noteId: noteRef.current.id,
                            revision: version.revision,
                            expectedRevision: noteRef.current.revision,
                          }),
                        );
                        await refresh();
                      })
                    }
                  >
                    {tr("restore")}
                  </Button>
                </>
              )}
            </section>
          )}
          <section
            className="rounded-lg border border-line p-3 space-y-2"
            aria-label={tr("transform")}
          >
            <h3 className="font-medium">{tr("transform")}</h3>
            <p className="text-xs text-text/70">{tr("providerHelp")}</p>
            <label className="block text-sm" htmlFor="note-prompt">
              {tr("savedPrompt")}
            </label>
            <select
              id="note-prompt"
              className={field}
              value={promptId}
              onChange={(event) => setPromptId(event.target.value)}
            >
              <option value="">{tr("customPrompt")}</option>
              {getSetting("post_process_prompts")?.map((prompt) => (
                <option key={prompt.id} value={prompt.id}>
                  {prompt.name}
                </option>
              ))}
            </select>
            <label className="block text-sm" htmlFor="note-instruction">
              {tr("instruction")}
            </label>
            <textarea
              id="note-instruction"
              className={field}
              maxLength={4000}
              value={instruction}
              onChange={(event) => setInstruction(event.target.value)}
            />
            <Button
              disabled={
                busy || (!promptId && !instruction.trim()) || (!note && !dirty)
              }
              onClick={() => void transform()}
            >
              {busy ? tr("working") : tr("preview")}
            </Button>
            {generated && (
              <div className="space-y-2">
                <h4>{tr("preview")}</h4>
                <pre className="max-h-80 overflow-auto whitespace-pre-wrap rounded bg-wash p-2 text-sm">
                  {generated.content}
                </pre>
                <Button
                  disabled={
                    busy ||
                    dirty ||
                    generated.based_on_revision !== note?.revision
                  }
                  onClick={() => void apply()}
                >
                  {tr("apply")}
                </Button>
                <Button variant="ghost" onClick={() => setGenerated(null)}>
                  {tr("discard")}
                </Button>
              </div>
            )}
          </section>
          <section
            className="rounded-lg border border-line p-3 space-y-2"
            aria-label={tr("chat")}
          >
            <h3 className="font-medium">{tr("chat")}</h3>
            <p className="text-xs text-text/70">{tr("chatHelp")}</p>
            <ol className="max-h-80 space-y-3 overflow-auto" aria-live="polite">
              {messages.map((message) => (
                <li key={message.id} className="rounded bg-wash p-2">
                  <p className="text-xs text-text/70">
                    {message.role === "user" ? tr("you") : tr("assistant")} ·{" "}
                    {tr("revision", { revision: message.note_revision })}
                  </p>
                  <p className="whitespace-pre-wrap text-sm">
                    {message.content}
                  </p>
                </li>
              ))}
            </ol>
            <label className="block text-sm" htmlFor="note-question">
              {tr("question")}
            </label>
            <textarea
              id="note-question"
              className={field}
              maxLength={4000}
              value={question}
              onChange={(event) => setQuestion(event.target.value)}
            />
            <Button
              disabled={busy || !question.trim() || (!note && !dirty)}
              onClick={() => void ask()}
            >
              {busy ? tr("working") : tr("ask")}
            </Button>
          </section>
        </div>
      </div>
    </section>
  );
}
