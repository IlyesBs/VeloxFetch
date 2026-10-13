import { describe, expect, it } from "vitest";
import { networkLabel } from "./networks";

const t = (k: string) => ({ "multi.kind.wifi": "Wi-Fi", "multi.kind.ethernet": "Ethernet", "multi.kind.phone": "Phone (USB)" })[k] ?? k;

describe("network names", () => {
  it("names a network by what it is, never by the system's identifier", () => {
    const all = [
      { name: "enp5s0", kind: "ethernet" as const },
      { name: "wlx00c0ca98ff70", kind: "wifi" as const },
      { name: "enx0ab80f3aab58", kind: "phone" as const },
    ];
    expect(all.map((n) => networkLabel(n, all, t))).toEqual(["Ethernet", "Wi-Fi", "Phone (USB)"]);
  });

  it("numbers several of the same kind, and keeps the system's name when the kind is unknown", () => {
    const all = [
      { name: "wlp0s20f3", kind: "wifi" as const },
      { name: "wlx00c0ca98ff70", kind: "wifi" as const },
      { name: "tailnet9", kind: "other" as const },
      { name: "old", kind: null },
    ];
    expect(all.map((n) => networkLabel(n, all, t))).toEqual(["Wi-Fi", "Wi-Fi 2", "tailnet9", "old"]);
  });
});
