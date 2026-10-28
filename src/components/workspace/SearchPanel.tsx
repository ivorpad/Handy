import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";

type Source = {
  id: string;
  kind: string;
  title: string;
  text: string;
  updated_at: number;
};
type Answer = { answer: string; sources: Source[] };
export default function SearchPanel() {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [sources, setSources] = useState<Source[]>([]);
  const [answer, setAnswer] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [searched, setSearched] = useState(false);
  const request = useRef(0);
  useEffect(
    () => () => {
      request.current++;
    },
    [],
  );
  const search = async (ask: boolean) => {
    const id = ++request.current;
    setBusy(true);
    setError("");
    setAnswer("");
    setSources([]);
    try {
      if (ask) {
        const result = await invoke<Answer>("workspace_answer_search", {
          query,
        });
        if (id === request.current) {
          setAnswer(result.answer);
          setSources(result.sources);
        }
      } else {
        const result = await invoke<Source[]>("workspace_search", { query });
        if (id === request.current) setSources(result);
      }
      if (id === request.current) setSearched(true);
    } catch (e) {
      if (id === request.current) setError(String(e));
    } finally {
      if (id === request.current) setBusy(false);
    }
  };
  return (
    <section className="space-y-4 p-4">
      <h2 className="text-lg font-semibold">{t("workspace.search.title")}</h2>
      <p className="text-sm opacity-70">{t("workspace.search.description")}</p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void search(false);
        }}
        className="space-y-3"
      >
        <label className="block">
          {t("workspace.search.query")}
          <input
            className="block w-full rounded border bg-transparent p-2"
            maxLength={200}
            required
            value={query}
            onChange={(e) => {
              request.current++;
              setBusy(false);
              setQuery(e.target.value);
              setSearched(false);
              setSources([]);
              setAnswer("");
              setError("");
            }}
          />
        </label>
        <div className="flex gap-2">
          <Button type="submit" disabled={busy || !query.trim()}>
            {t("workspace.search.search")}
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={busy || !query.trim()}
            onClick={() => void search(true)}
          >
            {t("workspace.search.ask")}
          </Button>
        </div>
        <p className="text-sm opacity-70">{t("workspace.search.disclosure")}</p>
      </form>
      {busy && <p role="status">{t("workspace.search.working")}</p>}
      {error && (
        <p role="alert" className="text-red-500">
          {error}
        </p>
      )}
      {answer && (
        <article className="whitespace-pre-wrap rounded border p-3">
          {answer}
        </article>
      )}
      {searched && !sources.length && <p>{t("workspace.search.empty")}</p>}
      {sources.map((source) => (
        <details className="rounded border p-3" key={source.id}>
          <summary className="cursor-pointer">
            {source.title}{" "}
            <span className="text-sm opacity-70">{source.id}</span>
          </summary>
          <p className="whitespace-pre-wrap break-words pt-2">{source.text}</p>
        </details>
      ))}
    </section>
  );
}
