import { t, useT, type Key } from "../i18n";
import type { FileKind } from "../types";
import { Icon, type IconName } from "../ui/icons";

const ICON: Record<FileKind, IconName> = {
  video: "film",
  audio: "music",
  image: "image",
  document: "doc",
  archive: "archive",
  program: "app",
  other: "file",
};

const KIND_KEY: Record<FileKind, Key> = {
  video: "kind.video",
  audio: "kind.audio",
  image: "kind.image",
  document: "kind.document",
  archive: "kind.archive",
  program: "kind.program",
  other: "kind.other",
};

/** The name of a kind of file, in the current language. */
export const kindLabel = (k: FileKind): string => t(KIND_KEY[k]);

export const KINDS: FileKind[] = ["video", "audio", "image", "document", "archive", "program", "other"];

/** Extensions VeloxFetch never opens (it shows them in their folder instead). Rust has the full rule. */
const RISKY = /\.(exe|msi|msix|bat|cmd|com|scr|ps1|vbs|js|lnk|url|hta|jar|sh|app|pkg|dmg|deb|rpm|apk|appimage|desktop|iso|img|html?|svg|xml)\.?\s*$/i;

/** Whether "Open" makes sense for this file (programs, scripts and web pages are only shown in their folder). */
export function canOpen(filename: string, kind: FileKind): boolean {
  return kind !== "program" && /\.[^.]+$/.test(filename) && !RISKY.test(filename);
}

/** A coloured tile with the icon of a kind of file. */
export function FileIcon({ kind, size = 40 }: { kind: FileKind; size?: number }) {
  useT();
  return (
    <div className={`kind-tile kind-${kind} grid shrink-0 place-items-center rounded-xl`} style={{ width: size, height: size }} title={kindLabel(kind)} data-kind={kind}>
      <Icon name={ICON[kind]} size={Math.round(size * 0.47)} />
    </div>
  );
}
