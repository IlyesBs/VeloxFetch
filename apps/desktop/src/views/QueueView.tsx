import { memo } from "react";
import { actions } from "../actions";
import { PrimaryAction } from "../components/DownloadCard";
import { QueueControls } from "../components/QueueControls";
import { hostOf } from "../format";
import { useDownload, useStore } from "../hooks";
import { PRIORITIES, type Priority } from "../types";
import { t as tr, useT, type Key } from "../i18n";
import { EmptyState, StatusChip } from "../ui/primitives";

const RANK: Record<Priority, number> = { lowest: 0, low: 1, normal: 2, high: 3, highest: 4 };

const QueueRow = memo(function QueueRow({ id, position, onOpen }: { id: number; position: number; onOpen: (id: number) => void }) {
  const d = useDownload(id);
  if (!d) return null;
  return (
    <li className="card card-hover flex items-center gap-3 p-3" data-testid={`queue-${d.id}`}>
      <span className="num grid h-7 w-7 shrink-0 place-items-center rounded-lg bg-accent-soft text-xs font-semibold text-accent">{position}</span>
      <div className="min-w-0 flex-1">
        <button className="block max-w-full truncate text-start text-[13px] font-semibold hover:underline" onClick={() => onOpen(d.id)}>
          {d.filename}
        </button>
        <p className="truncate text-xs text-dim">{hostOf(d.url)}</p>
      </div>
      <StatusChip status={d.status} />
      <select
        aria-label={tr("q.prioFor", { name: d.filename })}
        className="input h-8 w-28 py-0"
        value={d.priority}
        onChange={(e) => void actions.setPriority(d.id, e.target.value as Priority)}
      >
        {PRIORITIES.map((p) => (
          <option key={p} value={p}>
            {tr(`prio.${p}` as Key)}
          </option>
        ))}
      </select>
      <PrimaryAction d={d} compact />
    </li>
  );
});

/** What runs next: running first, then waiting downloads by priority, then paused ones. */
export function QueueView({ onOpen }: { onOpen: (id: number) => void }) {
  const t = useT();
  const groups = useStore((s) => s.groups);
  const byId = useStore((s) => s.byId);
  const waiting = [...groups.queued].sort((a, b) => RANK[byId[b]!.priority] - RANK[byId[a]!.priority] || a - b);
  const order = [...groups.active, ...waiting, ...groups.paused];

  return (
    <div className="space-y-4">
      <QueueControls />
      {order.length === 0 ? (
        <EmptyState icon="queue" title={t("queue.emptyTitle")} body={t("queue.emptyBody")} />
      ) : (
        <div className="space-y-3">
          <p className="text-dim">{t("q.how")}</p>
          <ol className="space-y-2" data-testid="queue-list">
            {order.map((id, i) => (
              <QueueRow key={id} id={id} position={i + 1} onOpen={onOpen} />
            ))}
          </ol>
        </div>
      )}
    </div>
  );
}
