# Opening a hosted multi-file solution

Baseline: main `f00d3552`.

How long the desktop client takes to open a hosted file that names many
related files, and what it asks the host for on the way: the time to the
first layout drawn with its records, the time to a window that is quiet
with every value shown, and the requests and bytes by file, operation and
catalog. It is the synthetic counterpart of #804's and #983's measurements
on a production-like solution (1,312 requests and 39.3 MB before the first
layout, 63 files opened), which cannot be shared.

## Reproduce

```sh
cargo test --release --locked -j 4 -p om-gui --test startup_load -- --ignored --nocapture
```

`OM_LOAD_RUNS` (default 3) sets the runs per case; the median by usable
time is reported. `OM_LOAD_CASES` picks cases by name, separated by `;`.
`OM_LOAD_DEMAND` is `eager`, `demand` or `both` (the default).
`OM_LOAD_TRACE` adds the root file's requests in the order sent, with
their times. Each case prints as it ends; every request group of the root
is listed.

## Method

- **The solution** (`crates/om-gui/tests/solution/mod.rs`) is synthetic.
  Every file is the in-memory Tasks fixture
  (`om-server/tests/fixtures/tasks.rs`), its Tasks table grown to 60 text
  fields and 200 records. The root has 40 more layouts, and two themes,
  each a copy of OpenMaker's new-file theme; every layout names the first,
  as Contacts' layouts name one of its two. Its data sources
  name every related file, with one occurrence of that file's Tasks table
  and a cartesian relationship to it. Its first layout shows the related
  Name of the first 8 files. Each related file names the next file on its
  host. Files on a second host are named by an `fmnet` path to that host's
  port, the form FileMaker Pro stores (file-metadata.md 1.1). No customer
  file, name or data is in it.
- **The hosts** are in-process OpenMaker Servers on loopback, over
  verified TLS, each recording its connections (`om-server --record`'s
  form). A relay can delay every chunk by a fixed time each way, as a link
  with that latency would.
- **The window** is the real client in the test harness, signing in
  through the Hosts dialog. Cold is the first open in a new window;
  repeated is the same window opening the file again after File > Close.
- **Opened** is when the window has the file: the provider's open has
  ended, its Open, Login and the root's catalogs included.
- **First paint** is the first frame of the window's logic (the harness
  draws no pixels) in which the window is settled: its layout loaded, its
  first page of records read, no request pending, and no script running,
  the OnFirstWindowOpen script included.
- **Usable** is the later of two times. One is the frame in which every
  related value the first layout shows is on screen (the run fails if they
  never are). The other is the last request or unsettled frame before 500
  ms in which nothing new reached the recordings and the window stayed
  settled; the 500 ms is not counted. On a delayed link the recordings see
  a reply before the window does, so the values on screen decide.
- **Each run** hosts its own servers and relay, and shuts them down when
  it ends.
- **Requests** are read back from the recordings: each request's file
  (by the Open that named it), its operation and the first two segments of
  a download's key path, the bytes each way, and the time of the poll that
  first saw it (every 1 ms or so).

## Field metadata construction

Calculation-schema and SQL metadata readers index a table's field-member
nodes for each read instead of scanning all siblings for every field. The
temporary index uses raw numeric node IDs and retains the first matching
member, as `Node::get` does. It is discarded after that table's pass. More
than 16,384 member nodes, or a failed index allocation, uses the previous
linear lookup without rejecting fields. During calculation-schema construction,
SQL metadata shares that table's index, field-node lookup and decoded options,
including SQL entries for fields omitted from calculation rules. Standalone
SQL catalog reads build their own temporary index. No index or decoded options
survive the call. The shared pass removes repeated local metadata work; its
startup wall-time effect has not been measured.

A related file's recovery creation stamp is read from its completed metadata
tree, after its privilege checks and source bindings, rather than rebuilding
the tree for that scalar. This removes one tree construction per successful
related-file load; its wall-time effect has not been measured. Loads on a
shared connection still run in order, including cancellation checks,
authentication, privilege reads and publication.

## Measurements

2026-11-17, release build at `fb89dafa` (this harness on main `f00d3552`;
the same-host cases make the same requests as at `d1e518b2`), x86_64 Linux VM shared
with other builds (load average 12 to 18 during the run). Median of 3
runs. The request counts do not depend on the load; the times do.

| Case | Open | Opened ms | First paint ms | Usable ms | Requests to paint | Requests | Received KB |
|---|---|---:|---:|---:|---:|---:|---:|
| 8 files, loopback | cold | 64 | 103 | 108 | 146 | 162 | 199.4 |
| 8 files, loopback | repeated | 66 | 106 | 111 | 146 | 162 | 199.4 |
| 60 files, loopback | cold | 72 | 257 | 263 | 926 | 942 | 428.4 |
| 60 files, loopback | repeated | 63 | 224 | 228 | 926 | 942 | 428.4 |
| 60 files, 1 ms each way | cold | 114 | 2639 | 2687 | 926 | 942 | 428.4 |
| 60 files, 1 ms each way | repeated | 260 | 2869 | 2918 | 926 | 942 | 428.4 |
| 60 files, 10 ms each way | cold | 437 | 20886 | 21250 | 926 | 942 | 428.4 |
| 60 files, 10 ms each way | repeated | 1747 | 22257 | 22612 | 926 | 942 | 428.4 |
| 60 files, 20 on a second host, loopback | cold | 68 | 226 | 230 | 624 | 642 | 343.1 |
| 60 files, 20 on a second host, loopback | repeated | 72 | 220 | 228 | 626 | 642 | 343.1 |

The 60-file open at 1 ms each way, by file and operation:

- **The root:** 26 requests in all; the window had the file (its Open,
  Login and catalogs) at 114 ms, its layout catalog having come at 90 ms.
- **The related files:** 916 requests from 130 ms to 2,684 ms, all before
  the first paint, about 2.8 ms each: 15 per file, one after another on
  the root's connection.
  - Open (op78), Login (op80) and RequestNotification (op63).
  - `[2]` twice: the login challenge, then again with the catalogs.
  - The catalogs `[3]`, `[T, 3]` for each of its two tables, `[16]`,
    `[25]`, `[32]` and `[33]`.
  - The account's privileges `[23, 1]`, `[23, 2]` and `[23, 3]`.
- **The values shown:** 16 requests read the 8 related Names after the
  first paint (`[129, 13]` and op92 `[129, 5]` of each file).

So the first layout waits for every related file to open and read its
catalogs, though it shows values from 8 of them. The cost grows with the
number of files times the link's round trip (2,639 ms at 1 ms each way,
20,886 ms at 10 ms), and a second open in the same window costs as much as
the first: nothing is kept between opens. A FileMaker Pro 26.0.3 capture of
the production-like solution opened 19 of its files over a 105-second
session (#983), which suggests it opens a related file when something
first needs it; these are not paired timings.

With 20 of the 60 files on a second host, those 20 are not opened at
all: no account is sent to another host until the user signs in to it
(#976), which this run does not do. That case opens 40, and its alert at
open lists the 20 files to sign in to (#981 keeps its OK on screen).

## A root with a large script catalog

2026-11-17, release build, on demand, 1 ms each way, median of 3, the two
commits back to back on the same shared VM (busy with other builds). The
root has 500 scripts of 10 Set Variable steps each (2.8 MB), indexed in
`[17, 1]` as every file FileMaker makes is. Its first script is its
OnFirstWindowOpen script, as a solution's startup script is, in every
case.

First paint is the first settled frame (Method); the startup script
runs before it.

| Root | Build | First paint ms | Usable ms | Requests | Received KB |
|---|---|---:|---:|---:|---:|
| ordinary | main `d80f2254` and this harness | 534 | 557 | 166 | 71.8 |
| 500 scripts | main `d80f2254` and this harness | 915 | 920 | 172 | 8,553.4 |
| ordinary | scripts read as they run (#1053) | 553 | 576 | 163 | 71.3 |
| 500 scripts | scripts read as they run (#1053) | 544 | 571 | 163 | 98.5 |

On main the open read the whole catalog three times before the first
paint:
- for the calculation schema (110 to 159 ms), which needs only the names;
- for the window's Scripts menu (195 to 231 ms), whose reply decodes
  every step;
- for the OnFirstWindowOpen script's run (769 to 793 ms), which needs one
  script.

The related files waited for the first two: they opened from 404 ms. With
the change, the root's script reads are the index (27 KB, at 108 ms) and
the startup script's member (0.2 KB, at 520 ms, once the first layout
had loaded). The related files opened from 131 ms, and the script-heavy
root drew as fast as the ordinary one. The whole catalog is read when
the Scripts menu is first opened, which this run does not do.

## Write rules reading related files

2026-11-17, release build, on demand, 1 ms each way, median of 3, main
`81dec30a` with this harness, on the same shared VM. In the second case the root's Tasks
also has 4 text fields, each auto-entering the Name of one related file
that nothing on the first layout reads (`Shape::write_rules`, #1145). At
that commit, planning a write opened the related files its rules read
(#1139), and the window planned its writes as it loaded the layout
(`RecordCapabilities`), so those files opened before the layout drew.

| Case | Open | First paint ms | Usable ms | Related files opened | Requests |
|---|---|---:|---:|---:|---:|
| 60 files | cold | 471 | 471 | 8 | 160 |
| 60 files | repeated | 470 | 470 | 8 | 160 |
| 60 files, 4 read only by write rules | cold | 605 | 605 | 12 | 216 |
| 60 files, 4 read only by write rules | repeated | 635 | 635 | 12 | 216 |

The 4 files cost 56 requests (14 each: Open, op63, Login and the file's
catalogs) before the first paint, and about 135 to 165 ms here. First
paint and usable coincide in these runs: the window is not settled while
the related values it shows are still being read (#1138).

Since #1146 the window's record capabilities open nothing: a plan they
cannot make while a write rule's file waits is offered from the fields'
definitions, and the write opens the file. The rule files now relate by
Name, as a write's rules need (#187); nothing reads them before a write
either way. In a debug build of #1146, one run each, counts only, both
cases open 8 related files with 159 requests before the first paint. The
4 rule-only files open with the first write that plans them. Those
timings, debug and single, are not comparable with the table's.

## Limits

- **Synthetic files and loopback servers.** The relay delays chunks, not
  bandwidth; packet loss, TLS cost of a real link and a busy host are not
  modeled. FileMaker Server is not measured, nor FileMaker Pro.
- **The time a request was sent** is the poll that first saw it, so about
  1 ms late; the recordings themselves add a write per message.
- **Usable** waits for 500 ms of quiet, so a request later than that
  (a keepalive, a timer) is not counted.
- **Rendering** is not timed: the harness runs the window's logic, not the
  GPU.
