import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import PersonalizationPanel from "./PersonalizationPanel";
import ProfilesPanel from "./ProfilesPanel";
import CommandsPanel from "./CommandsPanel";
import NotesPanel from "./NotesPanel";
import MeetingsPanel from "./MeetingsPanel";
import SearchPanel from "./SearchPanel";
import ConnectionsPanel from "./ConnectionsPanel";

const panels = {
  personalization: PersonalizationPanel,
  profiles: ProfilesPanel,
  commands: CommandsPanel,
  notes: NotesPanel,
  meetings: MeetingsPanel,
  search: SearchPanel,
  connectors: ConnectionsPanel,
};
type Section = keyof typeof panels;

export default function WorkspacePanel() {
  const { t } = useTranslation();
  const [section, setSection] = useState<Section>(() => {
    const stored = sessionStorage.getItem("workspace-section");
    return stored && Object.prototype.hasOwnProperty.call(panels, stored)
      ? (stored as Section)
      : "personalization";
  });
  useEffect(() => {
    const unlisten = listen("workspace-open-commands", () =>
      setSection("commands"),
    );
    return () => {
      void unlisten.then((dispose) => dispose());
    };
  }, []);
  const Panel = panels[section];
  return (
    <section className="space-y-4">
      <h1 className="text-xl font-semibold">{t("workspace.shell.title")}</h1>
      <nav
        aria-label={t("workspace.shell.navigation")}
        className="flex flex-wrap gap-2 border-b border-line pb-3"
      >
        {(Object.keys(panels) as Section[]).map((key) => (
          <button
            key={key}
            type="button"
            aria-current={section === key ? "page" : undefined}
            className={`rounded px-3 py-2 text-sm ${section === key ? "bg-logo-primary/80" : "hover:bg-line"}`}
            onClick={() => {
              sessionStorage.setItem("workspace-section", key);
              setSection(key);
            }}
          >
            {t(`workspace.shell.${key}`)}
          </button>
        ))}
      </nav>
      <Panel />
    </section>
  );
}
