import { advice, factorLines, headline, retryLine, SEVERITY_CLASS, sourceLine, spaceLine, type Line } from "../doctor";
import { useT } from "../i18n";
import type { Diagnosis } from "../types";
import { Icon } from "../ui/icons";

/**
 * Download Doctor's diagnosis under a download's error: what happened, where the problem is, whether retrying helps
 * (and when), what else the evidence shows, and what to do. Buttons stay in the error card (Sign in, Retry...).
 */
export function DoctorPanel({ d, onNewLink }: { d: Diagnosis; onNewLink?: () => void }) {
  const t = useT();
  const say = (l: Line) => t(l.key, l.vars);
  const space = spaceLine(d);
  const factors = factorLines(d);
  const tips = advice(d);
  return (
    <div className="mt-2 space-y-1.5 rounded-lg border border-line px-3 py-2 text-xs" data-testid="doctor" data-category={d.category} data-retry={d.retry}>
      <div className="flex flex-wrap items-center gap-2">
        <Icon name="activity" size={13} className="text-accent" />
        <span className="font-semibold" data-testid="doctor-headline">
          {say(headline(d))}
        </span>
        <span className={`chip ${SEVERITY_CLASS[d.severity]}`} data-testid="doctor-severity">
          {t(`doc.sev.${d.severity}`)}
        </span>
      </div>
      <p data-testid="doctor-source">{say(sourceLine(d))}</p>
      {space && <p data-testid="doctor-space">{say(space)}</p>}
      <p className="text-dim" data-testid="doctor-retry">
        {say(retryLine(d, Date.now()))}
      </p>
      {factors.length > 0 && (
        <ul className="list-disc space-y-0.5 ps-5 text-dim" data-testid="doctor-factors">
          {factors.map((l, i) => (
            <li key={i}>{say(l)}</li>
          ))}
        </ul>
      )}
      {tips.length > 0 && (
        <ul className="space-y-0.5" data-testid="doctor-advice">
          {tips.map((l, i) => (
            <li key={i} className="flex gap-1.5">
              <Icon name="chevron" size={12} className="mt-0.5 shrink-0 text-accent" />
              <span>{say(l)}</span>
            </li>
          ))}
        </ul>
      )}
      {onNewLink && d.actions.includes("newLink") && (
        <button className="btn mt-1 h-7 px-2 text-xs" data-testid="doctor-new-link" onClick={onNewLink}>
          <Icon name="link" size={13} />
          {t("nl.menu")}
        </button>
      )}
    </div>
  );
}
