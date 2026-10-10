// "Download again": the same link, folder and name, from the start (for a file deleted by mistake). The rules
// live in Rust (DownloadManager::redownload); these only decide where the UI offers it.

import type { Download } from "./types";

/** Finished, cancelled or failed: nothing continues it, so it can be downloaded again. */
export const canRedownload = (d: Download): boolean => d.status === "completed" || d.status === "cancelled" || d.status === "failed";

/** The main button of the download: a cancelled one, or a finished one whose file is gone. */
export const redownloadIsPrimary = (d: Download): boolean => d.status === "cancelled" || (d.status === "completed" && d.fileMissing === true);
