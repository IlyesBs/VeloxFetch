import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { readdirSync, readFileSync } from "node:fs";
import type { Plugin } from "vite";

const { version } = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version: string };

/**
 * `virtual:locale-meta`: each language file's `_meta` (its name, short name, direction), read from src/locales at
 * build time. The language menus need every name at start; the texts of a language are only loaded when it is used
 * (i18n.ts). A language is still added by adding a file.
 */
function localeMeta(): Plugin {
  const id = "virtual:locale-meta";
  const dir = new URL("./src/locales/", import.meta.url);
  return {
    name: "velox-locale-meta",
    resolveId: (source) => (source === id ? `\0${id}` : undefined),
    load(source) {
      if (source !== `\0${id}`) return undefined;
      const meta: Record<string, unknown> = {};
      for (const file of readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
        const url = new URL(file, dir);
        this.addWatchFile(url.pathname);
        meta[file.slice(0, -".json".length)] = (JSON.parse(readFileSync(url, "utf8")) as { _meta?: unknown })._meta ?? null;
      }
      return `export default ${JSON.stringify(meta)};`;
    },
  };
}

// The strict CSP (script-src 'self', style-src 'self') means: no inline scripts or styles.
// Keep it that way: everything is bundled into external files.
export default defineConfig({
  plugins: [react(), tailwindcss(), localeMeta()],
  clearScreen: false,
  // the version shown in the title bar (a constant in the bundle, not a request at run time)
  define: { __APP_VERSION__: JSON.stringify(version) },
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", assetsInlineLimit: 0, cssCodeSplit: false },
});
