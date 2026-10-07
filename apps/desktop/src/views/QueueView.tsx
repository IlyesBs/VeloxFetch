import { useCallback, useMemo, useState } from "react";
import { QueueControls } from "../components/QueueControls";
import { QueueEditor } from "../components/QueueEditor";
import { QueueHeader, QueueTabs, SmartQueueRow, StartAtDialog } from "../components/SmartQueue";
import { useStore } from "../hooks";
import { setPrefs, usePrefs } from "../prefs";
import { useT } from "../i18n";
import { MAIN_QUEUE, queueOf, queueOrder } from "../smartQueue";
import type { Download, QueueInfo } from "../types";
import { EmptyState } from "../ui/primitives";

/**
 * Smart Download Queue. The page keeps the queue controls it always had (how many downloads at once, pause
 * everything), then one tab per queue. The selected queue shows its state and rules, its actions, what starts
 * next, a 24-hour timeline when times are involved, and its downloads in the order they will run.
 */
export function QueueView({ onOpen }: { onOpen: (id: number) => void }) {
  const t = useT();
  const queues = useStore((s) => s.queues);
  const byId = useStore((s) => s.byId);
  const ids = useStore((s) => s.ids);
  const selected = usePrefs().queue; // the tab last open (a per-viewer convenience)
  const [editing, setEditing] = useState<QueueInfo | "new" | null>(null);
  const [scheduling, setScheduling] = useState<Download | null>(null);
  const closeEditor = useCallback(() => setEditing(null), []);
  const closeScheduler = useCallback(() => setScheduling(null), []);

  const select = useCallback((id: number) => setPrefs({ queue: id }), []);

  const list = queues?.queues ?? [];
  const q = list.find((x) => x.id === selected) ?? list.find((x) => x.id === MAIN_QUEUE);
  // Which downloads are in which queue, how many each still has to do, and whether it is downloading now.
  const { mine, counts, active, all } = useMemo(() => {
    const mine: number[] = [];
    const counts: Record<number, number> = {};
    const active: Record<number, boolean> = {};
    const all: Download[] = [];
    for (const id of ids) {
      const d = byId[id];
      if (!d || d.status === "cancelled") continue;
      all.push(d);
      const home = queueOf(d, queues)?.id ?? MAIN_QUEUE;
      if (home === q?.id) mine.push(id);
      if (!["completed", "failed"].includes(d.status)) counts[home] = (counts[home] ?? 0) + 1;
      if (d.status === "downloading" || d.status === "verifying") active[home] = true;
    }
    return { mine, counts, active, all };
  }, [ids, byId, queues, q?.id]);
  const order = useMemo(() => queueOrder(mine, byId, q), [mine, byId, q]);

  return (
    <div className="space-y-4">
      <QueueControls />
      {queues && q && (
        <>
          <QueueTabs queues={list} selected={q.id} counts={counts} active={active} onSelect={select} onNew={() => setEditing("new")} />
          <QueueHeader q={q} queues={queues} order={order} byId={byId} all={all} index={list.indexOf(q)} onEdit={() => setEditing(q)} />
        </>
      )}
      {order.length === 0 ? (
        <EmptyState icon="queue" title={list.length > 1 ? t("sq.emptyTitle") : t("queue.emptyTitle")} body={list.length > 1 ? t("sq.emptyBody") : t("queue.emptyBody")} />
      ) : (
        <div className="space-y-3">
          <p className="text-dim">{t("sq.how")}</p>
          <ol className="space-y-2" data-testid="queue-list">
            {order.map((id, i) => (
              <SmartQueueRow key={id} id={id} position={i + 1} order={order} q={q} queues={queues} onOpen={onOpen} onSchedule={setScheduling} />
            ))}
          </ol>
        </div>
      )}
      {editing && (
        <QueueEditor queue={editing === "new" ? null : editing} idleKnown={queues?.idleKnown ?? true} onClose={closeEditor} onSaved={editing === "new" ? select : undefined} />
      )}
      {scheduling && <StartAtDialog d={scheduling} onClose={closeScheduler} />}
    </div>
  );
}
