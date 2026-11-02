import { describe, expect, it } from "vitest";
import { LINUX_FONTS, pickUiFont } from "./uiFont";

describe("the Linux interface font learnt for the next start", () => {
  it("is the first of the list that is installed, like the full font list", () => {
    expect(LINUX_FONTS).toEqual(["Inter", "Cantarell", "Noto Sans"]);
    expect(pickUiFont((f) => f === "Noto Sans")).toBe("noto");
    expect(pickUiFont((f) => f === "Noto Sans" || f === "Cantarell")).toBe("cantarell");
    expect(pickUiFont(() => true)).toBe("inter");
  });

  it("is the system's own font when none of them is installed", () => {
    expect(pickUiFont(() => false)).toBe("system");
  });
});
