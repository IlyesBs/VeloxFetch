# Languages (October 2026)

VeloxFetch speaks **English, French, Arabic, Spanish, Portuguese (Brazil), German, Russian and Turkish**. Indonesian,
Simplified Chinese and Italian are planned next (the code already handles them: only their files are missing).

## How it works

* Each language is one file, `apps/desktop/src/locales/<code>.json` (`en`, `fr`, `ar`, `es`, `pt-br`, `de`, `ru`,
  `tr`), with a `_meta` entry (`name` in the language itself, `short`, `dir`). Every file in the folder is picked up
  when the app is built: adding a language is adding a file. `en.json` is the reference; a key missing elsewhere shows
  in English.
* Rust reads the same files for the tray menu and notifications (`apps/desktop/src-tauri/build.rs` ->
  `desktop::text`), so they follow the interface language.
* **First start in the computer's language** (`pickLang` in `src/i18n.ts`): the first of `navigator.languages` there
  is a file for: the exact code (`pt-BR` -> `pt-br`), else the same language from another country (`es-MX` -> `es`,
  `pt-PT` -> `pt-br`, `de-AT` -> `de`), else English. Traditional Chinese (`zh-TW`, `zh-HK`, `zh-Hant`) is never given
  Simplified. Nothing is saved until the user picks a language (Settings > Appearance > Language, or the title bar):
  until then the app follows the computer. Anything that is not a language code is ignored.
* Settings > Language is a drop-down (in the theme's colours), each language named in itself; the title-bar menu
  scrolls when the list is long.
* Numbers in sentences are written "Label: {n}" where a language's plural forms would otherwise be wrong (Russian).
* Fonts: Arabic uses the bundled Cairo font; Simplified Chinese (when added) the system's Chinese UI font
  (`:root[lang="zh-cn"]` in `styles.css`); the others the system font.
* Words a language shares with English ("Download", "Link", "Server" in German; "download", "link" in Brazilian
  Portuguese) are normal there.

## Translating

Translate every value of `en.json` (keep the keys, the `{placeholders}` and the `<b>` tags; never name a helper
program). The checks below tell exactly what is missing or wrong. Labels in the sidebar's Smart throttle card are
narrow: keep its title and mode names short.

## Tests

* `src/locales.test.ts`: every file has every key of `en.json` and no other, the same placeholders, only `<b>` as
  markup, no tool names, a language name, a code the app accepts; the eight languages ship.
* `src/i18nDetect.test.ts` (4): exact code, same language from another country, Simplified Chinese only for Simplified
  readers, down the list of wanted languages, nothing that is not a code.
* `src/timeline.test.ts`: every diagnostics line in every language, without English left.
* `tests/e2e/desktop-i18n.mjs` (the real app): every language can be chosen; in each, every page, Settings section,
  the Add and Batch dialogs and a failed download's details have no English words; nothing is wider than the window;
  the Smart throttle title and modes fit; the tray follows the choice; a French computer starts in French, with
  nothing saved. Screenshots `docs/screenshots/lang-de-dark.png`, `lang-ru-dark.png`.
* The e2e harness runs every suite in English (`LANG`, `LANGUAGE`, and `--lang` for the Windows web view in debug
  builds only) unless a test sets another language.
