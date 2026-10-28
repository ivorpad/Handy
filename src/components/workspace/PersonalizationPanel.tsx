import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import DictionarySection, { type Hint } from "./DictionarySection";

interface Snippet {
  id: string;
  trigger: string;
  expansion: string;
  enabled: boolean;
  revision: number;
}
interface AutomationSettings {
  background_learning: boolean;
  automatic_commands: boolean;
}
interface Entry {
  id: number;
  transcription_text: string;
  post_processed_text: string | null;
}
interface Proposal {
  id: string;
  before: string;
  after: string;
}
interface Receipt {
  hint: Hint;
  undo_token: string;
}

export default function PersonalizationPanel() {
  const { t } = useTranslation();
  const label = (key: string) => t(`workspace.personalization.${key}`);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [hints, setHints] = useState<Hint[]>([]);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [editing, setEditing] = useState<Snippet | null>(null);
  const [trigger, setTrigger] = useState("");
  const [expansion, setExpansion] = useState("");
  const [entryId, setEntryId] = useState("");
  const [edited, setEdited] = useState("");
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [undo, setUndo] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [automation, setAutomation] = useState<AutomationSettings | null>(null);
  const load = useCallback(async () => {
    const [s, h, history, automationSettings] = await Promise.all([
      invoke<Snippet[]>("list_snippets"),
      invoke<Hint[]>("list_learned_hints"),
      invoke<{ entries: Entry[] }>("get_history_entries", {
        cursor: null,
        limit: 50,
      }),
      invoke<AutomationSettings>("workspace_get_automation"),
    ]);
    setSnippets(s);
    setHints(h);
    setEntries(history.entries);
    setAutomation(automationSettings);
  }, []);
  useEffect(() => {
    void load().catch((e: unknown) => setError(String(e)));
    const subscription = listen("workspace-learning-changed", () => {
      void load().catch((e: unknown) => setError(String(e)));
    });
    return () => {
      void subscription.then((unlisten) => unlisten());
    };
  }, [load]);
  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const selected = entries.find((entry) => String(entry.id) === entryId);
  const field = "w-full rounded border border-line bg-background p-2 text-sm";
  return (
    <div className="space-y-6 p-4">
      <h2 className="text-lg font-semibold">{label("title")}</h2>
      {error && (
        <p role="alert" className="text-error">
          {error}
        </p>
      )}
      <DictionarySection hints={hints} busy={busy} run={run} />
      <section className="space-y-2 rounded border border-line p-3">
        <label className="flex gap-2">
          <input
            type="checkbox"
            checked={automation?.background_learning ?? false}
            disabled={busy || automation === null}
            onChange={(event) => {
              const background_learning = event.target.checked;
              void run(async () => {
                await invoke<AutomationSettings>("workspace_save_automation", {
                  changes: { background_learning },
                });
              });
            }}
          />
          {label("backgroundLearning")}
        </label>
        <p className="text-sm text-text/70">{label("backgroundHelp")}</p>
      </section>
      <section className="space-y-3">
        <h3 className="font-medium">{label("snippets")}</h3>
        <p className="text-sm text-text/70">{label("snippetHelp")}</p>
        <form
          className="space-y-2"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              await invoke<Snippet>("save_snippet", {
                id: editing?.id ?? null,
                trigger,
                expansion,
                enabled: editing?.enabled ?? true,
                expectedRevision: editing?.revision ?? null,
              });
              setEditing(null);
              setTrigger("");
              setExpansion("");
            });
          }}
        >
          <label className="block">
            {label("trigger")}
            <input
              className={field}
              value={trigger}
              maxLength={120}
              required
              onChange={(event) => setTrigger(event.target.value)}
            />
          </label>
          <label className="block">
            {label("expansion")}
            <textarea
              className={field}
              rows={3}
              value={expansion}
              maxLength={20000}
              required
              onChange={(event) => setExpansion(event.target.value)}
            />
          </label>
          <Button disabled={busy} type="submit">
            {label(editing ? "save" : "add")}
          </Button>
          {editing && (
            <Button
              variant="ghost"
              type="button"
              onClick={() => {
                setEditing(null);
                setTrigger("");
                setExpansion("");
              }}
            >
              {label("cancel")}
            </Button>
          )}
        </form>
        {snippets.length === 0 && <p>{label("emptySnippets")}</p>}
        {snippets.map((snippet) => (
          <div
            key={snippet.id}
            className="rounded border border-line p-3 space-y-2"
          >
            <label className="flex gap-2">
              <input
                type="checkbox"
                checked={snippet.enabled}
                disabled={busy}
                onChange={(event) => {
                  const enabled = event.target.checked;
                  void run(async () => {
                    await invoke<Snippet>("save_snippet", {
                      ...snippet,
                      enabled,
                      expectedRevision: snippet.revision,
                    });
                  });
                }}
              />
              <strong>{snippet.trigger}</strong>
            </label>
            <p className="whitespace-pre-wrap break-words text-sm">
              {snippet.expansion}
            </p>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="secondary"
                disabled={busy}
                onClick={() => {
                  setEditing(snippet);
                  setTrigger(snippet.trigger);
                  setExpansion(snippet.expansion);
                }}
              >
                {label("edit")}
              </Button>
              <Button
                size="sm"
                variant="danger-ghost"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await invoke<void>("delete_snippet", {
                      id: snippet.id,
                      expectedRevision: snippet.revision,
                    });
                  })
                }
              >
                {label("delete")}
              </Button>
            </div>
          </div>
        ))}
      </section>
      <section className="space-y-3">
        <h3 className="font-medium">{label("learning")}</h3>
        <p className="text-sm text-text/70">{label("learningHelp")}</p>
        <label className="block">
          {label("history")}
          <select
            className={field}
            value={entryId}
            onChange={(event) => {
              const id = event.target.value;
              setEntryId(id);
              const entry = entries.find((item) => String(item.id) === id);
              setEdited(
                entry?.post_processed_text ?? entry?.transcription_text ?? "",
              );
              setProposal(null);
            }}
          >
            <option value="">{label("chooseHistory")}</option>
            {entries.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {(entry.post_processed_text ?? entry.transcription_text).slice(
                  0,
                  100,
                )}
              </option>
            ))}
          </select>
        </label>
        {selected && (
          <>
            <label className="block">
              {label("original")}
              <textarea
                className={field}
                readOnly
                rows={3}
                value={
                  selected.post_processed_text ?? selected.transcription_text
                }
              />
            </label>
            <label className="block">
              {label("edited")}
              <textarea
                className={field}
                rows={3}
                value={edited}
                onChange={(event) => {
                  setEdited(event.target.value);
                  setProposal(null);
                }}
              />
            </label>
            <Button
              disabled={
                busy ||
                edited ===
                  (selected.post_processed_text ?? selected.transcription_text)
              }
              onClick={() =>
                void run(async () => {
                  setProposal(
                    await invoke<Proposal>("propose_correction", {
                      historyId: selected.id,
                      editedText: edited,
                    }),
                  );
                })
              }
            >
              {label("review")}
            </Button>
          </>
        )}
        {proposal && (
          <div className="rounded border border-line p-3 space-y-2">
            <p>{label("candidate")}</p>
            <p className="whitespace-pre-wrap">
              {proposal.before} → {proposal.after}
            </p>
            <div className="flex gap-2">
              {[false, true].map((replacement) => (
                <Button
                  key={String(replacement)}
                  disabled={busy}
                  variant="secondary"
                  onClick={() =>
                    void run(async () => {
                      const receipt = await invoke<Receipt>(
                        "confirm_correction",
                        { proposalId: proposal.id, replacement },
                      );
                      setUndo(receipt.undo_token);
                      setProposal(null);
                    })
                  }
                >
                  {label(replacement ? "confirmReplacement" : "confirmWord")}
                </Button>
              ))}
            </div>
          </div>
        )}
        {undo && (
          <Button
            disabled={busy}
            variant="ghost"
            onClick={() =>
              void run(async () => {
                await invoke<void>("undo_correction", { undoToken: undo });
                setUndo(null);
              })
            }
          >
            {label("undo")}
          </Button>
        )}
      </section>
    </div>
  );
}
