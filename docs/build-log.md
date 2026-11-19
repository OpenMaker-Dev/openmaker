# OpenMaker build log

> **Historical.** This log covers the first days of the project
> (2026-10-27 to 2026-10-29) and is no longer updated. Since then merged pull
> requests are the record, [ROADMAP.md](../ROADMAP.md) has the current status,
> and open items are GitHub issues. Statements below about what is missing may
> be out of date.

Reverse-chronological notes on building the client from the specs in `docs/re/`.
Each entry: what was built, how it was verified, and what is still open.

## 2026-10-28: Layout workspace and layout catalog

Replaced the synthesized Layout-mode surface and painted Inspector placeholder
with a scrollable canvas using actual provider geometry, selectable objects,
blue selection handles, searchable Objects pane, and four Inspector tabs.
Position reports actual edges/dimensions with unit conversion; Data reports
known field bindings. The mock now supplies bindings for its four field objects.
Manage Layouts opens from File/Layouts/toolbar menus, searches actual layouts,
and switches layouts through the worker. Unsupported authoring actions remain
disabled; nothing writes layout data to a file or server.

Reference: live FileMaker document/View menu plus focused REA decoding of the
installed client's authoring nibs. Provenance, observed labels, design choices
and unknowns are recorded in `docs/re/client-ui.md`.

Validation: GUI unit and UI tests, offscreen screenshots (18 screens, including
Objects/Inspector/Data/Manage Layouts), workspace library tests and GUI clippy.
New workflow coverage checks actual geometry, selection and Inspector values,
unit conversion, object filtering, unavailable-geometry selection reset, layout
search/open/cancel, and shortcut isolation while the modal is open. Visual QA
caught and fixed clipped measurements and overlapping catalog rows.

## 2026-10-27 overnight (unattended)

Goal for the session: scaffold the Rust workspace and implement the protocol
stack bottom-up, each layer tested against the real s03 capture, then attempt a
read-only live connect.

### Done

- **Workspace** (`Cargo.toml`, five crates: `om-scsu`, `om-giop`,
  `om-rctransaction`, `om-protocol`, `om-client`). `unsafe_code = forbid`.
- **`om-scsu`** - FileMaker's SCSU + XOR 0x5A string codec, three length
  framings. Verified by decoding the server name and "Server 26.0.3"
  straight from the s03 Connect reply. Decoder is full UTS #6; encoder emits the
  single-byte/Latin-1 mode plus SQU-quoting, which round-trips and matches
  FileMaker output for the ASCII/Latin-1 text OpenMaker sends.
- **`om-giop`** - GIOP 1.2 little-endian CDR reader/writer (message-relative
  alignment; padding skipped on read, zeroed on write) and Request/Reply
  parse/build (KeyAddr target, service-context list). Verified by parsing and
  round-tripping the captured `_is_a` request byte stream.
- **`om-rctransaction`** - the 20-byte header (verified byte-exact against the
  captured op90 Download With Lock) and the LIFO push/pop body primitives
  (big-endian ints, bools, SCSU strings, raw).

All layers build clean under `cargo clippy` and their unit tests pass.

- **`om-protocol`** - object key, RCFMP12 operations, the opcode catalog, and
  builders for `_is_a`, `Connect`, and `Perform` requests. An ignored
  integration test (`cargo test -p om-protocol --test capture_c2s -- --ignored`)
  parses every request in the real s03 session and reproduces the opcode
  histogram exactly (359 DownloadWithLock, 96 QueryProgram, 19 Login, 19 Open,
  12 HostCommit, 4 HostCreateRecord, ...).
- **`om-client`** - a synchronous TLS 1.3 + GIOP transport (rustls/ring, a
  self-signed client cert, a lab verifier that accepts the server cert and logs
  its subject) and an `isa` subcommand.

### MILESTONE: live read-only handshake works

`om-client isa --host <lab server> ...` connected to the real lab
server over mutual TLS 1.3 and the server replied that its object IS an
`IDL:RCFMP12:1.0`. This validates the whole transport stack (TLS, client cert,
GIOP framing, CDR) against the real server. `_is_a` sends no credentials.

All 11 workspace unit tests pass; clippy clean.

### Also built (foundation for M2)

- **`om-crypto`** - the login credential obscuring (PBKDF2-HMAC-SHA1 keystream
  XOR) and FileMaker password hashing, from spec 4.3. The PBKDF2-HMAC-SHA1
  primitive is verified against RFC 6070 vectors; the FileMaker-specific
  composition (pepper, per-login nonce, Unicode decomposition for non-ASCII) is
  implemented but still needs a live Login capture to confirm. 12 workspace
  tests pass in total.

### MILESTONE: M1 Connect works live (no license key)

The RE worker pinned the exact Connect byte layout (spec 5.2 rewritten). Added
`om_protocol::build_connect` / `parse_connect_reply` and the `MachineInfo`
payload. The reply parser is verified against the captured Connect reply, and
`om-client connect` ran live against the lab server:

- Connected with an EMPTY license key and NO account credentials; the server
  accepted it and returned a connection id, the identity (server name, Server 26.0.3,
  protocol 0x20, 32-char server id), and the 16-byte connection key (login
  salt). Clean Disconnect. So the server does not require a license key to
  Connect - good for OpenMaker.

Key facts from the worker: the reply's leading 16 bytes are the random
connection key (LCG-generated), not a hash; the connection id is the CORBA
return value, not inside the OctetSeq; the server stores but does not validate
the callback object reference at Connect time, so a minimal IOR (one IIOP
profile, zero components) is accepted.

### M2 progress and the Open blocker

- `Perform` reply format pinned (spec 5.6): reply is void + two octet sequences
  (header-less LIFO result stack + 0xFE backchannel). Implemented `ResultReader`,
  `parse_perform_reply`, and Open/Login reply parsers, all verified against the
  captured replies.
- Session layer (`om-client/src/session.rs`): connect, Perform, GIOP fragment
  reassembly, back-channel request handling, Disconnect. `om-client open` added.
- Fixed `Perform` arg3 = `maxreturn` (not the transaction length). Added the
  BiDirIIOP service context on `_is_a`.
- **Blocker: live Open (op78) returns 0x507c** for every file, including
  a hosted solution which a real client opened fine. `_is_a` + `Connect`
  succeed; the server logs nothing for the attempt (not even Access.log), so the
  session is rejected before being registered. Ruled out: file existence (both
  hosted + Normal), txn_id, host SCSU form, the U+E000 host sentinel. The cause
  is at the session/back-channel setup level. A worker is tracing the exact
  op78 0x507c precondition in the server DWARF (root SSH to the lab server available).

### macOS portability verified

The whole workspace builds and all unit tests pass on macOS arm64
(`aarch64-apple-darwin`) with no code changes, on the capture Mac (Rust
installed in its capture sandbox, source synced via rsync; no secrets or
captures copied). rustls+ring, pbkdf2, and the pure
protocol crates are portable as-is. The macOS target (part of the goal) is on
track; once the Linux read path works, OpenMaker can also run natively on the
Mac against the lab server for side-by-side comparison with the official client.

### MILESTONE: GUI chrome now matches FileMaker Pro 26

From the 40-screenshot reference set (re/captures/fm-ui, captured by a
GPT-6.1-Sol agent driving the real client), an Opus 5.5 agent rebuilt om-gui's
chrome: status toolbar (record chevrons/slider, found-set ring "N Total
(Unsorted)", Show All, New/Delete Record, Find, Sort, Share, Quick Find), the
#DEDEDF layout bar (Layout popup, View As, Preview, Aa, Edit Layout), the full
menu bar, and all four modes (Browse/Find/Layout/Preview) with their own
toolbars. Spec distilled to docs/re/client-ui.md. Icons are original glyphs;
Inter (OFL) stands in for the system font. The content area is still a generic
data-driven field list - the real layout renderer (draw the file's own
FMLayoutPB design) is the next GUI step and is not started yet.

### Login fully reverse-engineered (validated) and implementation under way

The op80 Login recipe is pinned and validated against the real capture two
ways (spec 4.3/5.4): the credential keystream (PBKDF2-HMAC-SHA1 keyed by a
per-login PasswordHash salted by the connection key), the PasswordHash
(peppermix + nonce), and the nonce math. Key finding: the second nonce half
`file[0xa64]` is `mInternalCreation`, a file-intrinsic HBAM u32 the client reads
from downloaded blocks, NOT sent on the wire. So Login needs connKey (Connect
reply), the Open nonce, and mInternalCreation from the file's HBAM.

An Opus 5.5 agent is implementing it across om-crypto (obscuring), om-hbam
(surface mInternalCreation), om-protocol (build_login), om-session, and
om-client, and live-validating against test project - using mInternalCreation
read from a root copy of the server file to de-risk the first attempt (op90
download path to follow). Safety: <=3 failed attempts, FMS verbose logging,
unlock via fmsadmin.

### FileMaker UI capture (GPT-6.1-Sol, on the Mac)

Screen Recording granted to SSH; a 6.1-Sol agent is driving the real FileMaker
Pro 26 (test project open) and capturing every mode (Browse/Find/Layout/
Preview), view, menu, and the authoring tools (Manage Database + Relationships
Graph, Script Workspace, Inspector, Security, etc.) to re/captures/fm-ui/, with
a written chrome-vs-content UI breakdown. Feeds the om-gui rebuild to match
FileMaker's look. Principle: om-gui replicates FileMaker's app CHROME; the
central layout area is RENDERED from the fmp12 data (om-hbam/FMLayoutPB).

### MILESTONE: GUI and HBAM decoder landed (parallel Opus 5.5 agents)

- **`om-gui`** - a working egui/eframe desktop app: Open Remote File connect
  panel, layouts sidebar, records in Form/List/Table views, full navigation,
  provider off the UI thread. Runs headless (Xvfb) and on macOS. Plugs the real
  backend in via the `OPENMAKER_PROVIDER` seam / `om_data::DataProvider`.
- **`om-hbam`** - HBAM container + packed-slot decoder: opens `.fmp12`, lists
  layouts and table fields, decodes record values, round-trips losslessly;
  validated on 132 templates + server samples (782k chunks). Its wire path
  (`SlotReader`/`KeyTree::from_slots`) decodes the op90/op86 reply buffers with
  the same code, so server downloads parse directly.
- **Integration plan**: `om-provider` (to build) implements `om_data::DataProvider`
  over `om-session` (connect/open/login) + `om-hbam` (schema/records from the
  op90 download, reply = HBAM slots + u16 lock status, spec 5.6.6). Wire it into
  the GUI seam. Blocked on Login (op90 schema/record downloads mostly need a
  logged-in session; pre-login op90s are only initial catalog reads).

### MILESTONE: M2 Open works live

With a valid `FM_LICENSE_KEY` in the Connect payload, `om-client open` opened
`test project.fmp12` on the lab server: status 0, and it received the per-file login
nonce (`file[0x3ac]`). The 0x507c was purely the server-side license check on
the first Open (not the back-channel). Full path validated live: TLS + client
cert + GIOP/CDR + RCTransaction + Connect + Perform + Open.

Next on the data path: Login (op80) against test project (its own admin account, credentials in .env) using
om-crypto obscuring keyed by the connection key + the login nonce; then schema
read (op90/op92) and records (op86). Login must be built carefully and its
request verified against the captured Login structure before any live attempt
(3-attempt account-lockout cap).

### GPT-6.1-Sol review (M0+M1), applied

Clean-room: no violation found. Fixes applied: SCSU window-offset table
(0x68->U+E000, 0xFF->U+FF60) and UDX (0xF1) per UTS #6; transport message-size
cap (64 MiB) against an oversized-reply OOM. Deferred: the RCTransaction header
session-id/file-id slot naming (offset 8 = session id, 12 = file id; moot for
Open's all-zero header, to fix with the op90 record path) and the encoder's
control-char quoting (low).

### Native GUI data and Browse renderer (2026-10-28)

Resolved the GUI's sample-data seam: the default provider now uses the native
session, authenticated HBAM downloads and verified server TLS. Explicit mock
mode is visibly labeled. The ignored local `.env` is loaded at startup and can
prefill the direct hosted-file connection.

Added file-driven Browse rendering for themed parts/objects, borders, fonts,
static and merge text, field values, literal placeholders, tabs and autosizing.
Local authoring geometry stays separate. Live verification against the supplied
test server loaded both real records on both layouts and exercised record
navigation and tab switching. The supplied layout's 50 objects and 8 font
families are decoded from the server rather than reconstructed in GUI code.

Validation: workspace library tests, 19 existing GUI interaction tests, 3 rich
document tests, 3 TLS verification tests, screenshot generation, the opt-in live
GUI integration and strict workspace Clippy pass. Remaining boundaries and
reference details are recorded in `docs/re/client-ui.md`.

### Lists, script actions and definition workspaces (2026-10-28)

Replaced single-body List rendering with virtual rows, background range fetches
and cached navigation. The native file's private member flags now select the
correct default view and disable views the layout does not support. Loaded the
file's own SVG button icons from library root 31 and resolved binary merge-field
references. Fresh server/UI validation shows all three records and working
Contact List → Contact Details button navigation.

Added provider/worker CRUD, field drafts, Save/Revert, automatic commit before
navigation and confirmed record deletion. Mutable-provider GUI tests cover the
full lifecycle, failed saves and scrolling beyond the first fetch window.
Added native script catalog decoding, a bounded interpreter, button/menu
actions, Script Workspace drafts and the Manage Database metadata/editor UI.
Schema tests preserve record values by field ID during renames.

Native writes remain gated: the first experimental create closed TLS, then
the server stopped accepting connections on port 5003; the maintainer rebooted it and all three records were
verified intact. The malformed create's serial-key framing was corrected, but
no further OpenMaker writes were attempted without reference-frame evidence.
The ad-hoc-signed reference app copy exits at startup; observation of an
unmodified official client is being investigated. Native schema and script
member transactions also require locks, parent catalog indexes/revisions and
table migration that are not yet fully verified. Detailed evidence is kept in
the record-writes, scripts, schema-manager and library-images notes.

Read-only field metadata now identifies the test file's required UUID, account
and timestamp auto-entry values, plus its stored Initial calculation. Raw
commit framing does not establish that the server generates or recalculates
these values; this is another requirement before enabling native CRUD. See
`docs/re/auto-entry.md`.

### Official record creation and update captures (2026-10-28)

Restored the separate capture app's original signature and used a small native
hardware-breakpoint observer after Gadget initialization failed under macOS
code-signing protections. The observer was tested in a hardened standalone
process before loading into the capture copy. The installed client binary and
system security settings were unchanged.

Created and committed `OpenMaker capture test` through the official client.
Fresh native reads verified record ID 10, its UUID/audit/timestamp values and
stored Initial of `O`; the original records 1, 2 and 3 remain. Captured allocation,
locking, commit and unlock traffic. A reversible Last Name edit on record 10
then exposed op14 record staging before op70 field-change notifications. The
Last Name was cleared again and committed. These captures correct the draft
assumption that op70 carries full record field values.

The second observer stopped with zero restoration failures, and both capture
processes were closed. Raw captures remain outside the repository; tracked
fixtures contain structural bytes with synthetic identifiers. OpenMaker native
writes remain disabled pending complete record semantics and verification.

### Hosted field editing and creation (2026-10-28)

Replaced the GUI's global write gate with per-layout create/update/delete
capabilities and an editable-field allowlist. Existing text fields accept clicks
and typing in Form, List and Table views. New Record focuses the first input;
background clicks, Save Record, Cmd+Enter, navigation and Refresh commit the
draft. Failed commits retain edits and block navigation; Revert reloads current
server data.

Implemented bounded schema-driven UUID/audit defaults, stored text expression
evaluation, required-field checks and generated UUID uniqueness. Recognized
the independent index-option mask from DBEngine Get/SetIndexingChoice, including
the hosted schema's generated index flags. Updates retrieve the host's lock
version, compare it with fresh field 252 and the displayed snapshot, and advance
its canonical serial encoding. Both create and update stage complete rows with
op14, notify with op70, and verify values plus versions through fresh reads.
Unsupported rules, protected/calculated fields, deletion and catalog writes
remain unavailable.

The native round trip created blank owned record 12, changed and cleared First
Name, then restored its identifying `OpenMaker editing test` tag. UUID, Initial,
account/timestamp values and versions passed readback. A separate hosted GUI
test clicked/typed/saved its actual rich-layout field, verified through new
connections, navigated away/back and restored the exact original tag. Original
IDs 1, 2, 3, 10 were unchanged. The server retains five records, including the new
identified test row. Ignored screenshot 26 records the focused field editor.

207 workspace tests passed; both targeted native write/UI checks and strict
workspace Clippy passed. Both shared-target and ordinary worktree GUI builds
completed. The previously running process must be relaunched to load this build.

### Implicit version zero on original records (2026-10-28)

The original rows 1, 2, 3 omit bookkeeping field 252, while their successful host
lock replies return version 0 (`01 00`). Comparing an absent field directly
against explicit version 0 caused a false "version changed while locking"
error; the earlier mutation tests exercised a newly created row with an
explicit version and missed this case. Downloaded snapshots, locked rows and
verification now normalize an absent field to version 0, while malformed present
metadata and genuine stale versions still fail. Three regression tests cover
those cases. A hosted no-op edit check exercised all current rows through the
production locking/version path and verified identical raw data afterward;
no original values or bookkeeping were changed. 210 workspace tests, strict
Clippy and the ordinary worktree GUI build pass.

### Open questions / risks

- Target-key internal byte order in the RCTransaction header is not pinned
  (kept as raw bytes). Fine for round-trip; revisit if a build needs to compute
  it.
- Connect reply's leading binary word (possible hash/MAC) is unconfirmed
  (spec 5.2, experiment 3). May block synthesizing a Connect the server accepts;
  will know once om-client attempts a live Connect.
- Live Connect without a license key is untested against the server.

## 2026-10-28 OpenMaker Server, first slice

`om-server` hosts `.fmp12` files over RCFMP12, written from the spec and the
s03 capture. The approach that made it fast: `om-protocol` now holds both
halves of every exchange, and each server-side piece is checked three ways -
against its client twin, against the official client's captured requests, and
by rebuilding FileMaker Server's captured replies byte for byte.

Findings along the way (recorded in network-protocol.md 5.6.1 and 5.6.6):

- FileMaker Server leaves CDR alignment padding uninitialized.
- A Download reply's slot stream is one bracketed list relative to the
  requested path, with minimal chunk codes and value class 0 for empty leaves.
  Re-encoding the decoded subtree reproduces all 356 captured data replies, so
  `om_hbam::Node::to_wire` generates FileMaker Server's exact bytes.
- The 22-byte Download reply block has two values (data / no such key), and
  one reply used a different, unframed shape (open question).

End to end on loopback: `om-client login` against `om-server` hosting the
FileMaker Server sample file succeeds with the configured account and is
refused (212) with a wrong password. Before Login only `[2]` is downloadable.

Next: DownloadList (op92) and QueryProgram (op86) so a client can browse, then
FileMaker Pro itself against OpenMaker Server.

## 2026-10-28 OpenMaker Server: FileMaker Pro, finds, sorts, large downloads

FileMaker Pro 26 now opens `test project` hosted by `om-server`, logs in and
renders its layouts with record data. What it took, beyond the first slice:
accepting FileMaker Pro's X.509 v1 client certificate (custom DER parsing and
ring verification), the pre-Login security catalog `[23, 1, 1]`, the account's
own record key as the Login result key, and an echo for op62 Unlock.

Large downloads page with the resume block shared by every upload/download
op (network-protocol.md 5.6.6a, 5.6.6c); a 141 KB theme at 4 KiB pages
matches FileMaker Server's pages byte for byte.

Finds run in the new `om-query` crate, the op86 QueryProgram engine shared
by server and client (docs/re/query-program.md). It answers from the file's
stored indexes, so it never collates text itself, and reproduces 78 of the
live FileMaker Server replies byte for byte: SimpleFind, JoinProjection, the
RID-list operations, ExistsInIndex, NodeExists/GetKey and SortList, including
a two-field sort. Not yet: Projection (11), LookupValue (17), finder kind 8,
and sorting by a field without a value index (needs the calculation engine
and the collation table).

Next: the write path (new records, commit, locks) so FileMaker Pro can edit.

## 2026-10-28 OpenMaker Server: records are written and saved

Two research specs landed: the record write path (write-path.md; capture
s03 plus 12 live write runs against test project) and how to write
`.fmp12` files (hbam-writer.md; a prototype's files passed FileMaker's
consistency check and opened on FileMaker Server). On top of them:

- `om-collate`: FileMaker's English-form text index keys are the DUCET
  5.2.0 primary weights, verified on whole indexes (159 value keys, 195
  word sets, 147 typed keys across test project and the six FMS samples),
  plus the word breaker and number, date, time and timestamp keys.
- `om-protocol` frames both halves of ops 71, 72, 14, 70, 103, 60, 62, 65
  and the change notifications; all 42 recorded vectors rebuild exactly.
- `om-hbam` writes RID lists with FileMaker Server's rule and rebuilds
  whole files from a key tree; FileMaker's `--checkConsistency` reports 0
  bad blocks for rebuilt and server-saved files.
- `om-server` holds each hosted file's key tree in memory, answers the
  write path (record ids, serials, staged uploads, commits with index
  maintenance, per-transaction locks, notifications to other clients) and
  saves each change as a new file renamed over the old.

Next: FileMaker Pro creating records on OpenMaker Server, Find-mode
captures, and the incremental writer for large files.

## 2026-10-28 Client and server branch integration

The desktop client work now includes the current server, query, framing and
HBAM writer changes. Native record IDs use serial encoding independently of
unsigned table and field keys; read caches, locks, staged rows and notifications
share that convention. Snapshots preserve empty pushed subtrees when an
unrelated text field changes. Offline regressions cover both cases.

The combined workspace passes 274 tests (48 opt-in tests ignored), strict
Clippy and formatting checks. A read-only native smoke check authenticated
and loaded both hosted Contacts layouts and all six current records. Earlier
authorized create, text-edit, clear and GUI save/restore checks remain recorded
in the client notes; integration validation issued no new record writes.

## 2026-10-28 OpenMaker Server: container fields

The maintainer's photo upload from FileMaker Pro failed with "Unknown Error: -1": the
server did not know op88 and answered a 2-byte status where the upload
framing needs its trailer. The containers spec (containers.md; seven live
runs against test project) describes the libraries container data lives
in, and on top of it:

- `om-protocol` frames op88 UploadBinaryData, op87 AdjustReferenceCount
  and op96 DownloadThumbnail; all 21 recorded vectors rebuild exactly,
  FileMaker Pro's own op88 included.
- `om-hbam` keys the pieces of chunked values with serial keys (`7f`, then
  `80 00`), which a 1154-piece picture from the live runs showed.
- `om-server` stores uploaded data in the table's library, deduplicated by
  MD5 (recomputed and checked, which FileMaker Server does not do), counts
  references with op87 and deletes members at zero, refusing any op87 path
  that is not a library member (FileMaker Server wrote to the file's root
  for one), and renders op96 thumbnails at exactly the requested size: PNG,
  or JPEG for JPEG photos. Multi-part uploads of large pictures work.

The live runs corrected one rule: the library's modification count rises
by two per new member and one per deleted member, not per count change.

Next: schema editing (new tables and fields from Manage Database).

## 2026-10-28 Client writes on the specified write path

Review fixes for the client branch. The client's record writes now use the
spec-based `om_protocol::write` framing (write-path.md) instead of a second,
binary-derived implementation, and follow FileMaker Pro's order: op71 to
create, op60 to lock, op14 and op70 to commit, op65 to release. New:

- Delete Record, as write-path.md 7 specifies (lock, op70 `R = 03`, op65).
- Unique fields are checked the way capture s06-omserver-n shows FileMaker
  Pro doing it: a write lock on the field's index `[T, 01, F]` and an op86
  ExistsInIndex (write-path.md 1.3), instead of downloading every record.
- Record pages come from the table's record list `[T, 0d]` and one op92;
  saves read only the table's fields and the record itself.
- Layouts on occurrences past `d0 00 7f` map to their base table (the alias
  is the occurrence key's serial-key value, layout-model.md 4.1).
- The option bits that mark a field's indexes are cited from
  schema-path.md 5.5 and checked against all 4,750 fields of test project
  and the shipped templates and samples (auto-entry.md), not taken from
  engine code.

The unverified op91 catalog upload and the experimental script save were
removed; catalog writes will be built from the schema-editing spec
(schema-path.md). Scripts stay readable and runnable. The GUI reads only explicit `OPENMAKER_*` variables and
never prefills a password. Provider and GUI write tests now host a copy of test
project on an in-process `om-server`, so they run without the lab server.

## 2026-10-28 Secure desktop client license storage

The Hosts dialog and OpenMaker menu now offer FileMaker License: masked entry,
save/replace and removal. The native OS credential store persists the key for
the current OS account (Keychain, Windows Credential Manager or Linux Secret
Service); the desktop client no longer reads a license environment variable.
The provider worker loads the key before each connection. Turning off Use
saved license bypasses storage and clears the next connection's install code,
so an OpenMaker Server connection can proceed without licensing. Failed writes
keep the draft for retry, and storage failures never fall back to plaintext.
License values and the machine-info install code are redacted in diagnostics.

Validation on macOS: 276 workspace tests passed, strict Clippy, formatting and
the desktop build passed. Three UI tests cover persistence across app instances,
removal, failed save/remove retry and bypassing a locked store. A separate real
Keychain test saved, reopened, replaced and deleted an isolated synthetic key.
The five opt-in provider/GUI hosted tests also passed using local database
copies, and the masked license dialog was rendered and visually checked.

## 2026-10-28 OpenMaker Server: Manage Database

The maintainer tried to define a new table from FileMaker Pro and OpenMaker Server
refused the first lock upgrade. The schema spec (schema-path.md; FileMaker
Pro's own Manage Database requests plus live runs that created, changed
and deleted a table on FileMaker Server) shows the server computes almost
nothing: FileMaker Pro edits its own copy of each catalog and uploads it.
On top of it:

- `om-protocol` frames op82 (host clock), op61 (lock upgrade), op73
  (deferred field work), op15 (download without a lock) and op13 (upload
  straight into the file); 52 vectors rebuild exactly.
- `om-query` runs UploadMultiple (store and delete catalog members) and
  DeleteTable as writes the caller applies.
- `om-server` answers the whole dialog: it stores catalogs and members,
  re-reads the schema so new tables and fields take part in writes and
  finds, upgrades and releases catalog locks (a table lock now excludes
  other users' record locks), tells other clients which catalog changed,
  and on op73 drops deleted fields' values and builds field indexes.

A replay of FileMaker Server's own runs (create a table, add a record,
rename/index/delete/add fields, delete the table) against OpenMaker
Server gets FileMaker Server's replies, its notices to the other session
and its file contents byte for byte. One deliberate difference: fields
defined as indexed get their index when their table is saved, where
FileMaker Server waits for an unknown later request (schema-path.md 12.1).

Next: the maintainer's retest of photos and new tables from FileMaker Pro.

## 2026-10-28 OpenMaker Server: the maintainer's first schema session

The maintainer created a table, fields, a relationship, layouts, a portal and a
stored calculation from FileMaker Pro against OpenMaker Server. Fixed on
the way, each from their recorded session and checked with probes against
FileMaker Server:

- GenerateMemberKey (op86 opcode 43), which FileMaker Pro uses to number
  a new layout: the catalog's next id and mod count, written to its index.
- An op13 that stores a single value carries no slots; the server took it
  for an unfinished upload and FileMaker Pro repeated it 18,076 times.
- op60 returns the mod count of any locked node, not only of records.
- Indexes are built on demand: a portal over a field with "create indexes
  as needed" read an index that did not exist yet and showed no rows. The
  server now builds the index a query reads and stores it.

Open: stored calculations are not recomputed on the server when a formula
changes (FileMaker Server does; it needs a calculation engine), and the
op89 CompareModCounts request FileMaker Pro sends after opening a file.

## 2026-10-28 Calculation and script engines: specifications and foundation

The maintainer asked for OpenMaker scripting to be a 1:1 clone of FileMaker's
calculation language and script engine (docs/scripting.md). Work so far:

- Specifications from FileMaker Server 26.0.3's Linux engine (unstripped,
  DWARF): calc-encoding.md (stored formulas), calc-values.md (values and
  evaluation), calc-functions.md (every function and Get selector),
  script-steps.md and script-runtime.md (every step's parameters, the
  runtime, client/server support), with their catalogs.
- Two oracles: `tools/calc-oracle` drives FileMaker Pro 26.0.3 on this Mac
  through an isolated capture copy; `tools/fms-calc-harness` runs FileMaker
  Server's own engine in a network-less Docker container to evaluate, to
  compile text to stored streams, and to evaluate on a record. They agree
  on 338 of 342 cross-checked expressions (the rest are context and
  version strings).
- `om-formula`: the stored formula codec, compiler and printer. All 5,304
  formulas of the 66 templates and test project decode, re-encode, print
  and compile back byte for byte (55,269 MBS example formulas too), and 602
  probes match the engine's errors, spans and streams.
- `om-scsu` now writes FileMaker's own compressor output; ASCII and
  Latin-1 are unchanged, other scripts and C0 controls now match FileMaker.
- `om-calc` value core: FileMaker's decimal numbers (precision, guard
  digits, its multiplication truncation), dates and times, styled text,
  collation, locales, operators and errors; 1,539 engine vectors pass.
  The function library is laid out by Claris category.
- `om-scripts`: typed, lossless step parameters for every step, an edit API
  that keeps block indexes right, and the client compatibility table. Two
  bugs fixed (step value and flags were swapped; the step-key check rejected
  the server samples and many button references). All 3,639 definitions in
  746 sample files re-encode exactly.

Next: the function library (four agents by category), the evaluator over
om-formula trees with the CalcHost trait, the script runtime on top of it,
then review and oracle comparison passes.

## 2026-10-29 Calculation and script engines: running in client and server

Since the previous entry:

- `om-calc` now computes with FileMaker's own decimal arithmetic and
  transcendental algorithms (calc-values.md 14): every one of the 2,179
  value vectors matches FileMaker Server's engine, and 8,179 of 8,186
  engine result lines (the rest: a glibc near-tie in `Sin(1529)`, three
  cartesian `Lookup` rows, one `While`/`Let` slot case). Every pure
  function category is implemented, including JSON, container encoding and
  embeddings, plus `ExecuteSQL` in the new `om-sql` crate (1,617 of 1,625
  engine vectors).
- The script runtime runs every template script against an in-memory
  host. The desktop client runs scripts on its provider worker with
  dialogs, alerts, pause/abort, layout and object triggers, finds and
  sorts with stored specs, windows, Perform Script on Server and Insert
  from URL; it evaluates unstored calculation fields, hide conditions,
  conditional formatting, tooltips and calculated button parameters, and
  record writes run auto-enter, stored calculations and validation through
  `om_data::rules` (editable tables over 400 sample files: 201 to 274 of
  602).
- OpenMaker Server answers Perform Script on Server (op30, op105 callback)
  with the same record rules, transactions that commit atomically,
  privilege checks and limits. Running it: an account must be given
  `--full-access` for its scripts to run (file privilege sets are not
  decoded yet, so other accounts fail closed with error 9 unless the script
  runs with full access privileges); `--scripts`, `--script-steps` and
  `--script-seconds` bound concurrent scripts (100), steps (100,000,000)
  and time (30 minutes).
- Two GPT-6.1 Sol reviews found 12 bugs (transactions, triggers, variable
  repetitions, record rules, Insert from URL limits, server privileges and
  runaway scripts); all are fixed with regression tests.

Workspace: 666 tests pass, clippy and rustfmt clean. Open items are listed
in docs/scripting.md.
