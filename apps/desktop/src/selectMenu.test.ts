import { describe, expect, it } from "vitest";
import fs from "node:fs";
import path from "node:path";
import { nextEnabled } from "./ui/selectMenu";

const css = fs.readFileSync(path.join(__dirname, "styles.css"), "utf8");
const main = fs.readFileSync(path.join(__dirname, "main.tsx"), "utf8");
const src = fs.readFileSync(path.join(__dirname, "ui", "selectMenu.ts"), "utf8");

describe("drop-down lists in the theme's colours", () => {
  it("are installed for the whole app at start-up", () => {
    expect(main).toMatch(/installSelectMenus\(\)/);
  });

  it("draw the list with the theme's tokens only (no grey, no fixed colour)", () => {
    const block = /\.select-menu \{([^}]*)\}/.exec(css)?.[1] ?? "";
    expect(block).toMatch(/background: linear-gradient\(var\(--raised\), var\(--raised\)\), var\(--solid\)/);
    expect(block).toMatch(/color: var\(--fg\)/);
    expect(block).not.toMatch(/#[0-9a-f]{3,6}\b|gray|grey/i);
    expect(/\.select-menu-option\.is-active \{[^}]*var\(--accent-soft\)/.test(css)).toBe(true);
  });

  it("give the field itself the same surface as the other fields", () => {
    expect(/select\.input \{[^}]*background-color: var\(--solid\)/.test(css)).toBe(false);
  });

  it("open inside the field's open <dialog> (the top layer), not on <body>, so a modal never hides the list", () => {
    // The only place the menu is attached to the DOM; inside a showModal() dialog it must share that dialog's
    // top layer, or it is painted under the dialog.
    expect(src).toMatch(/\(select\.closest\("dialog\[open\]"\) \?\? document\.body\)\.appendChild\(menu\)/);
  });

  it("skip options that cannot be chosen when moving with the keyboard", () => {
    const items = [
      { kind: "option" as const, index: 0, label: "a", disabled: false },
      { kind: "group" as const, label: "g" },
      { kind: "option" as const, index: 1, label: "b", disabled: true },
      { kind: "option" as const, index: 2, label: "c", disabled: false },
    ];
    expect(nextEnabled(items, 0, 1)).toBe(2);
    expect(nextEnabled(items, 2, -1)).toBe(0);
    expect(nextEnabled(items, 2, 1)).toBe(2); // the end: stays
    expect(nextEnabled(items, -1, 1)).toBe(0); // nothing chosen yet: the first
  });
});
