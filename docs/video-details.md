# Video download details

Clicking a video download opens its own details page, like a file download's. This covers YouTube and similar
sites, and HLS / DASH streams. Before, a video download was only a row in the list, with no details.

## 1. What the page shows

| Section | Contents |
|---|---|
| Header | Title, link, status, and every action the list row has: pause, resume / retry, open, show in folder, cancel, delete file, remove from list |
| Errors and notes | The error in full; the app's notes (in the user's language) |
| Overview | Percent, downloaded / size (estimated with `~` while running), progress bar, pieces or percent, speed, time left, downloaded, remaining, time running / time taken |
| Network | Live speed chart (one reading a second), current, average (over running time only), peak, retries |
| Player | Watch the part downloaded so far, or the finished video |
| Segments | Stream pieces as a strip (done / in progress / waiting); a site sends the video whole, so the page says only the percent applies |
| Metadata | Link (copy), site, type (video page / stream / sound only), quality, file type, subtitles, size in bytes (finished), added, finished / stopped, folder (copy), full path of every saved file (copy) |

* **Live updates.** The page follows the job's events: pause, cancel or finish, from here or from the list.
* **Removed while open:** the page goes back to the list.
* **Opening it:** click anywhere on the row, or its title (a button, so the keyboard works too). The row's own
  buttons and player do not open it.
* **Navigation:** the sidebar marks Downloads, the title bar says "Details", and Back returns to the list.
* **Texts:** en / fr / ar (`md.*`). The row's "left" and "pieces" texts, which were English only, are translated
  now. No tool or technology names appear (the e2e checks this).

## 2. Data (`MediaJobDto`, `crates/download-manager/src/api.rs`)

New fields:
* `url`;
* `quality` (the chosen id; none means the best);
* `format`, `audioOnly`, `subtitles` (count);
* `startedAtMs`, `endedAtMs`, `activeMs`, `runningSinceMs`;
* `fileSize` (the saved files on disk, once completed);
* `retries`.

`track_time` (in `manager/media.rs`) keeps the clock after every change:
* a run starts when the job becomes active and is added to `activeMs` when it stops, with the stop time noted;
* paused time does not count;
* an end is noted once.

Jobs stay in memory, as before.

On the TypeScript side, the new fields are optional, so an older answer still renders. The pure helpers
(`mediaDetail.ts`) are elapsed time, average, size or estimate, full paths (`/` or `\`), type and quality text.

## 3. Code

* `components/MediaJobs.tsx`:
  * `MediaControls` and `MediaPlayer` are shared by the row and the page;
  * `openMediaDetail(id)` sends `velox:open-media`.
* `App.tsx`: the route `{ name: "media", id }`.
* `views/MediaDetailView.tsx`: the page. It reuses `Card`, `Stat` and `Meta` from `DetailView`.

## 4. Security

No new command. The page uses the job list the app already has, and the existing media commands for its buttons.
Paths shown are the job's own folder and files.

## 5. Tests

* `download-manager` `clock_tests::the_details_clock_counts_running_time_only`.
* `apps/desktop/src/mediaDetail.test.ts` (4): clock and average, size and estimate, paths on Windows and Unix,
  type and quality, every `md.*` text in every language.
* e2e `desktop-media.mjs`:
  * clicking a finished stream opens the page: overview, network, pieces, all metadata, player, actions, no tool
    names; Back returns;
  * a running stream's page is live; Cancel from the page; the page shows it stopped.

## 6. Limitations

* A site sends no pieces: that section says so, and progress is the percent.
* Connections per video, server headers and per-download speed limits do not exist for video downloads. Video
  downloads follow the global limits and Smart throttle.
