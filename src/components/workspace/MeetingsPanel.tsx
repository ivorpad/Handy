import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useTranslation } from "react-i18next";

interface Action {
  id: string;
  text: string;
  owner: string | null;
  due: string | null;
  confirmed: boolean;
  done: boolean;
}
interface Meeting {
  id: string;
  title: string;
  status: string;
  created_at: number;
  transcript: string;
  summary: string;
  actions: Action[];
  error: string | null;
  sources: string[];
  revision: number;
  timeline: Timeline;
}
interface Segment {
  id: string;
  start_ms: number;
  end_ms: number;
  speaker_id: string | null;
  model_speaker_ids: string[];
  text: string;
  confidence: number | null;
  overlap: boolean;
  corrected: boolean;
}
interface Participant {
  id: string;
  label: string;
}
interface Timeline {
  segments: Segment[];
  participants: Participant[];
  alignment: string;
  max_speakers: number | null;
  model: string | null;
  notes_stale: boolean;
}
interface SpeakerModel {
  path: string | null;
  available: boolean;
  download_bytes: number;
  download_url: string;
}
const at = (milliseconds: number) =>
  new Date(milliseconds).toISOString().slice(11, 23);
interface Capabilities {
  microphone: boolean;
  system_audio: boolean;
  reason: string | null;
}

export default function MeetingsPanel() {
  const { t } = useTranslation();
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [speakerModel, setSpeakerModel] = useState<SpeakerModel | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [newParticipant, setNewParticipant] = useState("");
  const [title, setTitle] = useState("");
  const [microphone, setMicrophone] = useState(true);
  const [systemAudio, setSystemAudio] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const meeting = meetings.find((item) => item.id === selected);
  const recording = meetings.find((item) => item.status === "recording");
  const button =
    "rounded border border-mid-gray/30 px-3 py-2 disabled:opacity-40";
  const refresh = async () => {
    const items = await invoke<Meeting[]>("meeting_list");
    setMeetings(items);
  };
  useEffect(() => {
    void Promise.all([
      invoke<Capabilities>("meeting_capabilities").then((value) => {
        setCapabilities(value);
        setMicrophone(value.microphone);
      }),
      refresh(),
      invoke<SpeakerModel>("meeting_speaker_model").then(setSpeakerModel),
    ]).catch((reason: unknown) => setError(String(reason)));
  }, []);
  useEffect(() => {
    if (!recording) return;
    const timer = window.setInterval(() => {
      void refresh().catch((reason: unknown) => setError(String(reason)));
    }, 2000);
    return () => window.clearInterval(timer);
  }, [recording?.id]);

  const run = async (command: string, args?: Record<string, unknown>) => {
    setBusy(true);
    setError("");
    setCopied(false);
    try {
      const result = await invoke<Meeting>(command, args);
      setSelected(result.id);
      return true;
    } catch (reason) {
      setError(String(reason));
      return false;
    } finally {
      try {
        await refresh();
      } catch (reason) {
        setError(String(reason));
      }
      setBusy(false);
    }
  };
  const importRecording = async () => {
    try {
      const path = await open({
        multiple: false,
        filters: [{ name: "WAV", extensions: ["wav"] }],
      });
      if (typeof path === "string") {
        await run("meeting_import", {
          path,
          title: title.trim() || t("workspace.meetings.importedTitle"),
        });
      }
    } catch (reason) {
      setError(String(reason));
    }
  };
  const configureModel = async (download: boolean) => {
    try {
      let path: string | null = null;
      if (!download) {
        const choice = await open({
          multiple: false,
          filters: [{ name: "GGUF", extensions: ["gguf"] }],
        });
        if (typeof choice !== "string") return;
        path = choice;
      }
      setBusy(true);
      setDownloading(download);
      setError("");
      const config = await invoke<SpeakerModel>(
        download
          ? "meeting_download_speaker_model"
          : "meeting_set_speaker_model",
        path ? { path } : {},
      );
      setSpeakerModel(config);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
      setDownloading(false);
    }
  };

  return (
    <section className="space-y-4 p-5">
      <h2 className="text-xl font-semibold">{t("workspace.meetings.title")}</h2>
      <p className="text-sm text-mid-gray">
        {t("workspace.meetings.description")}
      </p>
      {capabilities?.reason && <p role="status">{capabilities.reason}</p>}
      <div className="space-y-2 rounded border border-mid-gray/30 p-3">
        <h3 className="font-semibold">
          {t("workspace.meetings.speakerModel")}
        </h3>
        <p className="text-sm">{t("workspace.meetings.localSpeakers")}</p>
        <p role="status">
          {t(
            speakerModel?.available
              ? "workspace.meetings.modelReady"
              : "workspace.meetings.modelMissing",
          )}
        </p>
        <div className="flex flex-wrap gap-2">
          <button
            className={button}
            disabled={busy || !!recording}
            onClick={() => void configureModel(false)}
          >
            {t("workspace.meetings.selectModel")}
          </button>
          <button
            className={button}
            disabled={busy || !!recording}
            onClick={() => void configureModel(true)}
          >
            {t("workspace.meetings.downloadModel")}
          </button>
        </div>
        {downloading && (
          <p role="status">{t("workspace.meetings.downloadingModel")}</p>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-3">
        <input
          className="rounded border border-mid-gray/30 bg-transparent p-2"
          aria-label={t("workspace.meetings.meetingTitle")}
          placeholder={t("workspace.meetings.meetingTitle")}
          value={title}
          onChange={(event) => setTitle(event.target.value)}
          maxLength={300}
        />
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={microphone}
            disabled={!capabilities?.microphone || !!recording || busy}
            onChange={(event) => setMicrophone(event.target.checked)}
          />
          {t("workspace.meetings.microphone")}
        </label>
        <label className="flex items-center gap-2">
          <input
            type="checkbox"
            checked={systemAudio}
            disabled={!capabilities?.system_audio || !!recording || busy}
            onChange={(event) => setSystemAudio(event.target.checked)}
          />
          {t("workspace.meetings.system")}
        </label>
        <button
          className={button}
          disabled={
            busy ||
            !!recording ||
            !title.trim() ||
            (!microphone && !systemAudio)
          }
          onClick={() =>
            void run("meeting_start", { title, microphone, systemAudio })
          }
        >
          {t("workspace.meetings.record")}
        </button>
        <button
          className={button}
          disabled={busy || !!recording}
          onClick={() => void importRecording()}
        >
          {t("workspace.meetings.import")}
        </button>
      </div>
      {recording && (
        <div
          role="status"
          className="flex items-center gap-3 rounded border border-red-500 p-3"
        >
          <span>
            {t("workspace.meetings.recording", { title: recording.title })}
          </span>
          <button
            className={button}
            disabled={busy}
            onClick={() => void run("meeting_stop", { id: recording.id })}
          >
            {t("workspace.meetings.stop")}
          </button>
        </div>
      )}
      {busy && !downloading && (
        <p role="status">{t("workspace.meetings.processing")}</p>
      )}
      {error && (
        <p role="alert" className="text-red-500">
          {error}
        </p>
      )}
      <div className="grid gap-4 md:grid-cols-[220px_1fr]">
        <div className="space-y-2">
          {meetings.length === 0 && <p>{t("workspace.meetings.empty")}</p>}
          {meetings.map((item) => (
            <button
              key={item.id}
              className={`${button} block w-full text-left ${selected === item.id ? "bg-mid-gray/10" : ""}`}
              onClick={() => {
                setSelected(item.id);
                setCopied(false);
              }}
            >
              <span className="block font-medium">{item.title}</span>
              <span className="text-xs">
                {t(`workspace.meetings.status.${item.status}`, {
                  defaultValue: item.status,
                })}
              </span>
            </button>
          ))}
        </div>
        {meeting && (
          <article className="min-w-0 space-y-4">
            <h3 className="text-lg font-semibold">{meeting.title}</h3>
            <p className="text-sm">
              {meeting.sources
                .map((source) =>
                  t(`workspace.meetings.source.${source}`, {
                    defaultValue: source,
                  }),
                )
                .join(", ")}
            </p>
            {meeting.error && (
              <p role="alert" className="text-red-500">
                {meeting.error}
              </p>
            )}
            <div className="flex flex-wrap gap-2">
              <button
                className={button}
                disabled={
                  busy ||
                  !!recording ||
                  meeting.actions.some((action) => action.confirmed)
                }
                onClick={() =>
                  void run("meeting_identify_speakers", { id: meeting.id })
                }
              >
                {t("workspace.meetings.identifySpeakers")}
              </button>
              <button
                className={button}
                disabled={
                  busy ||
                  !!recording ||
                  meeting.actions.some((action) => action.confirmed)
                }
                onClick={() =>
                  void run("meeting_transcribe", { id: meeting.id })
                }
              >
                {t("workspace.meetings.transcribe")}
              </button>
              <button
                className={button}
                disabled={
                  busy ||
                  !!recording ||
                  !meeting.transcript ||
                  meeting.actions.some((action) => action.confirmed)
                }
                onClick={() =>
                  void run("meeting_summarize", { id: meeting.id })
                }
              >
                {t("workspace.meetings.summarize")}
              </button>
              <button
                className={button}
                disabled={!meeting.transcript}
                onClick={() => {
                  void writeText(
                    [meeting.title, meeting.summary, meeting.transcript]
                      .filter(Boolean)
                      .join("\n\n"),
                  )
                    .then(() => setCopied(true))
                    .catch((reason: unknown) => setError(String(reason)));
                }}
              >
                {t("workspace.meetings.copy")}
              </button>
              {copied && (
                <span role="status">{t("workspace.meetings.copied")}</span>
              )}
            </div>
            {meeting.timeline?.segments.length > 0 && (
              <div className="space-y-3">
                <h4 className="font-semibold">
                  {t("workspace.meetings.timeline")}
                </h4>
                <p className="text-sm">
                  {t("workspace.meetings.timelineExplanation", {
                    count: meeting.timeline.max_speakers ?? undefined,
                  })}
                </p>
                <p className="text-xs">{meeting.timeline.model}</p>
                {meeting.timeline.notes_stale && (
                  <p role="status">{t("workspace.meetings.staleNotes")}</p>
                )}
                <h5 className="font-semibold">
                  {t("workspace.meetings.participants")}
                </h5>
                <div className="flex flex-wrap gap-2">
                  <input
                    className="rounded border border-mid-gray/30 bg-transparent p-2"
                    aria-label={t("workspace.meetings.newParticipant")}
                    placeholder={t("workspace.meetings.newParticipant")}
                    value={newParticipant}
                    maxLength={200}
                    disabled={busy || !!recording}
                    onChange={(event) => setNewParticipant(event.target.value)}
                  />
                  <button
                    className={button}
                    disabled={busy || !!recording || !newParticipant.trim()}
                    onClick={() =>
                      void run("meeting_add_participant", {
                        id: meeting.id,
                        label: newParticipant,
                        expectedRevision: meeting.revision,
                      }).then((saved) => {
                        if (saved) setNewParticipant("");
                      })
                    }
                  >
                    {t("workspace.meetings.addParticipant")}
                  </button>
                </div>
                {meeting.timeline.participants.map((participant) => (
                  <ParticipantEditor
                    key={participant.id}
                    participant={participant}
                    participants={meeting.timeline.participants}
                    disabled={busy || !!recording}
                    save={(label) =>
                      void run("meeting_rename_speaker", {
                        id: meeting.id,
                        speakerId: participant.id,
                        label,
                        expectedRevision: meeting.revision,
                      })
                    }
                    merge={(intoId) =>
                      void run("meeting_merge_speakers", {
                        id: meeting.id,
                        fromId: participant.id,
                        intoId,
                        expectedRevision: meeting.revision,
                      })
                    }
                  />
                ))}
                <p className="text-sm">
                  {t("workspace.meetings.overlapExplanation")}
                </p>
                {meeting.timeline.segments.map((segment) => (
                  <SegmentEditor
                    key={segment.id}
                    segment={segment}
                    participants={meeting.timeline.participants}
                    disabled={busy || !!recording}
                    save={(speakerId, text) =>
                      void run("meeting_correct_segment", {
                        id: meeting.id,
                        segmentId: segment.id,
                        speakerId,
                        text,
                        expectedRevision: meeting.revision,
                      })
                    }
                  />
                ))}
              </div>
            )}
            {meeting.summary && (
              <div>
                <h4 className="font-semibold">
                  {t("workspace.meetings.summary")}
                </h4>
                <p className="whitespace-pre-wrap">{meeting.summary}</p>
              </div>
            )}
            {meeting.actions.length > 0 && (
              <div className="space-y-2">
                <h4 className="font-semibold">
                  {t("workspace.meetings.actions")}
                </h4>
                <p className="text-sm">
                  {t("workspace.meetings.draftExplanation")}
                </p>
                {meeting.actions.map((action) => (
                  <div
                    key={action.id}
                    className="flex items-start gap-3 rounded border border-mid-gray/30 p-3"
                  >
                    <div className="flex-1">
                      <p>{action.text}</p>
                      {action.owner && (
                        <p className="text-sm">
                          {t("workspace.meetings.owner", {
                            owner: action.owner,
                          })}
                        </p>
                      )}
                      {action.due && (
                        <p className="text-sm">
                          {t("workspace.meetings.due", { due: action.due })}
                        </p>
                      )}
                    </div>
                    {!action.confirmed ? (
                      <button
                        className={button}
                        disabled={busy}
                        onClick={() =>
                          void run("meeting_confirm_action", {
                            id: meeting.id,
                            actionId: action.id,
                          })
                        }
                      >
                        {t("workspace.meetings.confirm")}
                      </button>
                    ) : (
                      <label className="flex items-center gap-2">
                        <input
                          type="checkbox"
                          checked={action.done}
                          disabled={busy}
                          onChange={(event) =>
                            void run("meeting_set_action_done", {
                              id: meeting.id,
                              actionId: action.id,
                              done: event.target.checked,
                            })
                          }
                        />
                        {t("workspace.meetings.done")}
                      </label>
                    )}
                  </div>
                ))}
              </div>
            )}
            {meeting.transcript && (
              <details open>
                <summary className="cursor-pointer font-semibold">
                  {t("workspace.meetings.transcript")}
                </summary>
                <p className="whitespace-pre-wrap pt-2">{meeting.transcript}</p>
              </details>
            )}
          </article>
        )}
      </div>
    </section>
  );
}

function ParticipantEditor({
  participant,
  participants,
  disabled,
  save,
  merge,
}: {
  participant: Participant;
  participants: Participant[];
  disabled: boolean;
  save: (label: string) => void;
  merge: (id: string) => void;
}) {
  const { t } = useTranslation();
  const [label, setLabel] = useState(participant.label);
  const [target, setTarget] = useState("");
  return (
    <div className="flex flex-wrap items-center gap-2 rounded border border-mid-gray/30 p-2">
      <span className="text-xs">{participant.id}</span>
      <input
        aria-label={t("workspace.meetings.participantName", {
          id: participant.id,
        })}
        className="rounded border border-mid-gray/30 bg-transparent p-2"
        value={label}
        maxLength={200}
        disabled={disabled}
        onChange={(event) => setLabel(event.target.value)}
      />
      <button
        className="rounded border border-mid-gray/30 p-2 disabled:opacity-40"
        disabled={disabled || !label.trim() || label === participant.label}
        onClick={() => save(label)}
      >
        {t("workspace.meetings.saveName")}
      </button>
      {participants.length > 1 && (
        <>
          <select
            className="rounded border border-mid-gray/30 bg-background p-2"
            aria-label={t("workspace.meetings.mergeTarget", {
              id: participant.id,
            })}
            value={target}
            disabled={disabled}
            onChange={(event) => setTarget(event.target.value)}
          >
            <option value="">
              {t("workspace.meetings.chooseMergeTarget")}
            </option>
            {participants
              .filter((item) => item.id !== participant.id)
              .map((item) => (
                <option key={item.id} value={item.id}>
                  {item.label}
                </option>
              ))}
          </select>
          <button
            className="rounded border border-mid-gray/30 p-2 disabled:opacity-40"
            disabled={disabled || !target}
            onClick={() => merge(target)}
          >
            {t("workspace.meetings.merge")}
          </button>
        </>
      )}
    </div>
  );
}

function SegmentEditor({
  segment,
  participants,
  disabled,
  save,
}: {
  segment: Segment;
  participants: Participant[];
  disabled: boolean;
  save: (speaker: string | null, text: string) => void;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState(segment.text);
  const [speaker, setSpeaker] = useState(segment.speaker_id ?? "");
  return (
    <div className="space-y-2 rounded border border-mid-gray/30 p-3">
      <p className="text-sm">
        {t("workspace.meetings.segmentTime", {
          start: at(segment.start_ms),
          end: at(segment.end_ms),
        })}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <select
          className="rounded border border-mid-gray/30 bg-background p-2"
          aria-label={t("workspace.meetings.segmentSpeaker", {
            time: at(segment.start_ms),
          })}
          value={speaker}
          disabled={disabled}
          onChange={(event) => setSpeaker(event.target.value)}
        >
          <option value="">{t("workspace.meetings.unassigned")}</option>
          {participants.map((participant) => (
            <option key={participant.id} value={participant.id}>
              {participant.label}
            </option>
          ))}
        </select>
        {segment.overlap && (
          <span>
            {t("workspace.meetings.sharedOverlap", {
              speakers: segment.model_speaker_ids
                .map(
                  (id) =>
                    participants.find((participant) => participant.id === id)
                      ?.label ?? id,
                )
                .join(", "),
            })}
          </span>
        )}
        {segment.corrected && <span>{t("workspace.meetings.corrected")}</span>}
        {segment.confidence !== null && (
          <span className="text-xs">
            {t("workspace.meetings.confidence", {
              confidence: Math.round(segment.confidence * 100),
            })}
          </span>
        )}
      </div>
      <textarea
        className="w-full rounded border border-mid-gray/30 bg-transparent p-2"
        aria-label={t("workspace.meetings.segmentText", {
          time: at(segment.start_ms),
        })}
        value={text}
        maxLength={20000}
        disabled={disabled}
        onChange={(event) => setText(event.target.value)}
      />
      <button
        className="rounded border border-mid-gray/30 p-2 disabled:opacity-40"
        disabled={
          disabled ||
          (text === segment.text && speaker === (segment.speaker_id ?? ""))
        }
        onClick={() => save(speaker || null, text)}
      >
        {t("workspace.meetings.saveCorrection")}
      </button>
    </div>
  );
}
