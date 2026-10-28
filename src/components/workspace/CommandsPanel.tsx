import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { ShortcutInput } from "../settings/ShortcutInput";

interface CommandRecord {
  id: string;
  destination: string;
  original: string;
  instruction: string;
  source_transcript: string | null;
  result: string | null;
  status: string;
  error: string | null;
  created_at: number;
}

export default function CommandsPanel() {
  const { t } = useTranslation();
  const label = (key: string) => t(`workspace.commands.${key}`);
  const [records, setRecords] = useState<CommandRecord[]>([]);
  const [selected, setSelected] = useState<CommandRecord | null>(null);
  const [instruction, setInstruction] = useState("");
  const [busy, setBusy] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [error, setError] = useState("");
  const [automaticCommands, setAutomaticCommands] = useState(false);
  const [automationBusy, setAutomationBusy] = useState(true);
  const load = useCallback(async (preferredId?: string) => {
    const records = await invoke<CommandRecord[]>("workspace_list_commands");
    setRecords(records);
    setSelected(
      (current) =>
        records.find((record) => record.id === (preferredId ?? current?.id)) ??
        records[0] ??
        null,
    );
    if (
      preferredId &&
      sessionStorage.getItem("workspace-command-id") === preferredId
    ) {
      sessionStorage.removeItem("workspace-command-id");
    }
  }, []);
  useEffect(() => {
    void invoke<{ automatic_commands: boolean }>("workspace_get_automation")
      .then((settings) => setAutomaticCommands(settings.automatic_commands))
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setAutomationBusy(false));
  }, []);
  useEffect(() => {
    setInstruction(selected?.instruction ?? "");
  }, [selected?.id, selected?.instruction]);
  useEffect(() => {
    void load(
      sessionStorage.getItem("workspace-command-id") ?? undefined,
    ).catch((e: unknown) => setError(String(e)));
    const openSubscription = listen<string>(
      "workspace-open-commands",
      ({ payload }) => {
        void load(payload).catch((e: unknown) => setError(String(e)));
      },
    );
    const subscription = listen<CommandRecord>(
      "workspace-command-updated",
      ({ payload }) => {
        setSelected(payload);
        setInstruction(payload.instruction);
        void load().catch((e: unknown) => setError(String(e)));
      },
    );
    return () => {
      void subscription.then((unlisten) => unlisten());
      void openSubscription.then((unlisten) => unlisten());
    };
  }, [load]);
  const run = async (action: () => Promise<CommandRecord>) => {
    setError("");
    setBusy(true);
    try {
      const record = await action();
      setSelected(record);
      setInstruction(record.instruction);
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const capture = async () => {
    setCapturing(true);
    await run(() => invoke<CommandRecord>("workspace_capture_command"));
    setCapturing(false);
  };
  const saveAutomaticCommands = async (enabled: boolean) => {
    setAutomationBusy(true);
    setError("");
    try {
      const settings = await invoke<{ automatic_commands: boolean }>(
        "workspace_save_automation",
        { changes: { automatic_commands: enabled } },
      );
      setAutomaticCommands(settings.automatic_commands);
    } catch (e) {
      setError(String(e));
    } finally {
      setAutomationBusy(false);
    }
  };
  const cancellable =
    selected &&
    ["captured", "generating", "ready", "failed"].includes(selected.status);
  const canGenerate =
    selected && ["captured", "ready", "failed"].includes(selected.status);
  return (
    <div className="space-y-5 p-4">
      <div>
        <h2 className="text-lg font-semibold">{label("title")}</h2>
        <p className="mt-1 text-sm text-ink-2">{label("description")}</p>
      </div>
      <section className="space-y-2 rounded border border-line p-3">
        <label className="flex items-center gap-2 text-sm font-medium">
          <input
            type="checkbox"
            checked={automaticCommands}
            disabled={automationBusy}
            onChange={(event) =>
              void saveAutomaticCommands(event.target.checked)
            }
          />
          {label("automaticCommands")}
        </label>
        <p className="text-sm text-ink-2">{label("automaticHelp")}</p>
        <p className="text-sm text-ink-2">{label("automaticLimits")}</p>
      </section>
      <ShortcutInput shortcutId="voice_command" descriptionMode="inline" />
      <p className="text-sm text-ink-2">{label("voiceHelp")}</p>
      {error && (
        <p role="alert" className="text-error">
          {error}
        </p>
      )}
      <div className="flex items-center gap-3">
        <Button disabled={busy} onClick={() => void capture()}>
          {label("capture")}
        </Button>
        {capturing && (
          <p role="status" className="text-sm">
            {label("captureHelp")}
          </p>
        )}
      </div>
      {selected && (
        <section className="space-y-3 rounded border border-line p-4">
          <div className="flex justify-between gap-2">
            <h3 className="font-medium">{selected.destination}</h3>
            <span role="status" className="text-sm text-ink-2">
              {label(`status.${selected.status}`)}
            </span>
          </div>
          <label className="block space-y-1 text-sm">
            <span>{label("original")}</span>
            <textarea
              readOnly
              value={selected.original}
              rows={4}
              className="w-full rounded border border-line bg-background p-2"
            />
          </label>
          <Button
            variant="secondary"
            onClick={() => {
              void writeText(selected.original).catch((e: unknown) =>
                setError(String(e)),
              );
            }}
          >
            {label("copyOriginal")}
          </Button>
          <label className="block space-y-1 text-sm">
            <span>{label("instruction")}</span>
            <textarea
              value={instruction}
              onChange={(event) => setInstruction(event.target.value)}
              maxLength={16000}
              rows={2}
              disabled={busy || !canGenerate}
              className="w-full rounded border border-line bg-background p-2"
            />
          </label>
          {selected.source_transcript && (
            <label className="block space-y-1 text-sm">
              <span>{label("sourceTranscript")}</span>
              <textarea
                readOnly
                value={selected.source_transcript}
                rows={2}
                className="w-full rounded border border-line bg-background p-2"
              />
            </label>
          )}
          <div className="flex flex-wrap gap-2">
            <Button
              variant="secondary"
              disabled={!instruction}
              onClick={() => {
                void writeText(instruction).catch((e: unknown) =>
                  setError(String(e)),
                );
              }}
            >
              {label("copyInstruction")}
            </Button>
            <Button
              disabled={busy || !canGenerate || !instruction.trim()}
              onClick={() =>
                void run(() =>
                  invoke<CommandRecord>("workspace_run_command", {
                    id: selected.id,
                    instruction,
                  }),
                )
              }
            >
              {label("generate")}
            </Button>
            {cancellable && (
              <Button
                variant="secondary"
                onClick={() =>
                  void run(() =>
                    invoke<CommandRecord>("workspace_cancel_command", {
                      id: selected.id,
                    }),
                  )
                }
              >
                {label("cancel")}
              </Button>
            )}
          </div>
          {selected.error && (
            <p role="alert" className="text-error">
              {selected.error}
            </p>
          )}
          {selected.result && (
            <>
              <label className="block space-y-1 text-sm">
                <span>{label("preview")}</span>
                <textarea
                  readOnly
                  value={selected.result}
                  rows={6}
                  className="w-full rounded border border-line bg-background p-2"
                />
              </label>
              <p className="text-sm text-ink-2">{label("replaceHelp")}</p>
              <div className="flex gap-2">
                <Button
                  disabled={busy || selected.status !== "ready"}
                  onClick={() =>
                    void run(() =>
                      invoke<CommandRecord>("workspace_apply_command", {
                        id: selected.id,
                      }),
                    )
                  }
                >
                  {label("replace")}
                </Button>
                <Button
                  variant="secondary"
                  onClick={() => {
                    void writeText(selected.result ?? "").catch((e: unknown) =>
                      setError(String(e)),
                    );
                  }}
                >
                  {label("copy")}
                </Button>
              </div>
            </>
          )}
        </section>
      )}
      <section className="space-y-2">
        <h3 className="font-medium">{label("history")}</h3>
        {records.length === 0 && (
          <p className="text-sm text-ink-2">{label("empty")}</p>
        )}
        {records.map((record) => (
          <button
            key={record.id}
            disabled={busy}
            onClick={() => {
              setSelected(record);
              setInstruction(record.instruction);
              setError("");
            }}
            className={`block w-full rounded border p-3 text-left text-sm ${selected?.id === record.id ? "border-logo-primary" : "border-line"}`}
          >
            <span className="font-medium">{record.destination}</span>
            <span className="ml-2 text-ink-2">
              {label(`status.${record.status}`)}
            </span>
            <span className="mt-1 block truncate">
              {record.instruction || record.original}
            </span>
          </button>
        ))}
      </section>
    </div>
  );
}
