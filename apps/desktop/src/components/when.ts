import { getLang } from "../i18n";

/** "2026-03-02T22:00" (local time from the backend) -> "Mon 22:00". */
export function formatWhen(local: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(local);
  if (!m) return local;
  const [, y, mo, d, h, mi] = m.map(Number) as [number, number, number, number, number, number];
  const date = new Date(y, mo - 1, d, h, mi);
  const day = date.toLocaleDateString(getLang(), { weekday: "short" }); // in the app's language
  return `${day} ${String(h).padStart(2, "0")}:${String(mi).padStart(2, "0")}`;
}

/** Whole seconds left until `endsAtMs`, never negative. */
export function secondsLeft(endsAtMs: number, nowMs: number): number {
  return Math.max(0, Math.ceil((endsAtMs - nowMs) / 1000));
}
