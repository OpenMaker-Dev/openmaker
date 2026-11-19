# s35 findings: portals, windows, OnWindowTransaction, transaction validation

FileMaker Server 26.0.3's script engine run through `tools/fms-script-harness`
(black box: the engine's step errors, Get selectors and the stored records).
Batteries `batteries/s35-*.txt`, raw outputs `results/s35-*.tasks-en.tsv`
(every run line, then `>` trace lines with each step's own error; `!` lines are
the server event log). All files are generated from the Tasks template's
saveAsXML (`tasks-en`) with the battery's schema directives (README), because
FileMaker enforces relationship, portal and validation options only in files
FMUpgradeTool generated.

The test layout is Task Details (occurrence Tasks). It stacks the Attachments
portal (object id 9, named "Attachments Portal" by the battery; one row field
Attachments::Description named "att desc"; relationship Tasks::PrimaryKey =
Attachments::ForeignKey, "allow creation" on the Attachments side, no cascade
delete; portal options word 257; 3 visible rows) before the "Assignee Portal"
(occurrence Assignments, no creation, empty in every probe). Fields outside
the portals include the object "Task" (Tasks::Task). "Attachment List" is a
layout on Attachments the battery adds.

State notation (s35-portals and its variants): `[e:row:field:fieldTO:object:rep:rec:task:desc:layout:found]` =
the error of the step before (Get ( LastError )), Get ( ActivePortalRowNumber ),
Get ( ActiveFieldName ), Get ( ActiveFieldTableName ),
Get ( ActiveLayoutObjectName ), Get ( ActiveRepetitionNumber ),
Get ( RecordNumber ), Tasks::Task, Attachments::Description (evaluated in the
window: the active row's record, else the first related one), Get ( LayoutName ),
Get ( FoundCount ). The state is read in a sub-script ("st"); `inline t3` shows
the same values read in the probe itself, so Perform Script does not disturb
the active row or field.

## A. Portal rows (s35-portals, -nocreate, -valid, -validentry, -word0/1/256/273)

### A1. Go to Portal Row with no active portal

No active portal (fresh window on a task): `[0:0::::0:...]`: row 0,
ActiveFieldName "", ActiveFieldTableName "", ActiveLayoutObjectName "",
ActiveRepetitionNumber 0.

| step (task with 2 rows, t2) | error | state after |
|---|---|---|
| Go to Portal Row [ First ] | 0 | row 1, field Description, TO Attachments, object "att desc", repetition 1 |
| [ Last ] | 0 | row 3 (the empty creation row, see A2), field Description |
| [ Next ] | 0 | row 1 (as First) |
| [ Previous ] | 101 | nothing active (row 0, no field) |
| [ First ; Select: On ] | 0 | row 1, ActiveFieldName "", ActiveFieldTableName "", object "Attachments Portal", repetition 0 |

So with no active portal the step uses the first portal in stacking order
(Attachments, not the Assignee portal), and with the focus on a field outside
every portal (Go to Object [ "Task" ] then [ Next ]) also row 1 of the first
portal. Select Off puts the focus into the row's first field (the portal's
only field object here); Select On makes the portal row itself active: no
active field, the active object is the portal, repetition 0.
Get ( RecordNumber ) is the parent's record number throughout.

### A2. Moves by related-record count, By calculation, Exit after last

The creation row: when the relationship allows creation and the portal scrolls
(options bit 0x1, see A5), the portal has one more row than related records,
the empty row after the last record (Attachments::Description reads "" there).
With creation off (`-nocreate`) or a portal that does not scroll (word 0,
which caps the reachable rows at the 3 visible ones) there is no such row.

| related records | First | Last | Next at the last row | Previous at row 1 |
|---|---|---|---|---|
| 0 (creation on) | 0, row 1 (creation row) | 0, row 1 | 101, stays 1 | 101, stays 1 |
| 1 | 0, row 1 | 0, row 2 | 101, stays 2 | 101, stays 1 |
| 2 | 0, row 1 | 0, row 3 | 101, stays 3 | 101, stays 1 |
| 3 | 0, row 1 | 0, row 4 | 101, stays 4 | 101, stays 1 |
| 0 (creation off) | 101, nothing active | 101, nothing active | | |
| 1 / 3 (creation off) | 0, row 1 | 0, row 1 / row 3 | 101, stays | 101, stays |

A failing move keeps the active row and field (Next/Previous 101 above);
Previous with nothing active returns 101 and activates nothing.

By calculation (task with 3 records, creation on, so rows 1-4):

| n | error | row |
|---|---|---|
| 1, 2, 3 | 0 | 1, 2, 3 |
| 4 (count + 1, the creation row) | 0 | 4 |
| 9 (past the last row) | 101 | nothing active (from row 2: stays 2) |
| 0, -1 | 101 | nothing active (from row 2: stays 2) |
| 1.5, 2.9 | 0 | 1, 2 (truncated, not rounded) |
| "2" (text) | 0 | 2 |
| "abc", empty | 101 | nothing active (from row 2: stays 2) |
| 1 on 0 records, 2 on 1 record | 0 | 1, 2 (creation rows) |
| 4 with creation off | 101 | nothing active |

Exit after last, in a Loop (6 passes at most): Go to Portal Row [ Next ; Exit
after last: On ] at the last row returns 101 and leaves the loop; the 101 stays
the script's LastError, so the script ends with error 101 (the run line's
`error` column; RunScriptAndClose returns it) even though the Exit Script
after the loop runs. "Last row" is the creation row when there is one: 0
records leave at pass 1, 2 records at pass 3 (rows 1, 2, 3), 3 records at pass
4. Without Exit after last the loop keeps running: Next returns 101 each pass
and the row stays at the last one (4).

Select On with moves (`seq select t3`): First/Next/Previous with Select On give
rows 1, 2, 3, 2 with no active field and object "Attachments Portal"; a
following Go to Portal Row [ Last ; Select: Off ] gives row 4 with field
Description, object "att desc".

### A3. Go to Object, Go to Field

Go to Object [ name ] on a task with 2 records (t2):

| object | error | state | then Go to Portal Row [ Next ] |
|---|---|---|---|
| "Attachments Portal" (the portal) | 0 | row 0, no field, object "Attachments Portal", repetition 0 | 0, row 1, field Description |
| "att desc" (field in the portal) | 0 | row 1, field Description, object "att desc", repetition 1 | 0, row 2 |
| "att desc", Repetition 1 | 0 | as without | |
| "att desc", Repetition 2 (field has 1 repetition) | **102** | nothing active | |
| "Task", Repetition 2 | **102** | nothing active | |
| "Assignee Portal" (empty, no creation) | 0 | row 0, object "Assignee Portal" | 101, stays |
| "assignee name" (field in that empty portal) | **101** | row 0, object "Assignee Portal" (the portal becomes active) | 101 |
| "Task" (field outside portals) | 0 | row 0, field Task, TO Tasks, object "Task", repetition 1 | 0, row 1 of the Attachments portal |
| "no such" | **116** (Layout object is missing) | nothing active | 0, row 1 |
| "" (empty name) | **5** (Command is invalid) | nothing active | 0, row 1 |
| "att desc" on a task with 0 records, creation on | 0 | row 1 (creation row) | 101 |
| "att desc" on 0 records, creation off | **101** | object "Attachments Portal" active, row 0 | 101 |
| "Attachments Portal" on 0 records, creation off | 0 | row 0, portal active | 101 |

From an active row 2: Go to Object [ "att desc" ] keeps row 2; [ "Attachments
Portal" ] keeps row 2 with the portal active and no field; [ "Task" ] leaves the
portal (row 0; Attachments::Description reads the first related record again).

Go to Field (t2, then t3 from row 2):

| step | error | state |
|---|---|---|
| Go to Field [ Attachments::Description ] (no active row) | 0 | **row 1**, field Description, object "att desc" |
| then Go to Portal Row [ Next ] | 0 | row 2 |
| Go to Field [ Select/perform ; Attachments::Description ] at row 2 | 0 | stays row 2 |
| Go to Field [ Attachments::Description[2] ] | 102 | nothing active (the failed step clears the focus) |
| Go to Field [ Tasks::Task ] | 0 | row 0, field Task, object "Task" |
| Go to Field [ Attachments::PrimaryKey ] (not on the layout) | 102 | nothing active |
| Go to Field [ Assignees::Name ] (field in the empty Assignee portal) | 101 | nothing active |
| Go to Field [ Attachments::Description ] at row 2 (`fld from 2`) | 0 | stays row 2 |
| Go to Field [ Tasks::Task ] then Go to Portal Row [ Next ] | 0, 0 | row 0, then **row 1** (the row is not remembered) |
| Go to Field [ Attachments::Description ], 0 records, creation off | 101 | nothing active |

### A4. Set Field [ Attachments::Description ] through the portal

`sf` returns the states before and after Set Field, the related records'
descriptions and count right after it, Commit Records' error, then Revert
Record's error and the records again; SQL counts are `ExecuteSQL` in the same
session. Value "<task>+".

| case | Set Field error | which record | after Commit (0) and Revert |
|---|---|---|---|
| (a) no active row, 2 records | 0 | the **first** related record (record 1 changed) | kept |
| first / (b) row 2 of 3 | 0 | the active row's record (record 1 / record 2) | kept |
| (e) Go to Portal Row [ Last ] (creation row 3), 2 records | 0 | **a new related record** (3 records) | kept, ForeignKey = the task's PrimaryKey |
| (e) By calculation count + 1 (row 3), 2 records | 0 | a new related record | kept |
| (c) no related record, creation on (no active row) | 0 | a new related record (1) | kept, ForeignKey matches |
| 0 records, [ Last ] / By calculation 1 (creation row 1) | 0 | a new related record | kept |
| (d) no related record, creation off | **101** | nothing created (0) | commit 0 |
| (d) creation off, [ Last ] on 0 records | Go to Portal Row 101, Set Field 101 | nothing | |
| creation off, [ Last ] on 2 records | 0 | record 2 (the last row is a record) | kept |
| creation off, By calculation count + 1 | Go to Portal Row 101, Set Field 0 | the **first** related record (the failed move left no active row) | kept |
| Set Field then Revert Record (no commit): edit of row 1 / row 2 / a created record | 0 | | reverted: the edit is undone, the created record is gone |

- Set Field By Name [ "Attachments::Description" ; ... ] (`sfbn`) targets the
  same record as Set Field: the active row's record (row 2), with no active
  row the first related record; a name that is not a field gives 102.
- The created record's match field holds the parent's key (SQL `ForeignKey = Tasks::PrimaryKey` counts 1 after the commit; it sits in the portal), its
  PrimaryKey auto-enter has run (length 36 before the commit).
- Related edits and created records belong to the parent's open record:
  Commit Records commits them (error 0) and Revert Record discards them. Commit
  Records leaves the portal: the active row is 0 after it.
- ExecuteSQL in the same session already sees the uncommitted related value
  (`sql 1` before the commit, `sql 0` after a revert).
- Count quirk (`cnt`, `sf count+1`): an aggregate over a related field
  evaluated once on a record keeps its value until the commit. Count (
  Attachments::PrimaryKey ) was 2 before the creation row was filled and stayed
  2 afterwards while Count ( Attachments::Description ), ValueCount ( List (
  Attachments::PrimaryKey ) ) and Count ( Attachments::ForeignKey ), not
  evaluated before, gave 3; after Commit Records all give 3. Where no Count
  was evaluated before (`sf last`) it gave 3 at once.

Validation of the related field (`-valid`: strict, Always, `Length ( Self ) <=
5`; `-validentry`: the same rule, only during data entry; identical results):

| case | Set Field | Commit Records | stored after Revert |
|---|---|---|---|
| invalid "toolong", no active row (record 1) | 0 | **507** | record 1 unchanged |
| invalid, row 2 | 0 | **507** | unchanged |
| invalid, creation row (new record) | 0 | **507** | no new record |
| invalid, no related record (created) | 0 | **507** | none created |
| valid "ok" | 0 | 0 | stored |

The related record is validated when the parent commits, not at Set Field;
one failing related record fails the whole Commit Records with 507 and nothing
is committed (the record stays open with the invalid value until the revert).
"Only during data entry" applies to Set Field in a server script exactly as
Always.

### A5. Delete Portal Row

| case | error | result |
|---|---|---|
| no active row, 2 records | **3** | nothing deleted ("nothing happens", but error 3) |
| no active row, 0 records | **3** | |
| row 2 of 3, With dialog Off | 0 | record 2 deleted at once (the portal shows 2 records); afterwards **no active row** (row 0, no field); Commit Records 0, the delete persists |
| row 2 of 3, With dialog On | 0 | the same (no dialog on the server) |
| row 4 = the creation row of 3 records | 0 | nothing deleted, no active row |
| creation off, row 3 of 3 (the last record) | 0 | record 3 deleted |
| row 2, then Revert Record instead of a commit | 0 | **restored**: Revert Record brings the deleted row back (3 records) |
| Delete Record of the parent, cascade delete off | 0 | its 2 attachments stay (orphans) |
| Delete Record of the parent, cascade delete on (Attachments side, `-nocreate`) | 0 | its attachments are deleted (SQL count 0) |

Portal options word (`-word<n>`; the template has 257, Contacts' portals 273):
Delete Portal Row of row 2 deletes with every word tried (0, 1, 257, and see
the results for 256 and 273), so the server's Delete Portal Row does not check
an "allow deletion" bit in this word, or none of these bits is it. Bit 0x1
makes the rows past the visible ones reachable (word 0: rows 1-3 only, Next at
3 gives 101 and there is no creation row 4; word 1: rows 1-4 as 257): the
"allow vertical scrolling" option.

### A6. Go to Related Record from a portal row

GTRR [ From table: "Attachments" ; Using layout: "Attachment List" ] on a task
with 3 records:

| active row | Show only related | error | result |
|---|---|---|---|
| none | On | 0 | Attachment List, found 3, record 1 (the first related record) |
| row 2 | On | 0 | found 3, **record 2** (the active row's record) |
| none | Off | 0 | the window's existing Attachments found set (6 here) kept, current record = the first related record (record 4) |
| row 2 | Off | 0 | existing found set (9) kept, current record = the active row's record (record 8) |
| none, 0 records | On | **101** | stays on Task Details |

With Show only related records Off the step does not replace the found set
(it was the window's Attachments found set from the run line before plus the
records this probe made); it only moves to the related record.

### A7. The active row after other steps

`after a7` (row 2 active on a task with 3 records):

| step | error | active row after |
|---|---|---|
| Commit Records | 0 | 0 (no row, no field) |
| Go to Record [ Previous ] onto a task with **0** records | **101** | 0; the record did change (record 27 of 28) |
| Go to Record [ Next ] back | 0 | 0 |
| Go to Layout [ "Attachment List" ] then [ "Task Details" ] | 0, 0 | 0 |
| Go to Layout [ "Task Details" ] (the current layout) | 0 | **2** (kept) |

`after record a8` (both tasks with 3 records): Go to Record [ Previous ] from
row 2 gives row 2 on the previous record (error 0); Go to Portal Row [ 3 ] then
Go to Record [ Next ] gives row 3 on the next record; Previous again keeps 3.
So Go to Record keeps the active row number on the new record; when the new
record has no such row (0 records: only the creation row 1) the step returns
101 after moving and leaves no row active.

## B. Windows (s35-windows)

State notation: `[e:WindowName:WindowNames ( Get ( FileName ) ):LayoutName:FoundCount:RecordNumber:WindowMode:Tasks::Task]`
(WindowNames comma-separated). The session's own window is named after the
file, "work" (the harness's copy is `work.fmp12`). Five tasks a-e.

### B1. New Window

From W1 on Task Details, record 2 (b):

| step | error | new window's state |
|---|---|---|
| New Window [ Name: "W2" ] (no layout: "original layout") | 0 | `W2`, names `W2,work`, Task Details, found 5, record 2 (b), Browse: a copy of the current window's layout, found set and record |
| New Window [ ] (no name) | 0 | `W2 - 2` (the current window's name + " - 2"), names `W2 - 2,W2,work` |
| New Window [ Name: "W2" ] again | 0 | a second `W2`: duplicate names are allowed (`W2,W2 - 2,W2,work`) |
| New Window [ Name: "W5" ; Layout: "Assignments" ] | 0 | Assignments, found 0, record 0 (that table has no records) |
| New Window [ Name: "W6" ; Layout: "Task List" ] | 0 | Task List, found 5, record **1**; the layout's OnLayoutEnter ("Sort by Status") runs as a sub-frame right after New Window, before the next step, and the sort leaves record 1 current |

WindowNames lists the windows front to back (the current one first).

### B2. Per-window state

`perwin`: W1 on record 2; New Window W2 (found 5, record 2); in W2 Omit Record
(found 4, record 2 = c) and Go to Record [ Next ] (record 3 = d); Select Window
[ W1 ] (error 0): **W1 found 5, record 2 (b), Browse, Task Details: unchanged**;
Select Window [ W2 ]: found 4, record 3 (d) as left. Enter Find Mode in W2: W2
mode 1, FoundCount 1 (the one request), record 1; Select Window [ W1 ]: W1 mode
**0** (Browse), found 5, record 2; back in W2: still Find mode.

`perwin find`: W1 on record 5; New Window W2, find Task = d: W2 found 1 (d);
Select Window [ W1 ] (no "current file"): W1 found 5, record 5 (e).

Found set, current record, layout and mode are per window.

### B3. Close Window, Select Window

`close`: W1 (record 1), W2 (Go to Record [ Last ], record 5), W3 (record 4);
Select Window W1, then W2:

| step | error | current window afterwards |
|---|---|---|
| Close Window [ Current Window ] (W2) | 0 | **work (W1)**, the window that was current before W2 (not the newest, W3); names `work,W3`; W1's state: found 5, record 1 |
| Close Window [ Name: "W3" ; Current file ] | 0 | work |
| Close Window [ Name: "nosuch" ; Current file ] | **112** | work (unchanged) |
| Select Window [ Name: "nosuch" ; Current file ] / [ Name: "nosuch" ] / [ Name: "" ] | **112** | unchanged |
| Close Window [ Name: <W1's name> ; Current file ] while W2 is current (`close by name`) | 0 | W2; then Select Window [ W1 ] gives 112 |
| Close Window [ Current Window ] on the only window (`close last`) | **13** (File or object is in use) | the window is gone (Get ( WindowName ) "" after the script); **the script stops** at that step: the steps after it never run, the run returns error 0 and an empty result, `$$` variables set before it survive |

### B4. Window selectors

`{name,WindowVisible,WindowStyle,WindowTop,WindowLeft,WindowHeight,WindowWidth,WindowZoomLevel,WindowContentHeight,WindowContentWidth,WindowDesktopHeight,WindowDesktopWidth,WindowOrientation,ScreenHeight,ScreenWidth,Length ( WindowUUID )}`:

| window | values |
|---|---|
| W1 (the session window) | `work,1,0,0,0,480,640,100,480,640,480,640,-1,480,640,32` |
| W3, New Window [ Height: 400 ; Width: 600 ; Top: 10 ; Left: 20 ] | `W3,1,0,10,20,400,600,100,400,600,480,640,-1,480,640,32` |
| W1 again | as before |

The server's windows are 640 x 480 at 0, 0 on a 640 x 480 desktop and screen;
New Window's bounds are taken as given (top, left, height, width) and the
content area equals the window. Style 0 (Document), zoom 100, orientation -1,
visible 1. Get ( WindowUUID ) is 32 hex digits without dashes
(`6507A62443944D62B886C83A1733B60B`), different per window and stable while
the window lives. After the script the run line still sees both windows:
WindowNames ( "" ) and WindowNames ( "work" ) both give `work¶W3`.

## C. OnWindowTransaction (s35-wintx, -field, -option)

The trigger fires on the server engine with the harness's file-trigger rule
(fmsased's; `FILE_TRIGGERS=base` was not needed). Script "otx" appends
`{Get ( ScriptParameter )}` to $$tx.

### C1. What fires it (s35-wintx)

| operation | step error | firings, parameter entries |
|---|---|---|
| Commit Records of a new record | 0 | 1: `["New", <record id>, ""]` |
| Commit Records of an edit | 0 | 1: `["Modified", id, ""]` |
| Commit Records with nothing changed | 0 | **none** |
| Set Field of the same value, then Commit Records | 0 | 1: `Modified` |
| Delete Record | 0 | 1: `["Deleted", id, ""]` |
| Delete All Records (found set of 2 attachments) | 0 | **1** firing with both: `"Attachments" : [ ["Deleted",1,""], ["Deleted",2,""] ]` |
| Commit Transaction: an edit of task 6, a new task 7, a new attachment 3 | 0 | **1**: `"Attachments" : [ ["New",3,""] ], "Tasks" : [ ["Modified",6,""], ["New",7,""] ]` |
| Commit Transaction of a Delete Record | 0 | 1: `Deleted` |
| Delete Portal Row, then Commit Records | 0, 0 | 1: `"Attachments" : [ ["Deleted",4,""] ]` (the parent is not listed) |
| Set Field into a related field without related records (creates one) and of a parent field, Commit Records | 0 | 1: `"Attachments" : [ ["New",6,""] ], "Tasks" : [ ["Modified",10,""] ]` |
| Revert Record of an edit and of a new record | 0 | none |
| Revert Transaction, then Commit Transaction | 0 | none |
| Commit Transaction failing validation (507) | Set Field 0, Commit Transaction **507** | none |
| Commit Records failing validation | **507** | none |
| two Commit Records in one script | 0, 0 | 2, one per commit, in commit order |

Every commit inside the probes' setup sub-script fired as well (those
entries precede the probe's own in $$tx); record ids are the tables' record
ids (Get ( RecordID )), and a reverted new record still uses up an id.

### C2. The parameter

Exact text of the first firing (the run line's escapes: `\r` return,
`\t` tab), FileMaker's formatted JSON:

```
{\r\t"work" : \r\t{\r\t\t"Tasks" : \r\t\t[\r\t\t\t[\r\t\t\t\t"New",\r\t\t\t\t1,\r\t\t\t\t""\r\t\t\t]\r\t\t]\r\t}\r}
```

- The file key is the file name without extension ("work"); then one key per
  base table (base table name, not occurrence), keys in alphabetical order
  (Attachments before Tasks although Tasks is table 130 and Attachments 131),
  each an array of `[operation, record id (number), field contents]` entries
  in the order the records were changed.
- Operations `"New"`, `"Modified"`, `"Deleted"`.
- Field contents (`-field`: a text field named OnWindowTransaction in Tasks,
  none in Attachments; no field name option):

  | field value | third element |
  |---|---|
  | `hello` | `"hello"` |
  | `{"a":1,"c":[true,null]}` | the object itself, re-formatted: `{ "a" : 1, "c" : [ true, null ] }` |
  | `[1,2` (invalid JSON) | `"[1,2"` |
  | `42` | `"42"` (a string, not a number) |
  | empty | `""` |
  | a table without the field (Attachments) | `""` |
  | Delete Record of a task whose field holds `deleted value` | `"deleted value"` (read before the delete) |

- With the trigger's field name option set to `Description` (`-option`, no
  field named OnWindowTransaction): the same table, with each table's
  Description as the contents: Tasks' `hello`, the object, `"[1,2"`, `""`,
  `"42"` as above; in the transaction the new attachment's Description
  `{"b":2}` is embedded as the object `{ "b" : 2 }` and the task's as
  `"task value"`; the deleted task's `"deleted value"`. The option names one
  field name looked up in every base table.

### C3. When it runs

The trigger does not run at the commit: the step after Commit Records reads
$$tx unchanged ("seen" is empty in every probe) and Get ( LastError ) after
the commit is 0. Each commit queues the trigger script **below** the running
script: from the step after the first commit on, the probe's own steps run at
depth 2, after a second commit at depth 3 (the trace's depth column counts the
queued frames). When the probe (with its callers) finishes, the queued
trigger scripts run, the first commit's first, each with its own parameter;
the run line's expressions afterwards see all of them ($$txn = the number of
commits). Commits in a sub-script queue the same way (the setup sub-script's
firings ran after the top script). Get ( LastError ) evaluated after
everything is 0.

## D. Validation timing inside a transaction (s35-txvalidation)

Task a; Tasks::Description strict by calculation `Length ( Self ) <= 3`
validated Always ("D"), Tasks::Category the same rule only during data entry
("C"); Set Field to "toolong" after Open Transaction, then the leaving step,
then Commit Transaction. Values are each step's Get ( LastError ) right after
it (the trace agrees: no other step returns an error).

| field | leaving step | Set Field | leaving step | Commit Transaction | stored afterwards |
|---|---|---|---|---|---|
| D (Always) | Commit Records | 0 | **0** (record 1) | **507** | ok/ok: everything reverted |
| D | Go to Record [ Next ] | 0 | **0** (moves to record 2) | **507** | reverted |
| D | none | 0 | | **507** | reverted |
| C (data entry only) | Commit Records / Next / none | 0 | 0 | **507** | reverted |
| D or C, Set Revert Transaction on Error [ Off ] | as above | 0 | 0 | 507 | reverted |
| D, Open Transaction [ Skip data entry validation ] | Commit / Next / none | 0 | 0 | **507** | reverted |
| C, Open Transaction [ Skip data entry validation ] | Commit Records | 0 | 0 | **0** | **committed** (ok/toolong) |

- Inside a transaction the step that leaves the record (Commit Records, Go to
  Record) does not validate: it returns 0 and moves on; validation happens at
  Commit Transaction, which returns 507, rolls the whole transaction back and
  closes it (Get ( TransactionOpenState ) 1 before, 0 after; the step after
  Commit Transaction runs normally with LastError 507).
- Set Revert Transaction on Error makes no difference here because no step
  before Commit Transaction fails.
- Open Transaction's "Skip data entry validation" skips the rules validated
  only during data entry; rules validated Always still fail the commit (507).
- A data-entry-only rule applies to Set Field in a server script (C fails
  like D without the skip option).

Without a transaction (`plain`): Set Field 0; Commit Records **507**; Go to
Record [ Next ] **507** and it stays on record 1; Commit Records [ Skip data
entry validation ] of the Always rule **507**; C with Commit Records or Next
507. After a Revert Record nothing is stored.

## Questions the server engine could not answer

Each needs FileMaker Pro (the probe is the same script run from the Script
Workspace or a button in a window of the generated file: `runbat.sh` leaves
it as `$WORK/<battery>.tasks-en.gen.fmp12`, account Admin without a
password, no records; run the battery's `setup` first where it has one).

1. **Which portal option bit is "Allow deletion of portal records"**, and
   whether it gates the Delete Portal Row step at all. The server's step
   deleted with words 0, 1, 256, 257 and 273. FileMaker Pro: generated
   `s35-portals-word0` and `-word273`; on Task Details, task t3, run
   `Go to Portal Row [ By calculation: 2 ; With dialog: Off ]`,
   `Delete Portal Row [ With dialog: Off ]`, `Exit Script [ Get ( LastError ) & ":" & List ( Attachments::Description ) ]`;
   then also try deleting a row by hand (select the row, Records > Delete
   Record shows "Delete portal row?" only when deletion is allowed); and open
   Portal Setup of the Attachments portal in each file to read which boxes
   map to 0x10 and 0x100 (0x1 is "Allow vertical scrolling" by the server's
   behavior).
2. **Focus and selection in a real window** after Go to Portal Row with
   Select On/Off, Go to Object of a portal, and a failed Go to Field: the
   server reports the active field/object; FileMaker Pro should be checked
   for the same Get values plus Get ( ActiveSelectionStart ) /
   Get ( ActiveSelectionSize ). Probe: `s35-portals` scripts `seq select`,
   `obj`, `fld` with parameter t3, from a button on Task Details, returning
   `$$s`.
3. **Whether Go to Record keeps the active portal row in FileMaker Pro**
   (the server keeps the row number and returns 101 after moving when the new
   record lacks that row): `after record` and `after` with parameter a9 in
   FileMaker Pro.
4. **The aggregate cache** (`cnt`: Count ( Attachments::PrimaryKey ) stays 2
   after a related record is created through the creation row, until the
   commit): run `cnt` with parameter cnt2 in FileMaker Pro; and whether a
   portal redraw refreshes it.
5. **Dialogs**: Delete Portal Row [ With dialog: On ] and Go to Portal Row
   [ By calculation ; With dialog: On ] show nothing on the server (step
   error 0); in FileMaker Pro run `del` with "row2 dialog" (expect the
   confirmation dialog; Cancel's error, presumably 1).
6. **OnWindowTransaction timing in FileMaker Pro**: on the server the trigger
   is queued below the running script and runs after it. In FileMaker Pro run
   s35-wintx's `two commits` (parameter tc) from the Script Workspace with the
   debugger open and check whether "otx" starts after the script ends or at
   idle, and whether a commit made by the user (no script) fires it at once.
7. **Window geometry and style values in a desktop client**: the server's
   windows are fixed 640 x 480 at 0, 0; FileMaker Pro's values for `selectors`
   (and Get ( WindowStyle ) for card/floating windows, which the server
   builder does not make) need FileMaker Pro.
8. **Closing the last window in FileMaker Pro** (server: Close Window
   returns 13 and the script stops): `close last`.
9. **Revert of a Delete Portal Row**: the server's Revert Record restores a
   deleted portal row. FileMaker Pro: `del revert` with parameter dr2 (Claris
   says the step cannot be undone; Undo and Revert Record may differ).
