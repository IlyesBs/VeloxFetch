/** The app's version (package.json), put in by Vite at build time. */
declare const __APP_VERSION__: string;

/** Each language file's `_meta` (vite.config.ts `localeMeta`): its texts are loaded only when it is used. */
declare module "virtual:locale-meta" {
  const meta: Record<string, { name: string; short: string; dir?: "ltr" | "rtl" } | null>;
  export default meta;
}
