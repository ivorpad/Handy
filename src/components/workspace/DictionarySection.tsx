import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";

export interface Hint {
  id: string;
  word: string;
  wrong: string | null;
  revision: number;
  origin: string;
  reviewed: boolean;
  undo_token: string | null;
}

interface DictionarySectionProps {
  hints: Hint[];
  busy: boolean;
  run: (action: () => Promise<void>) => Promise<void>;
}

/** Dictionary words, and exact replacements for the ways a model hears them. */
export default function DictionarySection({
  hints,
  busy,
  run,
}: DictionarySectionProps) {
  const { t } = useTranslation();
  const label = (key: string) => t(`workspace.personalization.${key}`);
  const [hintDraft, setHintDraft] = useState<Hint | null>(null);
  const [word, setWord] = useState("");
  const [heardAs, setHeardAs] = useState("");
  const field = "w-full rounded border border-line bg-background p-2 text-sm";
  return (
    <section className="space-y-3">
      <h3 className="font-medium">{label("dictionary")}</h3>
      <p className="text-sm text-text/70">{label("dictionaryHelp")}</p>
      <p className="text-sm text-text/70">{label("dictionaryHeardAs")}</p>
      <form
        className="space-y-2"
        onSubmit={(event) => {
          event.preventDefault();
          void run(async () => {
            await invoke<void>("add_dictionary_word", {
              word,
              heardAs: heardAs
                .split(",")
                .map((spelling) => spelling.trim())
                .filter(Boolean),
            });
            setWord("");
            setHeardAs("");
          });
        }}
      >
        <label className="block">
          {label("dictionaryWord")}
          <input
            className={field}
            value={word}
            maxLength={120}
            required
            placeholder={label("dictionaryWordPlaceholder")}
            onChange={(event) => setWord(event.target.value)}
          />
        </label>
        <label className="block">
          {label("heardAs")}
          <input
            className={field}
            value={heardAs}
            placeholder={label("heardAsPlaceholder")}
            onChange={(event) => setHeardAs(event.target.value)}
          />
          <small className="block text-text/70">{label("heardAsHelp")}</small>
        </label>
        <Button disabled={busy} type="submit">
          {label("addWord")}
        </Button>
      </form>
      {hints.length === 0 && <p>{label("emptyDictionary")}</p>}
      {hintDraft && (
        <form
          className="space-y-2"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              await invoke<void>("update_learned_hint", {
                id: hintDraft.id,
                word: hintDraft.word,
                wrong: hintDraft.wrong,
                expectedRevision: hintDraft.revision,
              });
              setHintDraft(null);
            });
          }}
        >
          <label className="block">
            {label("word")}
            <input
              className={field}
              required
              maxLength={120}
              value={hintDraft.word}
              onChange={(event) =>
                setHintDraft({ ...hintDraft, word: event.target.value })
              }
            />
          </label>
          {hintDraft.wrong !== null && (
            <label className="block">
              {label("wrong")}
              <input
                className={field}
                required
                maxLength={120}
                value={hintDraft.wrong}
                onChange={(event) =>
                  setHintDraft({ ...hintDraft, wrong: event.target.value })
                }
              />
            </label>
          )}
          <Button disabled={busy} type="submit">
            {label("save")}
          </Button>
          <Button
            type="button"
            variant="ghost"
            onClick={() => setHintDraft(null)}
          >
            {label("cancel")}
          </Button>
        </form>
      )}
      {hints.map((hint) => (
        <div
          key={hint.id}
          className="flex items-center justify-between gap-2 rounded border border-line p-3"
        >
          <span>
            {hint.wrong ? `${hint.wrong} → ${hint.word}` : hint.word}
            {hint.origin === "background" && (
              <small className="block text-text/70">
                {label(
                  hint.reviewed ? "backgroundReviewed" : "backgroundPending",
                )}
              </small>
            )}
          </span>
          <div className="flex gap-2">
            {hint.origin === "background" && !hint.reviewed && (
              <Button
                variant="secondary"
                size="sm"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await invoke<void>("review_background_learning", {
                      id: hint.id,
                      expectedRevision: hint.revision,
                    });
                  })
                }
              >
                {label("reviewLearning")}
              </Button>
            )}
            {hint.origin === "background" && hint.undo_token && (
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await invoke<void>("undo_correction", {
                      undoToken: hint.undo_token,
                    });
                  })
                }
              >
                {label("undoLearning")}
              </Button>
            )}
            <Button
              variant="secondary"
              size="sm"
              disabled={busy}
              onClick={() => setHintDraft(hint)}
            >
              {label("edit")}
            </Button>
            <Button
              variant="danger-ghost"
              size="sm"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  await invoke<void>("delete_learned_hint", {
                    id: hint.id,
                    expectedRevision: hint.revision,
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
  );
}
