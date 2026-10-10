// Drop-down lists drawn by the app, in the theme's colours, on every system.
//
// The list a native <select> opens is drawn by the system, not the page: a grey GTK menu on Linux, a system menu
// on macOS, and on Windows it ignores most styles. No CSS can reach it. So every <select> keeps working as it is
// (React state, keyboard on the closed field, forms, screen readers), but instead of the system list, a menu of
// our own opens. Choosing sets the value with the native setter and sends a real `change` event, so `onChange`
// runs as for a system pick. One listener for the whole document: no component changes.

const MENU_CLASS = "select-menu";

type Item = { kind: "option"; index: number; label: string; disabled: boolean } | { kind: "group"; label: string };

/** The rows of a select, in order: group titles and options (with the option's index in `select.options`). */
export function itemsOf(select: HTMLSelectElement): Item[] {
  const out: Item[] = [];
  let index = 0;
  for (const child of Array.from(select.children)) {
    if (child instanceof HTMLOptGroupElement) {
      out.push({ kind: "group", label: child.label });
      for (const o of Array.from(child.children)) {
        if (o instanceof HTMLOptionElement) out.push({ kind: "option", index: index++, label: o.text, disabled: o.disabled || child.disabled });
      }
    } else if (child instanceof HTMLOptionElement) {
      out.push({ kind: "option", index: index++, label: child.text, disabled: child.disabled });
    }
  }
  return out;
}

/** Set the select's value as a user pick would: the native setter (React tracks it) and a bubbling change event. */
export function choose(select: HTMLSelectElement, index: number): boolean {
  if (index < 0 || index >= select.options.length || index === select.selectedIndex) return false;
  const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "selectedIndex")?.set;
  if (setter) setter.call(select, index);
  else select.selectedIndex = index;
  select.dispatchEvent(new Event("input", { bubbles: true }));
  select.dispatchEvent(new Event("change", { bubbles: true }));
  return true;
}

/** The next option that can be chosen from `from`, going `step` (+1 / -1); `from` itself if there is none. */
export function nextEnabled(items: Item[], from: number, step: 1 | -1): number {
  const opts = items.filter((i): i is Extract<Item, { kind: "option" }> => i.kind === "option");
  const pos = opts.findIndex((o) => o.index === from);
  for (let p = pos + step; p >= 0 && p < opts.length; p += step) if (!opts[p]!.disabled) return opts[p]!.index;
  return from;
}

const wanted = (el: EventTarget | null): el is HTMLSelectElement =>
  el instanceof HTMLSelectElement && !el.disabled && !el.multiple && el.size <= 1 && !el.dataset.nativeList;

let open: { select: HTMLSelectElement; menu: HTMLDivElement; active: number; close: () => void } | null = null;

function closeMenu(refocus = true) {
  if (!open) return;
  const { select, close } = open;
  open = null;
  close();
  if (refocus) select.focus({ preventScroll: true });
}

function openMenu(select: HTMLSelectElement) {
  closeMenu(false);
  const items = itemsOf(select);
  if (!items.some((i) => i.kind === "option")) return;
  const menu = document.createElement("div");
  menu.className = MENU_CLASS;
  menu.setAttribute("role", "listbox");
  menu.setAttribute("data-testid", "select-menu");
  const label = select.getAttribute("aria-label") ?? select.labels?.[0]?.innerText ?? "";
  if (label) menu.setAttribute("aria-label", label);
  menu.dir = getComputedStyle(select).direction;
  const rows = new Map<number, HTMLDivElement>();
  for (const it of items) {
    const row = document.createElement("div");
    row.textContent = it.label;
    if (it.kind === "group") {
      row.className = "select-menu-group";
      row.setAttribute("role", "presentation");
    } else {
      row.className = "select-menu-option";
      row.setAttribute("role", "option");
      row.dataset.index = String(it.index);
      row.setAttribute("aria-selected", String(it.index === select.selectedIndex));
      if (it.disabled) row.setAttribute("aria-disabled", "true");
      rows.set(it.index, row);
    }
    menu.appendChild(row);
  }
  document.body.appendChild(menu);

  const place = () => {
    const r = select.getBoundingClientRect();
    const vh = window.innerHeight;
    const vw = window.innerWidth;
    const width = Math.max(r.width, 140);
    menu.style.minWidth = `${width}px`;
    menu.style.maxWidth = `${Math.max(width, Math.min(420, vw - 16))}px`;
    const below = vh - r.bottom - 8;
    const above = r.top - 8;
    const h = Math.min(menu.scrollHeight, 320);
    const up = below < Math.min(h, 160) && above > below;
    menu.style.maxHeight = `${Math.max(120, Math.min(320, up ? above : below))}px`;
    menu.style.top = up ? "" : `${r.bottom + 4}px`;
    menu.style.bottom = up ? `${vh - r.top + 4}px` : "";
    // Same side as the field; kept inside the window.
    const w = menu.offsetWidth;
    const left = menu.dir === "rtl" ? r.right - w : r.left;
    menu.style.left = `${Math.min(Math.max(8, left), Math.max(8, vw - w - 8))}px`;
  };
  place();

  const setActive = (index: number) => {
    if (!open) return;
    rows.get(open.active)?.classList.remove("is-active");
    open.active = index;
    const row = rows.get(index);
    if (row) {
      row.classList.add("is-active");
      // Scroll the list only (scrollIntoView could move the page under it, which would close it).
      if (row.offsetTop < menu.scrollTop) menu.scrollTop = row.offsetTop;
      else if (row.offsetTop + row.offsetHeight > menu.scrollTop + menu.clientHeight) menu.scrollTop = row.offsetTop + row.offsetHeight - menu.clientHeight;
      menu.setAttribute("aria-activedescendant", (row.id ||= `select-menu-${index}`));
    }
  };
  const pick = (index: number) => {
    const it = items.find((i) => i.kind === "option" && i.index === index);
    if (!it || (it.kind === "option" && it.disabled)) return;
    closeMenu();
    choose(select, index);
  };

  let typed = "";
  let typedAt = 0;
  const onKey = (e: KeyboardEvent) => {
    if (!open) return;
    const k = e.key;
    if (k === "Escape" || k === "Tab") {
      if (k === "Escape") e.preventDefault();
      closeMenu(k === "Escape");
      return;
    }
    if (k === "ArrowDown" || k === "ArrowUp") setActive(nextEnabled(items, open.active, k === "ArrowDown" ? 1 : -1));
    else if (k === "Home" || k === "End") {
      const opts = items.filter((i) => i.kind === "option" && !i.disabled) as Extract<Item, { kind: "option" }>[];
      const target = k === "Home" ? opts[0] : opts[opts.length - 1];
      if (target) setActive(target.index);
    } else if (k === "Enter" || k === " ") pick(open.active);
    else if (k.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      // Type the first letters of an option to go to it, as in a system list.
      typed = Date.now() - typedAt > 700 ? k.toLowerCase() : typed + k.toLowerCase();
      typedAt = Date.now();
      const hit = items.find((i) => i.kind === "option" && !i.disabled && i.label.toLowerCase().startsWith(typed));
      if (hit && hit.kind === "option") setActive(hit.index);
    } else return;
    e.preventDefault();
    e.stopPropagation();
  };
  const onDown = (e: MouseEvent) => {
    if (!menu.contains(e.target as Node) && e.target !== select) closeMenu(false);
  };
  const onClick = (e: MouseEvent) => {
    const row = (e.target as HTMLElement).closest<HTMLDivElement>(".select-menu-option");
    if (row?.dataset.index) pick(Number(row.dataset.index));
  };
  const onMove = (e: MouseEvent) => {
    const row = (e.target as HTMLElement).closest<HTMLDivElement>(".select-menu-option");
    if (row?.dataset.index && row.getAttribute("aria-disabled") !== "true") setActive(Number(row.dataset.index));
  };
  // The field moved (the page scrolled under it): close rather than float away from it. A scroll that left the
  // field where it was (a list updating elsewhere) changes nothing.
  const anchor = select.getBoundingClientRect();
  const onScroll = (e: Event) => {
    if (menu.contains(e.target as Node)) return;
    const r = select.getBoundingClientRect();
    if (Math.abs(r.top - anchor.top) > 1 || Math.abs(r.left - anchor.left) > 1) closeMenu(false);
  };
  const onResize = () => closeMenu(false);
  const onBlur = () => closeMenu(false);

  menu.addEventListener("click", onClick);
  menu.addEventListener("mousemove", onMove);
  menu.addEventListener("mousedown", (e) => e.preventDefault()); // keep the focus on the field
  document.addEventListener("keydown", onKey, true);
  document.addEventListener("mousedown", onDown, true);
  document.addEventListener("scroll", onScroll, true);
  window.addEventListener("resize", onResize);
  window.addEventListener("blur", onBlur);

  open = {
    select,
    menu,
    active: select.selectedIndex,
    close: () => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onResize);
      window.removeEventListener("blur", onBlur);
      select.setAttribute("aria-expanded", "false");
      menu.remove();
    },
  };
  select.setAttribute("aria-expanded", "true");
  setActive(select.selectedIndex >= 0 ? select.selectedIndex : nextEnabled(items, -1, 1));
}

/** Install once at start-up: every <select> in the app opens the themed menu instead of the system list. */
export function installSelectMenus(): () => void {
  const onMouseDown = (e: MouseEvent) => {
    if (e.button !== 0 || !wanted(e.target)) return;
    e.preventDefault(); // no system list
    const select = e.target;
    if (open?.select === select) return closeMenu();
    select.focus({ preventScroll: true });
    openMenu(select);
  };
  const onKeyDown = (e: KeyboardEvent) => {
    if (open || !wanted(e.target)) return;
    // The keys that open a system list open ours; arrows on the closed field still change the value natively.
    if (e.key === "Enter" || e.key === " " || e.key === "F4" || (e.altKey && (e.key === "ArrowDown" || e.key === "ArrowUp"))) {
      e.preventDefault();
      openMenu(e.target);
    }
  };
  document.addEventListener("mousedown", onMouseDown, true);
  document.addEventListener("keydown", onKeyDown, true);
  return () => {
    closeMenu(false);
    document.removeEventListener("mousedown", onMouseDown, true);
    document.removeEventListener("keydown", onKeyDown, true);
  };
}
