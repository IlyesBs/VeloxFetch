import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { readFileSync } from "node:fs";

const { version } = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version: string };

// The strict CSP (script-src 'self', style-src 'self') means: no inline scripts or styles.
// Keep it that way: everything is bundled into external files.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  // the version shown in the title bar (a constant in the bundle, not a request at run time)
  define: { __APP_VERSION__: JSON.stringify(version) },
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", assetsInlineLimit: 0, cssCodeSplit: false },
});
