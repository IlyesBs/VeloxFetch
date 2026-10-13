// A download's diagnostics timeline in the user's language. Rust keeps each line as English text (it is saved, and
// copied into support reports as it is); here the known lines are recognised and said again with the locale's words.
// A line not recognised is shown as it was written.

import { localError } from "./errors";
import { getLang, t, type Key } from "./i18n";

const camel = (s: string) => s.charAt(0).toLowerCase() + s.slice(1);

/** A Doctor word (`Link`, `NotRetryable`, `Blocking`) through its locale key, or the word itself. */
function word(prefix: string, value: string): string {
  const key = `${prefix}.${camel(value)}` as Key;
  const text = t(key);
  return text === key ? value : text;
}

/** The engine's error text said by its code when one can be read from it (`HTTP status 503`). */
function why(raw: string): string {
  const http = /HTTP status (\d{3})/.exec(raw);
  return http ? localError(`http_${http[1]}`, raw) : raw;
}

/** Why a saved part could not be continued (`downloader-core` `check_resumable`). */
const REASONS: [RegExp, Key][] = [
  [/^already completed$/, "tl.r.done"],
  [/^download is not resumable/, "tl.r.sequential"],
  [/^server no longer supports verified range requests$/, "tl.r.ranges"],
  [/^size changed:/, "tl.r.size"],
  [/^remote resource changed or cannot be validated/, "tl.r.changed"],
  [/^\.part size /, "tl.r.partSize"],
  [/^\.part file missing$/, "tl.r.partMissing"],
  [/^stored segment state is inconsistent/, "tl.r.inconsistent"],
];

/** Why a new link was not used (`download-manager` `relink_report`). */
const LINK_REASONS: Record<string, Key> = {
  "it is the link the download already uses": "tl.l.same",
  "it did not answer or refused": "tl.l.unreachable",
  "it gives a web page, not the file": "tl.l.webPage",
  "it gives a file of another size": "tl.l.size",
  "the server describes another version": "tl.l.version",
  "its bytes differ from the saved ones": "tl.l.bytes",
  "it cannot send the missing parts on their own": "tl.l.ranges",
  "it gives no way to notice a change during the download": "tl.l.validator",
  "nothing proves it is the same file": "tl.l.unproven",
  "it is likely, not proven, to be the same file": "tl.l.likely",
  "it was not needed": "tl.l.notNeeded",
};

function linkReason(raw: string): string {
  const m = /^(.*?)( \(HTTP \d{3}\))?$/.exec(raw)!;
  const key = LINK_REASONS[m[1]!];
  return key ? t(key) + (m[2] ?? "") : raw;
}

function reason(raw: string): string {
  const hit = REASONS.find(([re]) => re.test(raw));
  return hit ? t(hit[1]) : raw;
}

type Rule = [RegExp, (m: RegExpExecArray) => string];

const RULES: Rule[] = [
  [/^(Started again|Started|Resumed|Retried) \(attempt (\d+)\)$/, (m) => t(({ "Started again": "tl.startedAgain", Started: "tl.started", Resumed: "tl.resumed", Retried: "tl.retried" } as const)[m[1] as "Started"], { n: m[2]! })],
  [/^Finished: (.+) received at (.+)$/, (m) => t("tl.finished", { size: m[1]!, speed: m[2]! })],
  [/^Paused \(progress kept\)$/, () => t("tl.paused")],
  [/^Cancelled$/, () => t("tl.cancelled")],
  [/^Downloading again from the start$/, () => t("tl.again")],
  [
    /^Doctor: (\S+) - problem with: (\w+) - retry: (\w+) - (\w+)$/,
    (m) => t("tl.doctor", { code: m[1]!, source: word("doc.source", m[2]!), retry: word("doc.retry", m[3]!), sev: word("doc.sev", m[4]!) }),
  ],
  [/^Failed \(([^)]+)\): (.*)$/, (m) => t("tl.failed", { code: m[1]!, why: localError(m[1], m[2]) })],
  [
    /^Server answered (\d+): (.+), (parallel download possible|one connection only \(no verified ranges\))$/,
    (m) =>
      t("tl.answered", {
        status: m[1]!,
        size: m[2] === "size unknown" ? t("tl.sizeUnknown") : m[2]!,
        mode: m[3]!.startsWith("parallel") ? t("tl.parallel") : t("tl.oneConnection"),
      }),
  ],
  [
    /^(?:Part (\d+)|Request) failed \(try (\d+)\): (.*)\. Trying again in (.+)$/,
    (m) => t(m[1] ? "tl.partRetry" : "tl.requestRetry", { part: m[1] ?? "", n: m[2]!, why: why(m[3]!), delay: m[4]! }),
  ],
  [/^The server is limiting connections: using (\d+) from now on$/, (m) => t("tl.throttled", { n: m[1]! })],
  [/^Continuing from (.+)$/, (m) => t("tl.continuing", { size: m[1]! })],
  [/^Cannot continue the saved part \((.*)\): starting over$/, (m) => t("tl.startingOver", { why: reason(m[1]!) })],
  [/^New transfer in (\d+) parts over several connections$/, (m) => t("tl.plannedParallel", { n: m[1]! })],
  [/^New transfer over one connection$/, () => t("tl.plannedOne")],
  [/^The name was taken meanwhile: saved as (.+)$/, (m) => t("tl.renamed", { name: m[1]! })],
  [/^The server is preparing the file \(it answers when it is ready\): waiting for it$/, () => t("tl.waiting")],
  [
    /^The saved part changed since it was last saved \(a crash or an outside change\): (\d+) finished parts checked again, all intact(?:; (\d+) older parts could not be checked)?$/,
    (m) => t("tl.reverifiedOk", { n: m[1]! }) + (m[2] ? ` ${t("tl.reverifiedOld", { n: m[2] })}` : ""),
  ],
  [
    /^The saved part changed since it was last saved: (\d+) of (\d+) finished parts no longer match their check and are downloaded again \((.+)\)$/,
    (m) => t("tl.reverifiedBad", { bad: m[1]!, n: m[2]!, size: m[3]! }),
  ],
  [/^New link: the download starts from it \(nothing was saved yet\)$/, () => t("tl.linkFresh")],
  [/^New link: the same file \(confirmed by the server and by the saved bytes\)\. Continuing from (\d+) bytes$/, (m) => t("tl.linkSame", { n: m[1]! })],
  [/^New link: likely the same file \(the saved bytes match; chosen by the user\)\. Continuing from (\d+) bytes$/, (m) => t("tl.linkLikely", { n: m[1]! })],
  [/^New link not used: (.*)\. The saved bytes are kept$/, (m) => t("tl.linkRefused", { why: linkReason(m[1]!) })],
];

/** One timeline line in the current language (English: as Rust wrote it). */
export function localEvent(text: string): string {
  if (getLang() === "en") return text;
  for (const [re, say] of RULES) {
    const m = re.exec(text);
    if (m) return say(m);
  }
  return text;
}
