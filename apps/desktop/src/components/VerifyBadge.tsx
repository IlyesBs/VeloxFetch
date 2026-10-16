import { useT } from "../i18n";
import type { Verify } from "../types";
import { Icon } from "../ui/icons";

/** What the tooltip and the details say about a file's integrity check (feature #6). */
export function verifyText(t: ReturnType<typeof useT>, v: Verify): string {
  const algo = v.algo ?? "SHA-256";
  if (v.state === "looking") return t("verify.lookingTip");
  if (v.by === "publisher") {
    return t(v.state === "verified" ? "verify.publisherTip" : "verify.readyPublisherTip", { list: v.list ?? "", algo });
  }
  return t(v.state === "verified" ? "verify.youTip" : "verify.readyYouTip", { algo });
}

/**
 * The integrity badge of a download (card, Completed list, History): "Verified" in green once the file matched the
 * publisher's checksum (or the user's), "Will be verified" while it downloads with a checksum known, "Checking for a
 * checksum…" for the moment the publisher's list is looked for. Nothing when there is no checksum.
 */
export function VerifyBadge({ verify, className = "", narrow = false }: { verify?: Verify | null; className?: string; narrow?: boolean }) {
  const t = useT();
  if (!verify) return null;
  const tone =
    verify.state === "verified" ? "bg-ok/15 text-ok" : verify.state === "ready" ? "bg-accent-soft text-accent" : "bg-raised text-dim";
  const label = t(verify.state === "verified" ? "verify.verified" : verify.state === "ready" ? "verify.ready" : "verify.looking");
  return (
    <span
      className={`chip verify-badge min-w-0 max-w-full ${tone} ${className}`}
      title={verifyText(t, verify)}
      aria-label={narrow ? `${label}. ${verifyText(t, verify)}` : undefined}
      data-testid="verify-badge"
      data-state={verify.state}
      data-by={verify.by ?? undefined}
    >
      <Icon name="shield" size={13} className={`shrink-0 ${verify.state === "looking" ? "animate-pulse" : ""}`} />
      {/* `narrow`: inside a tight `@container` (a list row at the smallest window) only the shield shows; the words
          stay in its tooltip and its accessible name. */}
      <span className={narrow ? "hidden @[15rem]:inline" : "truncate"}>{label}</span>
    </span>
  );
}
