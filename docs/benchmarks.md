# Performance benchmarks (Phase 20)

How fast the download engine is, what it costs, and where the time goes, measured with `tests/bench`
(`velox-bench`, not shipped) and the desktop app's end-to-end performance test.

```
cargo build --release -p velox-bench
./target/release/velox-bench run --sizes 100M,1G,8G --conns 1,4,8,16,32,64 --adaptive --repeat 3 --dir /dev/shm/velox-bench
./target/release/velox-bench run --sizes 1G --conns 16 --per-conn 25          # 25 MB/s per connection, like a CDN
./target/release/velox-bench run --sizes 50G --conns 64 --dir <a drive with 55 GB free>
./target/release/velox-bench db --segments 64                                  # one progress save
node tests/e2e/desktop-perf.mjs                                                # start-up, frame rate, IPC latency
```

* The **test server** runs in its own process (its CPU is not counted). It serves a position-dependent pattern computed
  on the fly, so any size works without a source file, and a byte written at the wrong place changes the hash.
* Each **case runs in a fresh process** with the real engine and its SQLite store: wall time, MiB/s, the engine's CPU time,
  its peak memory, the time spent saving progress, and the time from the last byte to the finished file.
* **Every file is checked** against the SHA-256 of what the server sent (outside the timing), then deleted. A mismatch
  stops the benchmark. A size that does not fit on the disk (+10 %) is skipped and said so.
* `fixed` uses the engine's size-based connection count up to the limit (a 1 GiB file uses at most 32); `adaptive` is the
  app's default (starts at 4, climbs 4 → 16 → 64 while it pays).

Machine: Intel Core i7-13700F (24 threads, SHA extensions), 32 GB RAM, Linux 7.1, `/dev/shm` (memory) and a SATA SSD (ADATA
SU750, DRAM-less) with btrfs. Release build (`lto = "thin"`, one codegen unit). October 2026.

## A. The engine's ceiling (loopback, file in memory)

| size | connections | mode | MiB/s | CPU s / GiB | peak RAM (MiB) | after last byte |
|---|---|---|---|---|---|---|
| 100 MiB | 1 | fixed | 1127 | 1.13 | 13 | 48 ms |
| 100 MiB | 16 | fixed | 1147 | 3.17 | 60 | 46 ms |
| 100 MiB | 64 | adaptive | 1090 | 4.30 | 26 | 51 ms |
| 1 GiB | 1 | fixed | 1386 | 0.89 | 15 | 0.50 s |
| 1 GiB | 4 | fixed | 1363 | 1.78 | 36 | 0.49 s |
| 1 GiB | 16 | fixed | 1199 | 4.81 | 117 | 0.49 s |
| 1 GiB | 32 | fixed | 1141 | 5.70 | 211 | 0.51 s |
| 1 GiB | 64 | adaptive | 1325 | 2.04 | 66 | 0.49 s |
| 8 GiB | 1 | fixed | 1802 | 0.85 | 19 | 2.8 s |
| 8 GiB | 16 | fixed | 1610 | 5.26 | 142 | 2.1 s |
| 8 GiB | 32 | fixed | 1566 | 6.30 | 311 | 1.9 s |
| 8 GiB | 64 | adaptive | 1657 | 3.50 | 165 | 2.4 s |

Every file exact. **1.1 to 1.8 GB/s** whatever the connection count: the engine is never what limits a real line.
At this speed many connections cost more CPU per GiB: the work is the same, but each connection's socket wakes the
runtime separately, and the kernel spends the difference switching threads (16 connections: 4.5 s of system time
against 0.5 s for one). That disappears at real speeds (B). Here the segment hashes (SHA-256 of what was written,
resume integrity) cannot keep up with 1.5 GB/s, so a backlog is hashed after the last byte; on a real line they keep up (C).

## B. A 100 MB/s line (gigabit), shared by the connections

| connections | MiB/s | CPU s / GiB | peak RAM (MiB) | after last byte |
|---|---|---|---|---|
| 1 | 100 | 1.40 | 16 | 0 ms |
| 4 | 100 | 1.39 | 27 | 0 ms |
| 8 | 100 | 1.40 | 37 | 10 ms |
| 16 | 100 | 2.10 – 2.41 | 56 – 64 | 0 – 14 ms |
| 32 | 99 | 3.26 | 100 | 47 ms |

About **1.4 to 3 % of one core** at gigabit speed, and the file is finished as the last byte lands.

## C. 25 MB/s per connection (servers that cap each connection)

| connections | mode | MiB/s | CPU s / GiB | peak RAM (MiB) | after last byte |
|---|---|---|---|---|---|
| 1 | fixed | 25 | 1.54 | 14 | 0 ms |
| 4 | fixed | 100 | 1.32 | 26 | 10 ms |
| 16 | fixed | 391 | 1.98 | 56 | 56 ms |
| 32 (of 64) | fixed | 717 | 2.43 | 110 | 117 ms |
| up to 64 | adaptive | 313 | 1.41 | 90 | 67 ms |

Connections multiply the speed of capped servers almost linearly. The adaptive mode climbs 4 → 16 → 64 as it measures
gains; on a 1 GiB file it spends part of the 3 s climbing, on longer downloads it reaches the plateau.

## D. A real disk (SATA SSD, btrfs)

The disk decides. This SSD writes 1 GiB with one final sync in 2.5 s when its cache is ready, and in 10 to 16 s when
it is not (the same `pwrite` + `fdatasync` from Python). Alternating the two:

| run | plain write + sync | VeloxFetch, 1 connection, loopback |
|---|---|---|
| 1 | 2.54 s | 16.5 s (the drive's slow state) |
| 2 | 5.78 s | 3.14 s |
| 3 | 2.54 s | 3.23 s |

With the drive ready, the engine is within about 0.6 s of a plain write of the same gigabyte; the difference is the
resume-integrity hashing (A). From a 100 MB/s line, 16 connections reach 70 MB/s on this drive in its slow state: the
drive's own sustained speed. VeloxFetch syncs the file at each checkpoint (every 500 ms) so a crash never leaves
counters ahead of the bytes; a browser does not, so it may report "done" while the data is still only in memory.

## E. Saving progress (SQLite)

| segments | one save (median) | p95 |
|---|---|---|
| 4 | 13 µs | 14 µs |
| 64 | 54 µs | 57 µs |
| 256 | 201 µs | 208 µs |
| 1600 | 1.27 ms | 1.30 ms |

A save every 500 ms per running download: under 0.3 % of one core for any plan the engine makes (at most 4 segments
per connection, 256 at 64 connections).

## F. The desktop app (`desktop-perf.mjs`, debug build, Linux WebKitGTK)

| measure | result |
|---|---|
| page ready / window shown / first paint | 33 ms / 52 ms / 236 ms after the web view starts loading |
| frame rate, idle / 6 downloads running / running + scrolling | 62 / 60 / 60 fps (p95 frame 17 – 18 ms) |
| IPC call from the interface to Rust, idle (`get_metrics`, `list_downloads`) | p50 < 1 ms, p95 1 ms |
| IPC with downloads running | p50 < 1 ms, p95 1 ms, max 5 ms |

The test fails if the window shows more than 150 ms after the page, first paint takes over 1 s, the dashboard drops
under 45 fps (40 while scrolling), or an IPC call's p95 exceeds 20 ms idle / 30 ms busy.

## What changed because of these measurements

* **The whole file was read again after the last byte** (found by "after last byte" growing at 0.5 s per GiB). The
  periodic checkpoints hash finished segments into their own copy of the record; the final checkpoint did not see
  those hashes and hashed every segment again: 3.2 GB read for a 4 GiB file (1.6 s in memory, longer from a disk,
  gigabytes on a large download). Now each checkpoint continues the running hash of the segments still downloading and
  keeps the finished ones for the final checkpoint (`verify::RunningHashes`, `docs/resume-integrity.md`). 4 GiB at
  400 MB/s: **2.0 s → 38 ms** after the last byte, total 11.8 s → 10.3 s, CPU 7.3 s → 6.0 s. Same hashes, same rules.
* `ProgressHandle::downloaded()` reads the byte count without copying the segment list (the benchmark's sampler used
  `snapshot()`, which skewed the CPU it measured).

## Not changed, on purpose

* **The checkpoint `fsync`** (every 500 ms): it is what makes a crash safe to resume; the disk's speed is the limit
  either way (D).
* **Thread count and memory allocator:** with fewer allocator arenas (`MALLOC_ARENA_MAX=2`) the 1 GiB / 32-connection
  loopback case peaks at 146 MiB instead of 353, but at real speeds the memory is already 56 – 110 MiB, and changing the
  allocator affects the whole app on three systems for a loopback-only gain.
* **50 GB:** the tool runs it (`--sizes 50G`, needs 55 GB free); `/dev/shm` here holds 16 GB, so the largest measured
  file is 8 GiB. The engine's memory does not grow with the size (19 MiB at 8 GiB on one connection), and the hashing
  fix removes the step that grew with it.
