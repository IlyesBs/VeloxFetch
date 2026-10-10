# Download again

A finished, cancelled or failed download can be downloaded once more **from the start**, with the same link, folder
and name: for a file deleted by mistake, or a copy that is needed again. It is the same entry in the list (its
queue, priority and speed limit are kept), not a new one.

## 1. What was there before (audit)

| Capability | Before | Now |
|---|---|---|
| Fetch a finished file again | **Missing**: only by pasting the link again, as a new entry | "Download again" on the entry itself |
| Cancelled download | **Dead end**: no action (start and retry refused) | "Download again" is its main button |
| Failed download | Retry (continues from the saved parts) | Retry stays first; "Download again" (from the start) in the menu |
| A finished file deleted outside the app | Not noticed: Open failed | Shown as "The file is no longer on disk", and "Download again" becomes the main button |

## 2. Where it is

* **Download card** (dashboard, Downloads view, Details page):
  * the main button is **Download again** for a cancelled download, or a finished one whose file is gone (Open is
    hidden then);
  * the "…" / right-click menu has **Download again** for every finished, cancelled or failed download.
* **Completed view:** every file has a **Download again** button. It is the highlighted one when the file is gone,
  and the row says so.
* The list is checked again when the window comes back (minimised / restored, or switching back from another
  program, at most every 3 s). A file deleted in a file manager shows as missing on return.

UI texts in en / fr / ar: `card.redownload`, `card.fileMissing`, `act.redownloading`.

## 3. Rules (`DownloadManager::redownload`)

* **Allowed for:** completed, cancelled, failed.
* **Refused for:** running, paused or waiting (they already continue), and a download not started yet. A refusal
  changes nothing.
* **Never overwrites.** If a file with that name is in the folder (the user kept it, or a new file took the name),
  the new copy gets a free name (`name (1).ext`). The engine's usual name claim (`claim_name`) decides it.
* **From the start.**
  * Any partial file is deleted, but only inside the allowed download folders.
  * Size, parts, progress, the server's version tags (ETag / Last-Modified) and the last error are cleared.
  * The engine plans the download as new, so a file that changed on the server is fetched fresh, never mixed with
    old data.
* **Folder:** the same folder while it is still an allowed download folder; otherwise the current download folder.
  A deleted folder is created again. A file sorted into a kind folder (`Video/`) goes back there.
* **Kept:** link, name, queue, priority, per-download speed limit, expected checksum.
* **Not kept:** cookies a browser handed over. They are cleared when a download finishes or is cancelled, for
  privacy. A link that needs a sign-in may fail; Download Doctor then says so and offers "Sign in…".
* The download's log gets "Downloading again from the start". The Download Doctor's old diagnosis is cleared.
* `fileMissing` in the download's DTO: completed and nothing at its path (`symlink_metadata`, so a link is never
  followed).

## 4. Security

* New command `redownload`: main window only (`authz`, capability file, build manifest, destructive-commands test
  list). It takes only an id.
* Paths come from the stored record and are checked against the allowed folders (`AllowedRoots`) before anything is
  deleted. The engine checks them again before it writes (`confine`).

## 5. Tests

| Where | What |
|---|---|
| `tests/integration/tests/redownload.rs` (4) | A deleted file shows as missing, then comes back in place byte-exact (same entry, same path, no `.part`, still one entry). A file still there is never overwritten (`name (1).ext`). A cancelled download starts over, and a file changed on the server is fetched fresh, from byte 0. Only finished / cancelled / failed are accepted (running, paused, not started and an unknown id refused, a paused one keeps its progress), and a failed one runs again. |
| `apps/desktop/src/redownload.test.ts` (3) | Where it is offered, when it is the main button, texts in every language |

## 6. Limitations

* Media (stream) downloads have their own list and Retry. "Download again" covers file downloads.
* A temporary link that has expired cannot be fetched again. The error says so, as for any download.
