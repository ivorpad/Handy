import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";

interface Scope {
  app_id: string | null;
  domain: string | null;
}
interface RecordBase extends Scope {
  id: string;
  enabled: boolean;
  revision: number;
  created_at: number;
  updated_at: number;
}
interface Profile extends RecordBase {
  name: string;
  prompt_id: string | null;
  cleanup_level: string;
  priority: number;
}
interface Preference extends RecordBase {
  text: string;
  language: string | null;
}
interface Destination {
  app_id: string;
  app_name: string;
  domain: string | null;
  selected_text: string | null;
  token: string;
  captured_at: number;
  context_available: boolean;
  unavailable_reason: string | null;
}
const base = (): RecordBase => ({
  id: "",
  app_id: null,
  domain: null,
  enabled: true,
  revision: 0,
  created_at: 0,
  updated_at: 0,
});
const newProfile = (): Profile => ({
  ...base(),
  name: "",
  prompt_id: null,
  cleanup_level: "inherit",
  priority: 0,
});
const newPreference = (): Preference => ({
  ...base(),
  text: "",
  language: null,
});
const inputClass =
  "w-full rounded border border-mid-gray/30 bg-transparent p-2";
const buttonClass =
  "rounded border border-mid-gray/30 px-3 py-2 disabled:opacity-50";

export default function ProfilesPanel() {
  const { t } = useTranslation();
  const tr = (key: string) => t(`workspace.profiles.${key}`);
  const { settings } = useSettings();
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [overrideId, setOverrideId] = useState<string | null>(null);
  const [preferences, setPreferences] = useState<Preference[]>([]);
  const [profile, setProfile] = useState<Profile>(newProfile);
  const [preference, setPreference] = useState<Preference>(newPreference);
  const [destination, setDestination] = useState<Destination | null>(null);
  const [match, setMatch] = useState<Profile | null>(null);
  const [includeText, setIncludeText] = useState(false);
  const [busy, setBusy] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [error, setError] = useState("");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const refresh = async () => {
    const [p, w, selected] = await Promise.all([
      invoke<Profile[]>("list_writing_profiles"),
      invoke<Preference[]>("list_writing_preferences"),
      invoke<string | null>("get_writing_profile_override"),
    ]);
    setProfiles(p);
    setPreferences(w);
    setOverrideId(selected);
    if (destination) {
      setMatch(
        await invoke<Profile | null>("resolve_writing_profile", {
          destination,
        }),
      );
    }
  };
  useEffect(() => {
    void refresh().catch((e) => setError(String(e)));
    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
  }, []);
  const run = async (work: () => Promise<void>) => {
    setBusy(true);
    setError("");
    try {
      await work();
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const capture = () => {
    setCapturing(true);
    setError("");
    // Let the person focus the destination before querying NSWorkspace.
    timer.current = setTimeout(() => {
      void (async () => {
        try {
          const d = await invoke<Destination>("capture_destination", {
            includeText,
          });
          setDestination(d);
          setMatch(
            await invoke<Profile | null>("resolve_writing_profile", {
              destination: d,
            }),
          );
        } catch (e) {
          setError(String(e));
        } finally {
          setCapturing(false);
        }
      })();
    }, 3000);
  };
  const scopeFields = (scope: Scope, update: (scope: Scope) => void) => (
    <>
      <label className="block space-y-1">
        <span>{tr("appId")}</span>
        <input
          className={inputClass}
          value={scope.app_id ?? ""}
          placeholder={tr("appPlaceholder")}
          onChange={(e) => update({ ...scope, app_id: e.target.value || null })}
        />
      </label>
      <label className="block space-y-1">
        <span>{tr("domain")}</span>
        <input
          className={inputClass}
          value={scope.domain ?? ""}
          placeholder="example.com"
          onChange={(e) => update({ ...scope, domain: e.target.value || null })}
        />
      </label>
      {destination && (
        <button
          type="button"
          className={buttonClass}
          onClick={() =>
            update({ app_id: destination.app_id, domain: destination.domain })
          }
        >
          {tr("useDestination")}
        </button>
      )}
    </>
  );
  return (
    <div className="space-y-6 p-4 text-sm">
      <div className="space-y-2">
        <h2 className="text-xl font-semibold">{tr("title")}</h2>
        <p>{tr("description")}</p>
      </div>
      {error && (
        <p
          role="alert"
          className="rounded border border-red-500 p-3 text-red-500"
        >
          {error}
        </p>
      )}
      <section className="space-y-3 rounded border border-mid-gray/30 p-4">
        <label className="block space-y-1">
          <span>{tr("selectionMode")}</span>
          <select
            className={inputClass}
            disabled={busy}
            value={overrideId ?? ""}
            onChange={(e) => {
              const profileId = e.target.value || null;
              void run(async () => {
                setOverrideId(
                  await invoke<string | null>("set_writing_profile_override", {
                    profileId,
                  }),
                );
              });
            }}
          >
            <option value="">{tr("automatic")}</option>
            {profiles
              .filter((p) => p.enabled)
              .map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
          </select>
        </label>
        <p>{tr("overrideHelp")}</p>
        <h3 className="font-semibold">{tr("destination")}</h3>
        <p>{tr("captureHelp")}</p>
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={includeText}
            onChange={(e) => setIncludeText(e.target.checked)}
          />
          {tr("includeText")}
        </label>
        <button className={buttonClass} disabled={capturing} onClick={capture}>
          {tr(capturing ? "switchNow" : "capture")}
        </button>
        {destination && (
          <div className="space-y-1" aria-live="polite">
            <p>
              {destination.app_name} <code>{destination.app_id}</code>
              {destination.domain && ` · ${destination.domain}`}
            </p>
            <p>
              {tr("matched")}: {match?.name ?? tr("noMatch")}
            </p>
            {destination.unavailable_reason && (
              <p>{destination.unavailable_reason}</p>
            )}
            {destination.selected_text && (
              <pre className="max-h-32 overflow-auto whitespace-pre-wrap rounded bg-mid-gray/10 p-2">
                {destination.selected_text}
              </pre>
            )}
          </div>
        )}
      </section>
      <section className="space-y-3 rounded border border-mid-gray/30 p-4">
        <div className="flex justify-between">
          <h3 className="font-semibold">{tr("profiles")}</h3>
          <button
            className={buttonClass}
            onClick={() => setProfile(newProfile())}
          >
            {tr("newProfile")}
          </button>
        </div>
        <ul className="space-y-1">
          {profiles.map((p) => (
            <li key={p.id}>
              <button className="underline" onClick={() => setProfile(p)}>
                {p.name}
              </button>{" "}
              <span className="opacity-60">
                {p.domain ?? p.app_id ?? tr("defaultScope")}
                {!p.enabled && ` · ${tr("disabled")}`}
              </span>
            </li>
          ))}
        </ul>
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              setProfile(
                await invoke<Profile>("save_writing_profile", { profile }),
              );
            });
          }}
        >
          <label className="block space-y-1">
            <span>{tr("name")}</span>
            <input
              required
              maxLength={160}
              className={inputClass}
              value={profile.name}
              onChange={(e) => setProfile({ ...profile, name: e.target.value })}
            />
          </label>
          {scopeFields(profile, (scope) =>
            setProfile({ ...profile, ...scope }),
          )}
          <label className="block space-y-1">
            <span>{tr("prompt")}</span>
            <select
              className={inputClass}
              value={profile.prompt_id ?? ""}
              onChange={(e) =>
                setProfile({ ...profile, prompt_id: e.target.value || null })
              }
            >
              <option value="">{tr("inheritPrompt")}</option>
              {settings?.post_process_prompts?.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </label>
          <label className="block space-y-1">
            <span>{tr("cleanup")}</span>
            <select
              className={inputClass}
              value={profile.cleanup_level}
              onChange={(e) =>
                setProfile({ ...profile, cleanup_level: e.target.value })
              }
            >
              {["inherit", "none", "light", "medium"].map((level) => (
                <option key={level} value={level}>
                  {tr(`cleanup_${level}`)}
                </option>
              ))}
            </select>
          </label>
          <label className="block space-y-1">
            <span>{tr("priority")}</span>
            <input
              type="number"
              min={-2147483648}
              max={2147483647}
              className={inputClass}
              value={profile.priority}
              onChange={(e) =>
                setProfile({ ...profile, priority: Number(e.target.value) })
              }
            />
          </label>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={profile.enabled}
              onChange={(e) =>
                setProfile({ ...profile, enabled: e.target.checked })
              }
            />
            {tr("enabled")}
          </label>
          <div className="flex gap-2">
            <button disabled={busy} className={buttonClass}>
              {tr("save")}
            </button>
            {profile.id && (
              <button
                type="button"
                disabled={busy}
                className={buttonClass}
                onClick={() =>
                  void run(async () => {
                    await invoke("delete_writing_profile", {
                      id: profile.id,
                      revision: profile.revision,
                    });
                    setProfile(newProfile());
                  })
                }
              >
                {tr("delete")}
              </button>
            )}
          </div>
        </form>
      </section>
      <section className="space-y-3 rounded border border-mid-gray/30 p-4">
        <div className="flex justify-between">
          <h3 className="font-semibold">{tr("preferences")}</h3>
          <button
            className={buttonClass}
            onClick={() => setPreference(newPreference())}
          >
            {tr("newPreference")}
          </button>
        </div>
        <p>{tr("preferenceHelp")}</p>
        <ul>
          {preferences.map((p) => (
            <li key={p.id}>
              <button
                className="max-w-full truncate text-left underline"
                onClick={() => setPreference(p)}
              >
                {p.text}
              </button>
            </li>
          ))}
        </ul>
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              setPreference(
                await invoke<Preference>("save_writing_preference", {
                  preference,
                }),
              );
            });
          }}
        >
          <label className="block space-y-1">
            <span>{tr("preferenceText")}</span>
            <textarea
              required
              maxLength={4000}
              className={inputClass}
              value={preference.text}
              onChange={(e) =>
                setPreference({ ...preference, text: e.target.value })
              }
            />
          </label>
          {scopeFields(preference, (scope) =>
            setPreference({ ...preference, ...scope }),
          )}
          <label className="block space-y-1">
            <span>{tr("language")}</span>
            <input
              className={inputClass}
              placeholder="en-GB"
              value={preference.language ?? ""}
              onChange={(e) =>
                setPreference({
                  ...preference,
                  language: e.target.value || null,
                })
              }
            />
          </label>
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={preference.enabled}
              onChange={(e) =>
                setPreference({ ...preference, enabled: e.target.checked })
              }
            />
            {tr("enabled")}
          </label>
          <div className="flex gap-2">
            <button className={buttonClass} disabled={busy}>
              {tr("save")}
            </button>
            {preference.id && (
              <button
                type="button"
                disabled={busy}
                className={buttonClass}
                onClick={() =>
                  void run(async () => {
                    await invoke("delete_writing_preference", {
                      id: preference.id,
                      revision: preference.revision,
                    });
                    setPreference(newPreference());
                  })
                }
              >
                {tr("delete")}
              </button>
            )}
          </div>
        </form>
      </section>
    </div>
  );
}
