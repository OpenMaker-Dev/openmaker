# OpenMaker architecture

How the workspace is put together and the decisions that shape it. Facts
about FileMaker itself (formats, protocol, behavior) live in
[`docs/re/`](re/README.md); this page covers OpenMaker's own design. For
status by area see [ROADMAP.md](../ROADMAP.md).

## Principles

1. **Clean room.** Implementation is written from the specifications in
   `docs/re/`, captured traffic and observed behavior, never by copying or
   translating decompiled code. Reverse engineering produces specs;
   implementation reads specs. When a spec is missing, the gap is filed as
   a `needs-re` issue and the spec is written before (or alongside) the
   code that depends on it.
2. **FileMaker is a browser for its own documents.** OpenMaker replicates
   FileMaker Pro's *application chrome*: menu bar, status toolbar, layout
   bar, the Browse/Find/Layout/Preview modes, dialogs and the authoring
   tools. The *layout content* in the middle of the window is the file's
   design, decoded from the file (`FMLayoutPB` records, themes, styles) and
   rendered dynamically, the way a browser renders a page. No layout,
   field or theme of any particular file is ever hardcoded.
3. **Lossless round trips.** Every codec keeps what it does not understand:
   unknown HBAM keys, unknown `FMLayoutPB` fields, script descriptor filler
   and flag bits, stored formula streams. Re-encoding an unmodified
   structure reproduces its bytes exactly, which is how a codec is shown to
   be complete and why edits never silently drop data.
4. **FileMaker is the oracle.** Where behavior can be measured against
   FileMaker Pro or FileMaker Server (captures, the server-engine
   harnesses, live probes), OpenMaker matches the measurement. Where it
   cannot, the doc says what is inferred or OpenMaker's own choice, and the
   behavior counts as unverified parity until measured.
5. **One engine, two hosts.** The calculation, script, SQL and transfer
   engines know nothing about the GUI or the wire. The desktop client and
   OpenMaker Server each implement the engines' host traits, so both run
   the same FileMaker semantics.
6. **No `unsafe`.** The workspace sets `unsafe_code = "forbid"`.

## Crate map

Dependencies point down the table: a crate depends only on crates in
earlier groups (or earlier in its own group). Only `om-gui` depends on a UI
toolkit (egui/eframe).

| Group | Crate | Role |
|---|---|---|
| Wire primitives | `om-scsu` | FileMaker's string codec: SCSU with a per-byte XOR 0x5A mask |
| | `om-giop` | CORBA GIOP 1.2 framing and little-endian CDR |
| | `om-crypto` | Login credential obscuring and password hashing |
| | `om-rctransaction` | The RCTransaction envelope inside `Perform` (header + LIFO body) |
| File | `om-hbam` | The HBAM7 (`.fmp12`) container: pages, key space, packed slots; reader and writer |
| Engines | `om-collate` | Text index keys (DUCET 5.2.0) and word breaking |
| | `om-formula` | Stored formula codec, source parser, compiler and printer |
| | `om-layout` | Typed `FMLayoutPB` layouts, objects, styles and themes |
| | `om-calc` | Calculation values (FileMaker's decimal numbers, dates, text) and the function library |
| | `om-query` | Query programs (op86): finds, joins, sorts, found sets over a file's indexes |
| | `om-sql` | ExecuteSQL (FQL): parser, binder, executor |
| | `om-scripts` | Script catalog codec, typed step parameters, script runtime |
| | `om-transfer` | Import Records, Export Records, Save Records as Excel |
| | `om-url` | Insert from URL over HTTP(S) with FileMaker's cURL options |
| Protocol | `om-protocol` | The RCFMP12 session: both the client and the server half of every exchange |
| | `om-session` | TLS + GIOP transport and a live session, as a library |
| Data contract | `om-data` | `DataProvider`, the GUI's only view of data; value types, record rules, the mock provider |
| Host | `om-admin` | OpenMaker Server's Admin Console and Admin API: HTTPS, sign-in, sessions, pages and API tokens, over a backend trait ([admin-console.md](admin-console.md)) |
| | `om-server` | OpenMaker Server: hosts `.fmp12` files for FileMaker-protocol clients |
| Client | `om-provider` | `NativeProvider`: hosted files over the protocol, and local files through a private embedded host |
| | `om-gui` | The desktop application (egui/eframe) |
| | `om-client` | Command-line protocol probe (`isa`, `connect`, `open`, `login`, `files`) |
| Agents | `om-author` | Agent authoring: a versioned JSON service, CLI and MCP server ([authoring.md](authoring.md)) |

Each crate's `lib.rs` (or `main.rs`) opens with a module doc naming its
spec; start there before reading code.

## Seams

**Both halves of the protocol in one crate.** `om-protocol` builds and
parses every request and reply, for both ends. Each server-side parser and
builder is tested against its client twin, against requests FileMaker Pro
sent in captures, and against replies FileMaker Server sent (rebuilt byte
for byte).

**One slot format, two transports.** The packed slot buffers in an
`.fmp12` page are the same buffers `Perform` carries over the wire, so
`om-hbam` decodes local files and server replies alike.

**The data contract.** The GUI talks to data only through
`om_data::DataProvider`, which it runs on a background worker thread.
`om-provider`'s `NativeProvider` implements it for real files; the mock
provider (`OPENMAKER_PROVIDER=mock`) gives labeled sample data for UI work
without a server. A provider panic inside a request's provider call is
that request's error reply (`worker::contain`). One after the call answered
still sends the answer, with the schema read failed when it came asking
whether the schema grew, so the file's calculations stop instead of going
on with a stale schema. One while the provider switches to the window of a
save (an update or a deletion) is that save's failure reply
(`worker::handle_in`, [#313](https://github.com/OpenMaker-Dev/openmaker/issues/313)).
A panic elsewhere ends the running scripts, not the worker, and the request
gets no reply: other replies, such as a save's settling or a page and its
access mask, cannot be made into a plain failure without saying something
untrue ([#444](https://github.com/OpenMaker-Dev/openmaker/issues/444)).

**Local files use the hosted path.** Opening a `.fmp12` file on this
computer starts a private, in-process OpenMaker Server
(`om_server::LocalHost`) on an ephemeral loopback port that only this
client can use, and the client connects to it like any host. One code path
serves hosted and local files; files a window opens together share one
host per file ([local-files.md](re/local-files.md)).

**Host traits for the engines.** `om_calc::CalcHost`,
`om_scripts::ScriptHost` and `om_sql::SqlSource` are what the engines need
from their surroundings (records, windows, dialogs, files). The desktop
implements them on its provider worker (`om-gui/src/scripting/`); OpenMaker
Server implements them for Perform Script on Server and schedules
(`om-server/src/scripts/`). A host capability that is not implemented
returns FileMaker's error 3 (command unavailable), as FileMaker does for
steps a client lacks.

**Commands.** User-visible operations are commands that the UI and agents
share (AGENTS.md rule 3). The agent commands are described once in
`om_author::catalog`: each command's stable id, CLI name, MCP tool, title,
typed parameters (as JSON Schema), its effect (reads, authors the file's
design, or runs a script) and what it needs from the host. The request parser, the CLI, the MCP tool list,
`capabilities` and the service's availability checks read that table, and
the `commands` command returns it without a connection. The work itself
stays in the owning crates (`om_data::field_catalog`,
`om_data::layout_placement`, `om_data::choice`, `om_data::formula_editor`,
`om_scripts`).

A command that needs a desktop window says so (`needs: window`) and
answers a typed unavailability (error 3) outside one. For example,
`choose_value_list_item` runs a window's triggers and draft, which
`om-author` has not. Its desktop path calls the same operation. The
desktop's other menu and toolbar commands are still `om-gui`'s
crate-private `Action`s, which agents cannot reach.

**Index keys are cut in one place.** A field index holds the first 219
bytes of a key and lists records with a cut key under `ff 80` too
(query-program.md 7.1). `om-collate` owns the rule (`cut_index_key`,
`text_index_key`); `om_query::keys` applies it to every key built from a
stored value, the record writes and indexes built on demand of
`om-server`, and the client's own keys in `om-provider`; `om-query`'s index
reader skips the `ff 80` entry; the find planner cuts finder keys and lets
the caller filter when a key reaches the limit. A key compared with an
index anywhere else must come from `text_index_key` too.

**What the file can hold.** `om_hbam::writer` refuses key segments that are
empty or over 219 bytes, paths deeper than 16, items no page can hold and
keys that carry both a value and children; its `MAX_SEGMENT` and
`MAX_DEPTH` are the one definition of those limits, `writer::unwritable`
reports the nodes it would refuse, and `writer::put_refusals` checks a
write against the tree as it is (a node on its path that holds a value
would become a list). `om-query`'s program bounds keep UploadMultiple's
writes within them; `om-server`'s store checks every write against the
tree before making it and refuses it, or takes back the commit that holds
it, with nothing changed. After a failed save it only ever takes out
field indexes, which the records rebuild, and it serves a file read-only
when it loads with content beyond the limits.

**OpenMaker's own protocol extensions.** Features FileMaker's protocol does
not offer are CORBA interfaces of OpenMaker's own, negotiated with `_is_a`
and never sent to a host that has not claimed them (FileMaker Server
answers `false`):

- `IDL:OpenMaker/HostedFiles:1.0` lists a host's published files
  ([hosted-file-list.md](hosted-file-list.md)).
- `IDL:OpenMaker/Authoring:1.0` (opcode 240) carries `om-author`'s edits
  ([authoring.md](authoring.md)).

## Testing method

Correctness here means "the same as FileMaker", so tests compare against
FileMaker wherever possible:

- **Captures.** Protocol code parses and rebuilds real recorded sessions
  (`re/captures/`, git-ignored because they hold credentials and license
  keys). A protocol change is not done until it handles the captured
  bytes, not just synthetic ones.
- **Round trips.** Codecs re-encode every sample file and template
  byte for byte (formulas, scripts, layouts, HBAM pages).
- **Oracles.** FileMaker Server's own engine libraries, run in a
  network-less container (`tools/fms-*`), answer calculation, script, SQL
  and import/export probes; the results are checked in as vectors
  (`docs/re/catalogs/`, `crates/*/tests/data/`).
- **In-process server.** End-to-end tests host a file on an in-process
  `om-server` and drive it with the real client stack, including the GUI
  under `xvfb-run`.

See [development.md](development.md) for how to run each kind, and
[AGENTS.md](../AGENTS.md) for when to add a test at all.
