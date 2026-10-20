import { describe, expect, it } from "vitest";
import { autoRetryLine } from "./autoRetry";
import { visualState } from "./smartQueue";
import type { Download } from "./types";

const base = { id: 1, status: "failed", autoRetry: null } as unknown as Download;
const waiting = (atMs: number | null, waitingForNetwork = false): Download => ({ ...base, autoRetry: { atMs, waitingForNetwork, tries: 1, maxTries: 20 } });

describe("a failed download tried again on its own", () => {
  it("counts down to the next try", () => {
    expect(autoRetryLine(waiting(31_000), 1_000)).toEqual({ key: "card.autoRetryIn", vars: { s: "30 s" } });
    expect(autoRetryLine(waiting(121_000), 1_000)).toEqual({ key: "card.autoRetryIn", vars: { s: "2 min" } });
    expect(autoRetryLine(waiting(1_200), 1_000)).toEqual({ key: "card.autoRetryNow" });
  });

  it("waits for the network without a time", () => {
    expect(autoRetryLine(waiting(null, true), 0)).toEqual({ key: "card.waitingNetwork" });
    expect(autoRetryLine(waiting(5_000, true), 0)).toEqual({ key: "card.waitingNetwork" });
  });

  it("says nothing when it is not waiting, or not failed", () => {
    expect(autoRetryLine(base, 0)).toBeNull();
    expect(autoRetryLine({ ...waiting(5_000), status: "paused" }, 0)).toBeNull();
  });

  it("shows as retrying in the queue, not as given up", () => {
    expect(visualState(waiting(5_000))).toBe("retrying");
    expect(visualState(base)).toBe("failed");
  });
});
