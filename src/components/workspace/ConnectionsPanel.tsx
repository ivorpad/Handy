import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";

type Connector = {
  id: string;
  name: string;
  kind: string;
  source: string;
  status: string;
  last_synced_at: number | null;
  last_error: string | null;
  document_count: number;
};

export default function ConnectionsPanel() {
  const { t } = useTranslation();
  const [connections, setConnections] = useState<Connector[]>([]);
  const [name, setName] = useState("");
  const [kind, setKind] = useState("local_file");
  const [source, setSource] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(
    async () =>
      setConnections(await invoke<Connector[]>("workspace_list_connectors")),
    [],
  );
  useEffect(() => {
    void refresh().catch((e) => setError(String(e)));
  }, [refresh]);
  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      try {
        await refresh();
      } catch (e) {
        setError(String(e));
      }
      setBusy(false);
    }
  };
  const choose = async () => {
    try {
      const path = await open({
        directory: kind === "local_folder",
        multiple: false,
        filters:
          kind === "calendar"
            ? [
                {
                  name: t("workspace.connectors.calendarFiles"),
                  extensions: ["ics"],
                },
              ]
            : kind === "local_file"
              ? [
                  {
                    name: t("workspace.connectors.textFiles"),
                    extensions: ["txt", "md", "markdown"],
                  },
                ]
              : undefined,
      });
      if (typeof path === "string") setSource(path);
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <section className="space-y-4 p-4">
      <h2 className="text-lg font-semibold">
        {t("workspace.connectors.title")}
      </h2>
      <p className="text-sm opacity-70">
        {t("workspace.connectors.description")}
      </p>
      <form
        className="space-y-3"
        onSubmit={(event) => {
          event.preventDefault();
          void run(async () => {
            await invoke("workspace_save_connector", {
              input: { name, kind, source },
            });
            setName("");
            setSource("");
          });
        }}
      >
        <label className="block">
          {t("workspace.connectors.name")}
          <input
            className="block w-full rounded border bg-transparent p-2"
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={200}
            required
            disabled={busy}
          />
        </label>
        <label className="block">
          {t("workspace.connectors.type")}
          <select
            className="block w-full rounded border bg-transparent p-2"
            value={kind}
            onChange={(e) => {
              setKind(e.target.value);
              setSource("");
            }}
            disabled={busy}
          >
            {["local_file", "local_folder", "calendar"].map((value) => (
              <option key={value} value={value}>
                {t(`workspace.connectors.kinds.${value}`)}
              </option>
            ))}
          </select>
        </label>
        <label className="block">
          {t("workspace.connectors.path")}
          <input
            className="block w-full rounded border bg-transparent p-2"
            value={source}
            onChange={(e) => setSource(e.target.value)}
            required
            disabled={busy}
          />
        </label>
        <Button
          type="button"
          variant="outline"
          onClick={() => void choose()}
          disabled={busy}
        >
          {t("workspace.connectors.choose")}
        </Button>
        <p className="text-sm opacity-70">
          {t(
            kind === "calendar"
              ? "workspace.connectors.calendarLimits"
              : "workspace.connectors.localLimits",
          )}
        </p>
        <Button type="submit" disabled={busy}>
          {t("workspace.connectors.add")}
        </Button>
      </form>
      {busy && <p role="status">{t("workspace.connectors.working")}</p>}
      {error && (
        <p role="alert" className="text-red-500">
          {error}
        </p>
      )}
      {!connections.length && <p>{t("workspace.connectors.empty")}</p>}
      {connections.map((connection) => (
        <article className="space-y-2 rounded border p-3" key={connection.id}>
          <h3 className="font-semibold">{connection.name}</h3>
          <p className="break-all text-sm">{connection.source}</p>
          <p>
            {t(`workspace.connectors.status.${connection.status}`, {
              defaultValue: connection.status,
            })}{" "}
            ·{" "}
            {t("workspace.connectors.documents", {
              count: connection.document_count,
            })}
          </p>
          {connection.last_synced_at && (
            <p className="text-sm">
              {t("workspace.connectors.lastSync", {
                date: new Date(connection.last_synced_at).toLocaleString(),
              })}
            </p>
          )}
          {connection.last_error && (
            <p role="alert" className="text-red-500">
              {connection.last_error}
            </p>
          )}
          {connection.status === "error" && connection.document_count > 0 && (
            <p>{t("workspace.connectors.stale")}</p>
          )}
          {connection.status === "disabled" && (
            <p>{t("workspace.connectors.remoteDisabled")}</p>
          )}
          <div className="flex gap-2">
            <Button
              disabled={busy || connection.status === "disabled"}
              onClick={() =>
                void run(() =>
                  invoke("workspace_sync_connector", { id: connection.id }),
                )
              }
            >
              {t("workspace.connectors.sync")}
            </Button>
            <Button
              variant="danger-ghost"
              disabled={busy}
              onClick={() =>
                void run(() =>
                  invoke("workspace_disconnect_connector", {
                    id: connection.id,
                  }),
                )
              }
            >
              {t("workspace.connectors.disconnect")}
            </Button>
          </div>
        </article>
      ))}
    </section>
  );
}
