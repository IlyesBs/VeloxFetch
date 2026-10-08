import { formatBytes } from "../format";
import { useT } from "../i18n";
import { evidence, findingLine, HEALTH_CLASS, storageLine, strategyLine, warnings, type Line } from "../intelligence";
import type { Analysis } from "../types";
import { Icon } from "../ui/icons";

function Row({ label, children, testid }: { label: string; children: React.ReactNode; testid?: string }) {
  return (
    <div className="flex gap-3 py-1.5 text-[13px]">
      <dt className="w-24 shrink-0 text-dim">{label}</dt>
      <dd className="selectable min-w-0 flex-1 break-words" data-testid={testid}>
        {children}
      </dd>
    </div>
  );
}

/**
 * Download Intelligence: what the check before the download found, in a few rows, the warnings, and "Why?" (the
 * observations behind it). `basics` adds file, type and size (the confirmation dialog already shows those).
 */
export function IntelligencePanel({ a, basics = false }: { a: Analysis; basics?: boolean }) {
  const t = useT();
  const say = (l: Line) => t(l.key, l.vars);
  const warn = warnings(a);
  const kind = a.kind === "file" || a.kind === "unknown" ? null : t(`intel.kind.${a.kind}`);
  const dup = a.duplicate;
  return (
    <section className="mt-3 rounded-xl border border-line px-3 pb-2 pt-2.5" data-testid="intel" data-health={a.health} aria-labelledby="intel-title">
      <div className="flex items-center gap-2">
        <Icon name="shield" size={15} className="text-accent" />
        <h3 id="intel-title" className="flex-1 text-[13px] font-semibold">
          {t("intel.title")}
        </h3>
        <span className={`chip ${HEALTH_CLASS[a.health]}`} data-testid="intel-health">
          {t(`intel.health.${a.health}`)}
        </span>
      </div>
      <dl className="mt-1 divide-y divide-line">
        {basics && (
          <>
            <Row label={t("intel.file")} testid="intel-file">
              {a.filename}
            </Row>
            <Row label={t("intel.type")} testid="intel-type">
              {kind ?? a.contentType ?? t("intel.unknown")}
            </Row>
            <Row label={t("intel.size")} testid="intel-size">
              {a.size !== null ? formatBytes(a.size) : t("intel.unknown")}
            </Row>
            <Row label={t("intel.resume")} testid="intel-resume">
              {t(`intel.resume.${a.resume}`)}
            </Row>
          </>
        )}
        {!basics && kind && (
          <Row label={t("intel.type")} testid="intel-type">
            {kind}
          </Row>
        )}
        <Row label={t("intel.strategy")} testid="intel-strategy">
          {say(strategyLine(a))}
        </Row>
        <Row label={t("intel.storage")} testid="intel-storage">
          {say(storageLine(a))}
        </Row>
        <Row label={t("intel.security")} testid="intel-security">
          {a.https ? t("intel.secure") : t("intel.insecure")}
        </Row>
        {dup && (
          <Row label={t("intel.duplicate")} testid="intel-duplicate">
            {t(dup.id !== null ? "intel.dup.list" : "intel.dup.history", { name: dup.filename })} · {t(`intel.dup.${dup.reason}`)}
          </Row>
        )}
        <Row label={t("intel.warnings")} testid="intel-warnings">
          {warn.length === 0 ? (
            t("intel.none")
          ) : (
            <ul className="space-y-1">
              {warn.map((f) => (
                <li key={f.code} className={`flex gap-1.5 ${f.level === "problem" ? "text-bad" : "text-warn"}`} data-testid={`intel-finding-${f.code}`}>
                  <Icon name="alert" size={13} className="mt-0.5 shrink-0" />
                  <span>{say(findingLine(f))}</span>
                </li>
              ))}
            </ul>
          )}
        </Row>
      </dl>
      <details className="mt-1 text-xs" data-testid="intel-why">
        <summary className="cursor-pointer select-none text-dim hover:text-fg">{t("intel.why")}</summary>
        <ul className="mt-1.5 list-disc space-y-0.5 ps-5 text-dim">
          {evidence(a).map((l, i) => (
            <li key={i}>{say(l)}</li>
          ))}
        </ul>
      </details>
    </section>
  );
}
