import { useEffect, useState } from "react";
import { api, onClipboardLinks } from "../api";
import { hostOf, opensVideoDialog } from "../format";
import { act } from "../hooks";
import { useT } from "../i18n";
import { Icon } from "../ui/icons";
import { Rich } from "../ui/rich";

function nameOf(url: string): string {
  try {
    const last = new URL(url).pathname.split("/").filter(Boolean).pop();
    return last ? decodeURIComponent(last) : url;
  } catch {
    return url;
  }
}

/**
 * "You copied a download link": shown when clipboard detection (Settings) is on and the copied text is
 * one or more download links. Nothing happens until the user presses a button: one link goes to the
 * usual confirmation (details, Start / Cancel); several go to the batch dialog.
 */
export function ClipboardBanner({ onReviewMany, onStream }: { onReviewMany: (links: string[]) => void; onStream: (url: string) => void }) {
  const t = useT();
  const [links, setLinks] = useState<string[] | null>(null);

  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    onClipboardLinks((l) => setLinks(l)).then((u) => (cancelled ? u() : (off = u)));
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  if (!links || links.length === 0) return null;
  const single = links.length === 1 ? links[0]! : null;

  const review = async () => {
    if (single && opensVideoDialog(single)) {
      onStream(single); // a video stream: choose quality, audio and subtitles first
    } else if (single) {
      const r = await act(() => api.reviewLink(single));
      if (r === undefined) return; // an error toast was shown; keep the banner
    } else {
      onReviewMany(links);
    }
    setLinks(null);
  };

  return (
    <div className="border-b border-line bg-accent-soft" role="status" data-testid="clipboard-banner">
      <div className="mx-auto flex max-w-5xl items-center gap-3 px-5 py-2.5">
        <Icon name="download" size={16} className="shrink-0 text-accent" />
        <p className="min-w-0 flex-1 truncate text-[13px]" data-testid="clipboard-text">
          {single ? (
            <>
              <Rich text={t("clipb.one", { name: nameOf(single), host: hostOf(single) })} />
            </>
          ) : (
            <>
              <Rich text={t("clipb.many", { n: links.length })} />
            </>
          )}
        </p>
        <button className="btn btn-primary h-8" data-testid="clipboard-review" onClick={() => void review()}>
          {single ? t("clipb.download") : t("clipb.review")}
        </button>
        <button className="btn btn-ghost h-8" data-testid="clipboard-dismiss" onClick={() => setLinks(null)}>
          {t("clipb.dismiss")}
        </button>
      </div>
    </div>
  );
}
