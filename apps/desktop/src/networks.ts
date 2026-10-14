import type { Key } from "./i18n";
import type { AdapterKind } from "./types";

const KIND: Record<Exclude<AdapterKind, "other">, Key> = {
  wifi: "multi.kind.wifi",
  ethernet: "multi.kind.ethernet",
  usbEthernet: "multi.kind.usbEthernet",
  phone: "multi.kind.phone",
  mobile: "multi.kind.mobile",
  bluetooth: "multi.kind.bluetooth",
};

/**
 * A network's plain name: what it is ("Wi-Fi", "Phone (USB)"), numbered when there are several of the same kind
 * ("Wi-Fi", "Wi-Fi 2"); the system's own name when its kind is not known. `all` is every network shown, in order.
 */
export function networkLabel(n: { name: string; kind?: AdapterKind | null }, all: { name: string; kind?: AdapterKind | null }[], t: (k: Key) => string): string {
  if (!n.kind || n.kind === "other") return n.name;
  const same = all.filter((x) => x.kind === n.kind);
  const base = t(KIND[n.kind]);
  if (same.length < 2) return base;
  const i = same.findIndex((x) => x.name === n.name);
  return i <= 0 ? base : `${base} ${i + 1}`;
}
