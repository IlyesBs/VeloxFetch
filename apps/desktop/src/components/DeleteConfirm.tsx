import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { useT } from "../i18n";
import { Icon } from "../ui/icons";

/**
 * "Delete this file from the disk?": the one place a file is deleted for good, so it always asks first. It
 * offers the safe way out too (remove it from the list only). Focus starts on Cancel; Escape cancels.
 */
export function DeleteConfirm({ name, onDelete, onRemove, onClose }: { name: string; onDelete: () => void; onRemove: () => void; onClose: () => void }) {
  const t = useT();
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancel.current?.focus();
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [onClose]);
  const run = (fn: () => void) => () => {
    onClose();
    fn();
  };
  return createPortal(
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/50 p-4"
      onClick={(e) => {
        e.stopPropagation();
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="card rise w-full max-w-md space-y-4 p-5 shadow-xl" role="dialog" aria-modal="true" aria-labelledby="del-title" aria-describedby="del-body" data-testid="delete-dialog">
        <div className="flex items-start gap-3">
          <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-bad/15 text-bad">
            <Icon name="trash" size={19} />
          </div>
          <div className="min-w-0">
            <h2 id="del-title" className="break-words text-[15px] font-semibold">
              {t("confirm.title", { name })}
            </h2>
            <p id="del-body" className="mt-0.5 text-xs text-dim">
              {t("confirm.body")}
            </p>
          </div>
        </div>
        <div className="flex flex-wrap justify-end gap-2">
          <button ref={cancel} className="btn" data-testid="confirm-cancel" onClick={onClose}>
            {t("confirm.cancel")}
          </button>
          <button className="btn" data-testid="confirm-remove-only" onClick={run(onRemove)}>
            {t("confirm.removeOnly")}
          </button>
          <button className="btn btn-danger" data-testid="confirm-delete" onClick={run(onDelete)}>
            <Icon name="trash" size={14} /> {t("confirm.delete")}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
