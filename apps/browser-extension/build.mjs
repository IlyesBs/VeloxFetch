// Builds dist/chrome and dist/firefox from src/. No dependencies: plain file copies plus the
// per-browser manifest (Chrome's gets the public `key` that pins the extension id, which is the id
// the native host trusts).
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const src = join(root, "src");
const dist = join(root, "dist");
const key = readFileSync(join(root, "public-key.b64"), "utf8").trim();

rmSync(dist, { recursive: true, force: true });
// "chrome-store" is the same as "chrome" but without the id-pinning `key`: the Chrome Web Store / Edge
// Add-ons assign their own id (and reject a manifest that carries a key).
for (const target of ["chrome", "chrome-store", "firefox"]) {
  const out = join(dist, target);
  mkdirSync(out, { recursive: true });
  for (const f of ["background.js", "videobar.js", "popup.html", "popup.js", "popup.css", "lib", "icons"]) {
    cpSync(join(src, f), join(out, f), { recursive: true });
  }
  const manifest = JSON.parse(readFileSync(join(src, `manifest.${target === "firefox" ? "firefox" : "chrome"}.json`), "utf8"));
  if (target === "chrome") manifest.key = key;
  writeFileSync(join(out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`built ${join("dist", target)}`);
}
