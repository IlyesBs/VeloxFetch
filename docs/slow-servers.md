# Slow servers (issue #21)

A download from a slow server (10–50 KB/s, answers that take seconds, connections that break) must go at least as
fast in VeloxFetch as in a browser. A browser keeps **one unbroken connection**. VeloxFetch uses several, and lost time
where a browser loses none. The report: in 10 minutes the app saved 7 MB, and Brave 10.7 MB, at the same shown speed.

## What lost time, and what changed

| Loss | Before | Now |
|---|---|---|
| Lowering the connection count | The download's own judgement often lowers the count: a step up that brought nothing, a speed dip (slow servers swing all the time), errors, a "too many connections" answer. When it did, the connection that was **transferring** stopped (it is the only one that looks), and its bytes on the way were lost. Then came another wait for an answer and a new TCP / TLS start. Requests still **waiting** for an answer (a server serving one at a time) stayed. | Those lowerings are *soft* (`AdaptiveGate::set_limit_soft`). No new request goes over the limit, and requests still waiting for an answer give way at once (`GatePermit::until_over_limit`; they have received nothing). A transfer finishes its request. Only a speed limit or the user's settings (`set_limit`) stop transfers at once, as before. An answer that has already arrived (a refusal too) is read first, so it is counted. |
| Joins between parts | Parts that start side by side (nothing saved before them yet) had their join checked at the very end. Each check was one more request, **one after the other**, each waiting for its own answer: 2 s per join on a server answering in 2 s. | A request ending at a part whose neighbour is not finished reads 1 KiB past its end and keeps that tail's hash. The join is then checked without any request. The few joins left (a request that stopped early, a capped request) are checked **4 at a time**. |
| One-connection transfers | A server slow to answer the quick check (over 30 s) gets one patient request. If that connection broke, the transfer **started again from byte 0**, every time: on a long slow download, the progress never added up. | With a known size, the next attempt asks for the rest (`Range` + `If-Range`). It is used only when the server proves it is the same file: a 206 from that exact offset, the same total and version, and the last saved KiB sent again identical. A server that cannot do that gets the old way (from byte 0) and is not asked again. For such a server the request waits as patiently as the first one (`HttpClient::get_range_patient`). |

Nothing changed for fast servers, and the speed limits, Smart throttle and preview behave as before. Integrity rules
are unchanged: never two versions in one file (see `docs/resume-integrity.md`).

## Measured

A local server with a shared line that swings every second between 10 and 50 KB/s and 2 s to answer each request.
6 MB, curl (one connection, like a browser) and VeloxFetch run side by side on identical servers:

| | curl | VeloxFetch adaptive | VeloxFetch 32 fixed |
|---|---|---|---|
| Before | 201 s | 226 s (47 requests, 18 transfers cut) | 228 s (12 joins checked one by one at the end: 24 s) |
| After | 210 s | 213.6 s (cut: only requests still waiting) | 213 s (no join request at the end) |

What is left is a fixed cost: the first check before the download (a browser does not ask first), which is one wait for
an answer. A server answering in a minute once, then breaking every 300 KB, used to fail after its retries. It now
saves the file in 4 requests, byte for byte.
On a real line, a cut connection also loses what is in the network's buffers and the time to speed up again, so the
real gain is larger than on this local test.

## Tests

See `docs/testing.md`, "Slow servers (issue #21)".
