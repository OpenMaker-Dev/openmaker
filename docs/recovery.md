# Lost connections, reconnecting and save conflicts

What OpenMaker's desktop client does when a hosted file's connection is lost,
how the user reconnects, and how a save that meets another client's change
is settled. FileMaker Pro's own behavior in these cases has not been
captured (re/client-ui.md, Unknowns): this is OpenMaker's design, and native
parity is unmeasured. Code: `crates/om-gui/src/app/recovery.rs`,
`scripting/session.rs` (`ScriptSession::reconnect`), and
`DataProvider::reopen`, `record_by_id`, `retain_record`, `release_record`
and `record_position`.

## Keepalive and a lost connection

While a hosted file is open, the session sends FileMaker's keepalive
`Tickle` after 60 s without a request, and every 60 s after that
([network-protocol.md](re/network-protocol.md) 6.1). Without it, FileMaker
Server closes the idle connection, and OpenMaker Server does after an hour
(6.2). A Tickle never goes out in the middle of another request, and it
stops when the file closes.

A broken transport, a GIOP
CloseConnection from the host, or a Tickle the host does not answer within
30 s of sending it marks the session lost. The provider worker checks once
a second while idle, before every script step, and while a script pauses
or waits on a dialog; any failed request reports it too. The window wakes
when the worker finds out, and then:

- shows **The connection to the host was lost.** in place of the unsaved
  changes bar, with **Reconnect…** and **Close File** buttons. The bar
  counts the unsaved field changes still in the window;
- keeps the layout, records and record number it showed. A layout switch
  that was still loading is undone, and a record request that never came
  back leaves the current record on the loaded page. Layout and record
  replies still queued when the loss is noticed are ignored;
- keeps unsaved field edits on screen, and they stay editable so they stay
  visible. Save, navigation, layout switching, scripts and finds do
  nothing, and no request is sent. Closing the file discards the edits,
  as the bar says; reconnecting keeps them (below);
- stops the running script wherever it is (a step, a pause, a dialog),
  even with Allow User Abort off. Steps still in the current time slice
  (50 ms) can neither ask the user nor reach the host. What the script
  changed in the window is not applied: the loss reaches the window before
  any window state the script sends. A Commit Transaction the connection
  is lost in keeps the record's edits, since whether the host has them is
  unknown;
- starts no script: script triggers do not fire (Close File closes at
  once, without OnWindowClose), the OnTimer script and scripts waiting
  their turn are dropped;
- still allows view changes (Form, List, Table), zoom and the toolbars.

The session refuses requests on a lost connection without touching the
socket. Reconnecting means opening the file again. The inspected
FileMaker Pro recovery path also reopens it; no caller of `Reconnect`
was found in the inspected artifacts, and the inspected servants answer
false. Usage in other builds or client paths is not established
([network-protocol.md](re/network-protocol.md) 6.3).

## Reconnecting

**Asking.** The lost banner's **Reconnect…** opens a dialog for the
window's file: it names the file, host and account it was opened with and
asks for the password. Only the user starts a reconnect; nothing reconnects
by itself. The dialog holds the password in zeroized memory and hands it
to the provider worker, which clears its request copy after the login.
The provider keeps the successful login's pair in zeroized session memory,
as for an ordinary open, for later related reads and script opens. It is
never persisted, logged or shown; the request's debug form redacts it.
Hosted reconnect uses the same optional client-license
policy as opening: an unreadable credential store does not block a
license-free OpenMaker Server connection. A local reconnect reads no license
store and sends no license. The button is disabled while a script runs.

A reconnect is refused, before anything is opened, while the lost session
holds what it cannot recover: a transaction a script opened and had not
committed when the connection went, a Commit Transaction or Revert
Transaction the connection was lost in (whether the host has its writes is
unknown, also once the transaction is gone), changes a script left
uncommitted in any window of the file (its own fields, a New Record whose
record session the reconnect cannot carry (below), or related records'
fields the user did not type or a step created, except the refused ordinary
save described below and a begun row's match fields, which a commit fills
from the window's record at every save;
the worker's windows
count, since the loss reaches the window before the script's last changes
do), a related record deleted through a record and
not saved yet (nothing can check it against the version the window showed),
or a save of a record with changes to related records that may have sent
the host record data before the loss, or that the host took while the
connection went before the window learned of it, the user's or a script's
or a trigger's (whether the host has them cannot be told). The dialog says
why; copy what is needed and close the file.

Such a save is told apart from one that sent nothing by what went out on
the wire. Each request a connection is handed is counted, on the thread
that sends it, unless it neither carries nor commits record data:
- reads;
- locks;
- op71, after which no record exists (write-path.md 3.1);
- op72;
- the host's clock;
- library acquisitions and releases (op87/op88), with separate ownership proof;
- opening a file.

op14 counts whatever its flags say. So do op70, a query (op86, whose
program can delete records), Perform Script on Server, and anything unknown.
A save's requests, through each file and connection it reaches, go out on
the thread that saves, so equal counts before and after it prove that it sent
no host record data
(`DataProvider::submission_mark`). Such a save, a connection that turned out
dead at its first lock or op71 among them, keeps the related changes as they
were before it: rows begun take their match fields again at the next save.
The window's own save (a record's fields alone) left unanswered while its
related changes waited is still refused. A save lost after an op86 of its
own stays refused [I: the count does not read a query's program]. Library
operations alone submit no record data; missing ownership proof still refuses
container recovery as described below. Another file of the lost connection
whose windows hold such changes is not reopened with this one (#1011) and
stays lost. A user's own changes to related records, typed in a portal or a
related field, and rows begun through the record, were never sent: they stay, and are checked
(below, [#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)).

**Opening again.** The worker reconnects the file that owns the asking
window, on the host, port and account it was opened with (a file a script
opened reconnects where its opener's session opened it). It is a fresh
Connect, Open and Login with a new catalog: the file's cached schema,
layouts, scripts and calculation data are read again, and what belonged to
the lost session goes without being replayed: record locks, versions read,
the OnTimer script, pending Perform Script on Server callbacks and queued
OnWindowTransaction scripts. Interrupted scripts do not resume, and global
field values start over from the file's stored values, as on any open. `$$`
variables stay. The native provider keeps the windows' found sets and sorts
(lists of the file's record ids), leaving out records deleted since.

**The other files of the lost connection.** A host's files share one
connection (#1011), so they are lost together. The connection the file
reconnected on is handed to the lost one (`om_session::Session::hand_over`),
and each other file of the session that was on that same lost connection
reopens there with the account and password it holds, nothing more typed,
when it is on the same host and port and was opened with the same account
and password (their digest compared, `Session::successor`): one Connect
for all of them, and each file its own Open and Login there unless a file
already open on the new connection for that account holds it (#1016). Each
keeps its windows' found sets and reads what it shows again, as the asking
file does (`DataProvider::reopen_on_successor`); the dialog's answer names
the files open again, and their windows leave the lost state. A file
opened with another account, a file on another host, one whose session
holds what a reconnect cannot recover (above), and one refused at its Login
stay lost, with their banner, for their own Reconnect: none is tried again
with anything else [I: OpenMaker's choice; whether FileMaker Pro reconnects
a host's files together is unmeasured]. What a script, a lock or a timer
held in the lost session is gone for every file ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)).

A local file reconnects through its existing private engine, preserving its
exclusive file lock and endpoint across both a refused and successful login.
The file is not closed and reopened from disk to reconnect its session.
Each owning window reads and retains its own record version; settling an edit
in another window cannot advance or release that draft's conflict baseline.
The version travels with the exact accepted record page and is retained
before a typing-triggered action or save is queued. A page read completed
in the background, even one later accepted while the draft stays on screen,
cannot supply a newer baseline for that draft. A native draft without an
accepted version is refused with 306 until a successful check establishes one.
Retention happens immediately after collecting frame edits, before focus
validation/save/exit/enter triggers. Script dispatch also queues any new
retention before capturing and sending the job's drafts, including callbacks.

The native provider keeps the fresh login's ephemeral credentials for
related files reached later, including a cold Open File or foreign script
call. Each target performs its own Login and retains its own account
privileges; a refused account follows the existing explicit sign-in flow.
Closing or replacing the provider drops its session credentials. This
lifetime matches OpenMaker's ordinary opens; FileMaker's credential reuse
after reconnect remains unmeasured.

A failed attempt (a wrong password, an unreachable host, a refusal above)
keeps the dialog up with the reason and leaves the file as it was: lost,
every window's edits on screen, empty edits included. Cancel closes the
dialog; Close File still discards the edits as before.

**Checking the edits.** With the file open again, the worker reads back
by id each record the file's windows hold unsaved edits of (and a record a
lost delete was on its way to), whatever the found set, and finds each
window's current record in its found set. Each window goes back to the
record it showed (by id, not by position; when the record is no longer in
its found set the window stays where it was and says so) and compares each
record read back with what its edits started from: each edited field's
value when the edit began, and the record's values as last received before
the loss.

- Every edited field as it was, and no other field changed: the edits
  stay, to be saved or reverted as usual.
- Every edited field holds its edit on the host (a save whose answer the
  loss took away reached the host): the edits are no longer pending and
  nothing is sent again.
- Anything else: an edited field changed on the host, some edits there and
  others not, another field changed, the record deleted, or not read back.
  The edits stay on screen but are held. Nothing saves them: the window
  refuses saving, moving to another record, layout or mode changes, finds,
  closing and scripts, with an alert, and no script trigger fires in the
  file's windows, until the user chooses. **Keep My Changes** (offered
  when the host changed the record) lets them be saved over the host's
  values of the fields they change; **Discard My Changes** gives them up for
  the host's version; Revert Record discards the current record's. Edits of
  a deleted record can only be discarded (recreating it is not offered).
  Edits of a record that could not be read back can only be discarded or
  read again with **Check Again**, which checks them as above once the read
  succeeds.

**Related records.** Each related record whose fields the windows hold
unsaved edits of is read back on the new session too (by base table and
id, `CalcNeed::Record`), and a save of it answers to the version read then
(`DataProvider::retain_related_record`), whatever a later read sees. It is
compared with the edit: its modification count with the one the window
showed when the edit began, or, without one, the edited field's text with
the text it began from. Unchanged, the edit stays to be saved as usual.
Changed on the host, gone, or not read back, the edit stays on screen and
holds the whole record it was made through, as that record's own edits are
held: Keep My Changes lets it be saved over the host's values, Discard My
Changes gives up the record's changes, Check Again reads the record and its
related records again: one still not read keeps the record held, and once
it is read the record is held as changed (Keep My Changes or Discard My
Changes), since its own values cannot approve its related cells. Rows
begun through the record stay as they were, to be created with its save.
The pending edits are not overwritten when the window reads its portals
again.

The version a record was read back at is the one a save of it is checked
against (`DataProvider::record_by_id` keeps it), not the version a later
page read or script sees: the window's reload, a refresh or a script's
read cannot stand in for the check. When the host changed the record after
it was read back, the save is refused (306), and the window holds the
edits again and reads the record again, offering Keep or Discard against
the host's newer values. A saved, reverted or discarded record's version is
released; so is that of a record the reconnect read back but whose edits
the window does not keep.

Once the file's fresh layout list is in, every window of the file takes it
(names, menu, layouts added or removed) and reloads its layout and records.
A window whose layout is no longer in the file says so and shows the first
layout; the edits made on the layout gone are held, to be copied and
discarded.

A record write the loss left unanswered is never sent again. A save is
reconciled as above; a delete is reported as done or not by reading the
record back; a save of changes scripts made is reported as unknown.

**A new record not saved yet** ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)). New Record holds the record in the
window's record session: op71 reserves its id on the host and op72 its
serial numbers, neither of which changes a record, and its values go to
the host only with the window's save. A reconnect carries such a session
to the new connection when nothing of it was submitted
(`DataProvider::carried_record_sessions`): it holds only new records, no
save has begun in it (one refused before any record data is rolled back
first), and no save that may have sent data took it out. The provider
copies it before the lost session closes and, once the file is open
again, holds each record again under an id op71 reserves on the new
connection, with its values as they were: its cells (a script's among
them), its creation clock and UUID and what they computed, and its serial
numbers. Nothing is computed again, no auto-entry runs again and no serial
number is taken again; the id the lost connection reserved is not
reused. A reopen that fails keeps the copy for the next attempt. The
window's drafts, a row begun in its portals among them, follow the record
to its new id, the window finds it there in its found set (after records
other clients made meanwhile, in an unsorted one), nothing of it is read
back (the host has none of it), and the bar says it was made again. Rows
begun in its portals keep their previews, and the serial numbers, clock
and UUID those drew (record-writes.md); a row of another file's table,
whose preview ends with that file's session, keeps the window's new record
from being carried. A window's records are held again all
together or not at all: when one cannot be (its record session not begun,
op71 refused, an invalid id), the session begun for them ends and they are
kept exactly as they were, under the lost connection's ids, nothing sent
again by itself. The window shows them, frozen (no field takes typing, a
save does nothing, leaving the record is refused), with a bar that says
why and offers **Try Again** (`DataProvider::recreate_record`: op71 only,
nothing computed or reserved again) and **Discard New Record**
(`discard_unrecreated`: never submitted, so nothing on the host changes;
its serial numbers stay used). Closing the window is refused until the
user chooses, as leaving the record is; a later reconnect carries them
again. Outside the prepared attachment groups and refused ordinary group below,
not carried, and so still refused: other sessions holding container data,
another file's records or an existing record's changes, and a new record whose
save may have reached the host (that save's report and freeze,
record-writes.md). A
field whose value is the record's id (`Get ( RecordID )`) computed when it
was made keeps that value; the record's id is the new one [OpenMaker's
choice; FileMaker Pro's handling of a lost connection with a new record
open is unmeasured]. The reconnected window shows what it
found in a **Reconnected to the host.** bar until the user dismisses it,
saves, or resolves the held edits. A connection lost again later starts
over: the next reconnect checks every edit again.

**An unsaved record with an uploaded attachment** ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)). A single ordinary
new or prepared existing record in the opened file can carry positively
acknowledged table-library uploads before its first Save. Its no-record-submission
baseline is captured
when the record session begins, and its original own principal, static rules,
schema and file identity are captured before the new record is reserved.
For an existing record, `prepare_container_edit` takes the exact accepted page's
RID/version and attachment field before acquisition. It checks own static
View/Edit, layout/field and library permissions, reads the original stored row
and its container identities without advancing the accepted version, then locks
the same RID at that version. Existing pending acquisitions or a later observed
version cannot supply this baseline. Libraries whose existing header lacks its native identity remain unsupported:
normal library creation supplies that identity, but extending an unidentified
library does not repair it. No identity is invented for recovery.
Unprovable static/library metadata leaves
ordinary entry without this recovery promise; a missing/stale accepted version
refuses preparation. Stored originals are observations, never acquired ownership.
The accepted acquisition retains immutable bytes, library/member identities and
owned counts. Availability checks use cached proof and send no requests.
Missing proof refuses reconnect before it opens or closes a session; an
unacknowledged upload retains its request bytes and remains Unresolved.

Explicit Reconnect detaches the sole cleanup receipt before closing the old
session and revalidates all own and asset proof. A new record reserves one
replacement record ID. An existing record first checks the complete original
row/version and observed stored members, then reacquires the SAME RID's op60
lock and checks the returned version again before transferring its acquisition
ledger. A conflict, deletion, narrowed proof or lock refusal keeps the drafts
and sole receipt Unresolved; it neither rebases nor invokes Keep My Changes.
Reconnect neither uploads data nor changes counts or reissues serials,
clock or UUID. The draft waits for explicit Save or Revert. Revert and current
or parked window cleanup validate ownership before a single release; failed
proof retains the receipt. A validation failure before release keeps detached
ownership retryable; it does not restore release authority to the old transaction.
Only explicit Reconnect or a proven Revert can settle it; new acquisitions and
Save remain refused until then. A second reconnect before Save repeats these checks.
If live work adds another record, a deletion or a stored-source acquisition,
known acquisitions stay with that live transaction and this single-record
recovery proof ends. Related work also ends it unless each target is affirmatively prepared as
one of the owning-file groups below.
A single-record proof cannot substitute for a cross-file receipt. Existing
records without affirmative preparation, explicit transactions, submitted/unknown
saves, conditional/missing permissions, file-library references and stored-source
recovery remain outside this case. GUI Insert carries its accepted page version
before uploading; only the prepared existing attachment field's script cells
are eligible for this single-record recovery.

These operations are PM-chosen synthetic acceptance cases for ordinary new and
existing record attachment entry. They are not evidence that a real
hosted solution uses either sequence, nor a comparison with native FileMaker Pro or FileMaker Server.

**Existing attachments across files before Save** ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)). Insert into one
existing opened-file record and one existing row in each of several external
files can form an ordinary never-submitted group without first attempting a
Save. Root may be inserted before, between or after the related attachments.
The external-file admission limits still apply (128 file-open attempts,
4096 discovered sources and 16 paths per source); this does not add discovery
or permit more than one row per physical file. Each operation must prepare
its exact accepted RID/version
before acquisition. The portal caller resolves its target once and carries the
version shown by the UI; it cannot substitute a later worker read. The related
owner uses the immutable file identity from its own accepted Open/route result,
its own principal, static permissions, schema and complete original row.
When Related is inserted first, its preparation also receives the accepted
Root layout/RID/version before the first acquisition. Root's complete original
row and own proof are captured without choosing a field or taking ownership.
This awaiting-Root state promises no reconnect. Later Root Insert extends its
actual field permissions under the original stamp before upload. Eligibility
requires Root and every declared owner's known upload acknowledgements and
immutable ownership proofs; no later record read can replace an accepted
baseline. Declaring another owner makes the entire group incomplete before
joining it. Failed preparation or an unknown upload cannot leave an eligible
earlier subset. Intended and joined owners must match exactly. Aliases reuse
the same physical owner's receipt only when accepted identity, native table,
RID and version agree; they never create a second cleanup authority.

Every original row, version, rule, route and immutable library/member proof
is validated before replacement record work. Reconnect reacquires every
same-RID lock and checks every returned version before transferring any
acquisition ledger. Before old sessions close, the receipt also retains the
bounded read-demand history and accepted import topology, including passive
files that own no draft. Reconnect repeats those read groups and requires the
same file order, accepted identities/creation values and published client ID
mappings before participant validation or replacement locks. Passive files
receive no write/library permission or asset/cleanup authority. Missing topology
proof or more than 4096 recorded demand occurrences refuses recovery. No cached
schema is installed and no earlier participant stamp or accepted row baseline
is replaced; proof-changing schema or routing growth still refuses.
Existing edits remain live. It neither creates a record nor saves, reuploads,
regenerates values or adjusts reference counts.

An owner's upload payload is retained before wire I/O and collected even when
the upload fails. Missing ACK/fingerprint evidence makes the entire group
Unresolved with all drafts and bytes retained. Closing or a failed Revert
detaches live cleanup authority, including unproved acknowledged counts;
those counts remain evidence, never permission to release. Explicit Save or
Revert still requires the whole group's proof. A validation refusal before
submission retains the receipt across the temporary Save mark and rollback;
correction and another explicit reconnect remain available under that proof.
Original stored references are observations, not acquisitions owned by the
draft. Repeated Revert cannot release them. This bounded case requires an
acknowledged actual table-library upload in each file and unchanged own static
permissions (or FullAccess).
A Related-only interim draft, new rows, multiple rows per owning file,
explicit transactions, submitted/unknown saves, conditional/missing proof,
file libraries and stored-source recovery outside the bounded case below remain
unsupported. This is a PM-chosen synthetic
starter-style operation. The three-file actual-upload regression opens a later
owner on demand after Related A, Root and an intervening passive Read-Only
file; it does not preload the owner's baseline. The passive file stays outside
the write/asset participants and receives no replacement record lock. It is
not an N/order matrix, multi-host functional acceptance or
real hosted-solution use. Native FileMaker/FMS parity has not been measured.

Root may copy a metadata-only original stored container into a different field
of the same accepted existing row/table as its first mutation, then admit one
existing Related row with an actual table-library upload. This is also supported
after both rows have acknowledged actual uploads. The owning operation
`container.prepare_stored_copy` receives the actual accepted destination
RID/version before acquisition; Unavailable preserves ordinary copying without
promising recovery. The calculation field read carries the actual
source row/field/repetition/version; a library member or reference string alone
proves no source. Root's unchanged original row must name that member, its own
static rules must permit View of the source and Edit of the destination, and
all owner/row/schema/library proofs pass before acquisition. The caller's actual
Root layout extends the original footprint without replacing its baseline.

The client retains a typed stored-copy intent and the original member's native
identity, immutable bytes/unknown streams and count before op87 +1. Only a
positive ACK with matching fingerprint/count credits one acquired reference.
Original stored references remain observations with owned count zero. A lost
ACK or changed proof retains the whole group Unresolved, including pending
bytes, and never retries or releases a prefix. Reconnect takes the existing
same-RID locks but sends no op87/op88 or Save. Explicit Save transfers the extra
reference into the destination; Revert releases only the acknowledged extra
reference once, leaving the source's original reference intact. Stored-source
copying from materialized or cross-library sources, new/multiple rows and file
libraries remain outside this case. The synthetic lifecycle and lost-ACK relay
are OpenMaker verification, not native FileMaker/FMS recovery parity or real hosted-solution use.

**Mixed rows with attachments before Save** ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)). A new or accepted existing
opened-file record with an ACKed table-library upload can admit several accepted existing
related rows and several new log rows, including different native tables in
one physical owning file. Each logical row has its own before-work intent;
its file retains one transaction, original own principal/schema/rule proof and
acquisition ledger. A Creating token precedes op71, and only the engine's
complete acknowledged creation binds a new RID to its retained serials, UUID,
clock and validated preview provenance. A partial creation keeps its caller
input, known reservation and ACKed serials as evidence, never permission to
issue again. Exact declared membership must cover every Held and parked row;
an incomplete row freezes the whole group rather than leaving an eligible
prefix. The retained intent limit is 4,096 related rows.

Existing row proof takes the exact caller-accepted RID/version before its
first edit, with own static View/Edit and field permissions and the complete
original stored row. It never substitutes a later observation. An actual
Insert additionally prepares that row's first-repetition container field and
original library/member proof before acquiring data. Another row or repetition
cannot borrow that preparation. Scalar-only participation needs no library
permission or fabricated upload; an unused container field is harmless. The
existing-only attachment subtype keeps its original stricter proof.

Success and error paths collect each owner's complete evidence. Pending upload
bytes keep their prepared row/field association; unknown ACKs block recovery.
Two ACKed uploads that deduplicate to one member retain two owned counts in
that owner's ledger. Stored references are observations, not acquisitions;
equal IDs or bytes in another file confer no cleanup authority. Original own
stamps and passive read topology remain unchanged. On-demand loading can append
a previously unread table's complete field catalog only after every earlier
definition, principal and global catalog compares equal. Caller footprints may
extend under that proof; generated cells remain engine-owned creation state.

Explicit Reconnect validates every owner, original existing row/version and
actual acquisition before any replacement lock or reservation. It then
reacquires each existing SAME-RID lock and reserves only the complete new rows.
All replacements must succeed before any acquisition ledger transfers. Mapping
uses owning table and logical row, including colliding raw IDs. Recovery never
submits records, reuploads, adjusts counts or redraws issued values. Save/Revert
remain explicit and use whole-group sole cleanup; repeated Revert cannot release
again. Changed or unknown proof retains all drafts and acquisitions Unresolved.
Unknown submitted Save outcomes retain their existing explicit resolution path;
this receipt provides no automatic replay or distributed atomicity.

Providers advertise whether existing entry needs asynchronous preparation;
otherwise ordinary input keeps its existing synchronous behavior without a
recovery promise. Preparation errors retain unaccepted text, allow the alert to
be dismissed, and offer **Discard Input** without discarding accepted edits or
attachments. Discard remains available offline; reconnect requires another
explicit click after discarding refused input. Discarding never replays a queued
Save or Reconnect; a refreshed accepted page
can then start a new entry. This entry/discard policy has not been compared
with FileMaker Pro.

For an existing Root, the shared `record.prepare_edit` operation retains the
caller-accepted layout/RID/version, original stored row and own static Edit
footprint before the first changed scalar cell. Browse keeps text, paste and IME
in the same live editor while the worker prepares that baseline, publishing
neither drafts nor triggers/actions until its response. Script entry uses the
same operation before mutation. `Unavailable` preserves ordinary editing but
prevents later Insert from inventing a before-work baseline. Missing or
conditional proof confers no recovery authority; a real stale version or lock
failure refuses the mutation. Find, globals and unchanged values need no
recovery receipt. Later fields extend the original footprint; an actual Insert
adds library/member observations under that original row proof. Scalar entry
alone grants no library permission or acquisition ownership. Existing Root stays
an Edit with its original RID and checked version throughout reconnect; only
new related logs reserve and remap IDs. Insert-first entry retains the same
original row before upload and can subsequently extend its scalar footprint.

Normal script Insert stages accepted drafts through `write_record`, without
committing, before another acquisition. Existing versions, separate new draft
reservations, typed cells and owner-bound previews follow the normal caller.
Both new-Root and existing-Root lifecycles remain covered. The existing-Root
GUI case starts with real Browse scalar input before Insert, then two accepted
related edits and two uploaded new logs. An unchanged held container cell can be
restaged without borrowing the most recent Insert's target proof. The provider lifecycle uses two in-process owned
server endpoints, multiple existing Tasks and new Notes rows in one related
file, and an intervening passive Read-Only file. It covers Save, Revert and a
later existing-row conflict refusing before replacement work. The GUI caller
has displayed related container occurrences because Insert's current capability
check requires them; arbitrary cold Insert into an undisplayed occurrence is
not established. Its keepalive and idle worker detect loss without Refresh or
Commit, and an armed OnRecordCommit witness stays zero through reconnect and
reaches one after explicit Save. Stored rows remain unchanged beforehand.

This synthetic workflow does not establish real hosted-solution use, native/FMS
recovery parity, two OS server processes or stock-client acceptance. Explicit
transactions, submitted/unknown save replay, conditional/missing proof, file
libraries, stored-source copying and related container repetitions beyond the
first remain outside this extension. Unknown creation/upload outcomes are retained, never
replayed.

**A refused ordinary save across files** ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)). An ordinary save refused
before any participant submitted record data keeps a receipt for the window,
the existing records' checked modification counts, and every owner's parked
new records with their issued serials, creation clock and UUID. This includes
a new record in the opened file, an existing related record the user typed,
and a new related log a trigger created. The opened file's held new record
joins the same receipt as its related parts; it is not recreated separately
before their checks. Accepted corrections to the held new record are copied
before the old session closes, so recovery keeps the latest values along with
its original issued creation state. The receipt remains usable only while the
provider can prove that no record-data request has since been sent on its
worker thread.

Explicit Reconnect checks every participating file's endpoint/path identity,
internal creation value, table routing and field metadata, each file's own
authenticated permission proof, and every retained existing record's host
version. All checks precede any replacement reservation. Only then does it
reserve fresh wire IDs for the held and parked new records and remap the
window's current record and related reservations by owning table. The opened
file's found set follows its own table's mapping even when another file used
the same numeric record ID. It carries the issued values directly:
no new serial, clock, UUID or auto-entry is drawn by recovery, and no save is
replayed. All drafts wait for correction and an explicit Save or Revert.
Missing proof or a failed check leaves the entire group Unresolved with all
drafts and issued values kept; recovery sends no record data. A failed
connection attempt keeps the receipt for another explicit Reconnect, which
repeats all checks even after the old sessions have closed.

Uploaded container assets have a separate ownership receipt for each file.
It retains the acknowledged acquisition count, the library and member native
unique IDs (re/containers.md 1.2-1.3), and the complete immutable member tree,
including unknown streams. Reference counts and modification stamps can change;
all other bytes must match. Every file, record version and asset is checked
before any fresh record ID is reserved. Under custom permissions the asset owner
must also prove unchanged own static rules, global record/field visibility and
unconditional Edit on the actual wire table of each acquired library member.
The record/layout/caller-field proof still applies separately; permission to
change a library count does not grant permission to edit a field. Closing the old session detaches these
acquisitions; only after all new reservations succeed does the replacement
session take ownership. Recovery neither uploads bytes nor increments references.
An unused container field in the schema requires no asset receipt.

Direct writes remove the acquisitions they may store from pending draft
ownership before submission, so a later transaction cannot release them.
Explicit Save transfers used acquisitions to stored records and releases unused
ones. A correction's acknowledged release updates the retained receipt as well
as the live transaction; its next Revert cannot release a stored reference again.
Positive related adjustments join the owner's retained receipt too. Discarding
the owning window settles that receipt before ending its current or parked
transaction; a failed proof or release detaches it and retains its bytes instead.
Library op87/op88 requests change the separate asset ledger, not the record-data
submission count; unknown operations and record writes still invalidate no-data
proof. Explicit Revert validates asset ownership before releasing it, including
after a failed reconnect. A release is marked uncertain before sending its
request: a missing reply cannot cause another decrement. Changed library or
member identities, missing bytes, insufficient counts or an uncertain release
leave the group Unresolved and retain the receipt and immutable bytes. Revert
cannot discard that evidence while ownership is unknown. The GUI skips cleanup
for acquisitions the provider owns, avoiding a second release of deduplicated
members. Retained receipts live in memory; closing an unresolved session can
leave an orphan on the host rather than risk deleting another record's data.

This group requires a single active ordinary record session. Each file must
prove Full Access or a completely decoded static custom set. Custom recovery
retains the opaque Login account and assigned-set references, account name,
own decoded rules, schema and routes. The schema proof includes Root's separately
read custom functions, value lists and data-source catalogs in eager and on-demand
mode. A failed optional metadata read is unknown, not an empty catalog.
Before rollback it records Create/Edit
requirements and caller-written fields for current and parked records.
Later accepted inputs must extend that footprint under the original proof.
Generated serial, clock and UUID values are carried separately; they grant no
permission to edit a field. Relevant tables must be wholly readable, actual
caller fields modifiable and the originating own layouts authorized. Fresh
sessions must match the same identities, rules and schema, and every file
must pass before any replacement reservation. The mutable serial counter is
issued state, excluded from schema equality.

Custom sets with conditional rules anywhere, withheld values, missing identity
or rule metadata, ambiguous input provenance or changed rules remain Unresolved.
Uploaded acquisitions can travel under proven static custom permissions using
the same byte/identity/count receipt as Full Access. All participants' proofs
precede corrections, count adjustments, replacement reservations and ownership
transfer. Revert and current or parked window cleanup repeat those checks before
any owner's release; failed proof detaches and retains the receipt rather than
letting transaction cleanup decrement references. A held stored reference with
no owned acquisition grants no release authority. Recovery does not copy stored
sources or accept a stale source (1225), add file-library reference forms or
upload assets again. An unused container field does not prevent scalar recovery.
Script transactions, deletions and groups whose record data may have been
submitted remain refused.
These are OpenMaker recovery choices, tested with synthetic OpenMaker Server
files; FileMaker Pro's behavior for this lost-connection case is unmeasured.

Not covered: persistent crash recovery (edits live only in the running
window), edits scripts left uncommitted (their own fields on an existing
record, a new record a reconnect cannot carry, related changes outside the
proved refused ordinary group), related records deleted and not saved,
transactions, a save of related changes that may have sent the host
record data before the loss, and re-attaching the lost session with the
protocol's `Reconnect`
(6.3).

## Saving over another client's change

FileMaker Pro locks a record on the host when a user begins editing it,
and a second editor gets 301 ([write-path.md](re/write-path.md) 4.1).
OpenMaker locks the record only to commit a save (write-path.md 4.2), so a
conflict shows when the host refuses the save, and the window settles it
with the reconnect's workflow above.

**What an edit answers to.** When a window begins unsaved edits of a record
(typing, emptying a field, or a draft a script left), it keeps the record's
values as it shows them, and the provider keeps the version that window
last read of the record (its page reads, reads by id and writes) as the one
the save is checked against. Another window's reads, the window's own
reload and another client's change leave both. A save, a read by id after a
refusal, or giving the edits up (Revert Record, Discard My Changes, a
script committing them, closing the window) settles or releases the
window's version, and only that window's: another window editing the same
record keeps its own.

**A changed or deleted record.** A save the host refuses because the record
changed since the edits began (306) or is gone (101) shows no alert. The
edits stay in the window, held, and the record is read by id and compared
with the values the window showed when they began, as after a reconnect:
edits the host's copy still matches are saved by the user's next Save
Record; edits the host already holds are no longer pending; otherwise the
bar, headed **Your unsaved changes are on hold.**, offers **Keep My
Changes** (when the host changed the record) and **Discard My Changes**,
and nothing saves the edits or moves on until the user chooses. **Check
Again** reads a record that could not be read; a deleted record's edits can
only be discarded. Kept edits answer to the version read by id: a further
change on the host refuses their save and holds them again.

**A record another user is editing.** A save refused because another
client holds the record (301, or 300 for the file) leaves the edits as they
are, not held, with the host's reason in the window's bar instead of an
alert. Nothing retries by itself: **Save Record** tries again, **Revert
Record** gives the edits up. The retry answers to the version and values
the edits began from, whatever the window reloaded meanwhile: when the
other user changed the record before letting go, it is refused and held
as above, never saved over their change.

**Which record a command acts on.** A window's record is its record id,
not its position in the found set. When another client deletes a record
before it, the records after it move up, and the next command that reads
the record (a script step, a button, a menu command run as a step, a
step's calculation) finds it by id, acts on it, and the window shows it at
its new number. When another client deleted the record itself, the
command fails with 101 (Record is missing) and acts on no record, whether
or not the window has unsaved edits. Records > Delete Record deletes the
record shown by its id, as before. A record this client deletes, in any
window, or creates in a transaction that is reverted or fails to commit,
is let go by every window that showed it: they show the record that takes
its place, unless they hold unsaved edits of it. FileMaker
Pro's behavior after another client's deletion is unmeasured
([client-ui.md](re/client-ui.md), Unknowns); failing with 101 rather than
acting on the next record is OpenMaker's choice.

A refusal while a script runs (its own writes, a transaction it opened, a
save of changes scripts left uncommitted) keeps its alert and holds
nothing. Not covered: locking the record when an edit begins, as FileMaker
Pro does (another client can still change the record until the save), and
merging changes to different fields without asking.

### Edits from scripts and the user together

UI typing added to a record with pending script changes retains its own accepted
page version. Worker-supplied drafts alone do not create an unknown UI pin.
A refused mixed commit keeps the original UI values and enters conflict recovery;
any version conflict requires an explicit Keep even if only a repetition or related
cell changed and the visible scalar values match. Discard reverts through the
owning worker so pending script cells, new records and container references go too.
If deleting a script-created record is refused, its drafts and hold stay so
Discard can be retried. Once deletion is confirmed, a later refresh failure
does not restore the deleted record's pending changes.
