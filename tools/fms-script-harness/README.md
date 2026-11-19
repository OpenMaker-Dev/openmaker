# fms-script-harness: FileMaker Server's script engine as a black-box oracle

A research tool for `docs/re/script-runtime.md`. It runs FileMaker scripts
with FileMaker Server 26.0.3.309's own script engine (`libFMEngine.so` on
`libDBEngine.so`/`libSupport.so`, Ubuntu build) in the network-less amd64
container of `tools/fms-calc-harness`, the way the server's script engine
process runs a schedule, and reports each run's result, error, variables
and a trace of every executed step with its error. Probe scripts are added
to a private copy of a sample file with FileMaker's own tools. Observations
are runs of the real engine, not code reading. No FileMaker code is copied:
`scriptrun.cpp` only declares exported entry points (names and ABI from the
symbol table and DWARF) and wraps a few of them.

## Run

```sh
docker build --platform linux/amd64 -t om-fms-calc-harness tools/fms-calc-harness   # once
export FMS_ROOT=/path/to/re/extracted/fms/deb/opt/FileMaker   # unpacked .deb
export FILES=/path/to/scratch/files                          # .fmp12 copies
export HARNESS_LOCK=/path/to/re/work/fms-harness/.run.lock   # shared with fms-calc-harness
cp ".../DynamicTemplates/Contacts/en.fmp12" "$FILES/contacts-en.fmp12"
tools/fms-script-harness/runbat.sh tools/fms-script-harness/batteries/s01-lasterror.txt contacts-en.fmp12 /tmp/results
tools/fms-script-harness/runbat.sh -l /dev/null contacts-en.fmp12 /tmp/results   # list the file's scripts
# (folders and separators are listed with a leading \uE001 / \uE002)
```

`runbat.sh [-l] <battery> <file> [results dir]` writes
`<battery>.<file>.tsv` (and `.stderr`). Environment: `FMS_ROOT`, `FILES`
(mounted read-only), `WORK` (writable scratch for the file's saveAsXML
schema, generated patches and the patched copies; default
`script-harness-work` next to `$FILES`), `ACCOUNT` (a full access account
of the file as `name:password`, default `Admin:`, needed by FMDeveloperTool
and FMUpgradeTool), `HARNESS_LOCK`, `STEP_LIMIT` (steps per run before the
harness stops a runaway script, default 20000), `FILE_TRIGGERS=base` (see
below), `BEFORE` (a command started in the background in the container before
the run, such as a loopback HTTP server: the container has no other network); `runbat.sh` passes the last two into the container.
`DOCUMENTS` (a writable directory) is mounted as the Documents folder of the
container's root user, `/root/Documents`, and sets `SASE_EXPORT`, under which
Export Records runs as on FileMaker Server (below). Run one
container at a time: `runbat.sh` takes the machine-wide lock the calc
harness uses. A battery of a dozen scripts takes about 6 seconds.
FileMaker's script frames take tens of kilobytes each: deep recursion
probes need a large parameter, so that its 1 GiB guard stops them at a
few thousand frames (`s11`); without one a probe can exhaust the
machine's memory before the guard acts.

## What runs

Start-up as fmsased's `FMScriptMgr::OnInit`: `DBEngineController`,
`ApplicationInfo::SetApplicationInfo("", "Server 26.0.3", 7)` (the script
engine's values, so `Get(ApplicationVersion)` is `Server 26.0.3`),
`Start()`, `InstallDesignFunction()`. Each run line then does what
`FMScriptThread::dispatchCallBack` does for a schedule: a new
`Draco::SMSession(true, auth)` and
`SMSession::RunScriptAndClose(file, auth, script name, parameter, &result)`,
which opens the file in that session (`DBSession::OpenFileUser`), opens its
window (`FMSession::OpenWindow`, then `CompleteScript` runs the
OnFirstWindowOpen script it queued), runs the script to completion
(`SMScriptRuntime::ExecuteScript`: `CallScript` with state 0, then
`CompleteScript`) and closes the file (s10 shows OnWindowClose running
there). So every run line is a fresh server session like
one PSOS or schedule: `$$` variables do not survive to the next line, while
record changes do (all lines of a battery use one scratch copy). The
account is FMDeveloperTool's security bypass (`kSecurityByPass`): it
reports account `Admin` and privilege set `[Read-Only Access]` but has full
access; `@account` logs in with a real account instead.

Interposed functions (exported from the executable only, run.sh; each calls
the original through `RTLD_NEXT`):

| function | why |
|---|---|
| `ScriptRuntimeBase::Execute()` | records frame depth, script, line and step id before each executed step; applies `@call` after it |
| `ScriptRuntimeBase::PostStepCleanup(DBError&, bool handled, bool control)` | the step's own error and flags; LastError after cleanup; `@watch` values |
| `SMScriptRuntime::ExecuteScript(...)` | evaluates the run line's expressions after the script ends, before the file closes |
| `SMSession::LogOrCountError(int, unistring const&)` | the server event-log line `SMScriptRuntime::CheckError` writes |
| `ScriptRuntimeBase::AllowFileTriggers(FMFileUser*, bool)` | fmsased's `FMS::SASEScriptRuntime` overrides it to return the master trigger switch alone; the base also wants the debugger's user-trigger switch or a running step, which would suppress OnFirstWindowOpen/OnWindowClose. The harness applies fmsased's rule; `FILE_TRIGGERS=base` keeps libFMEngine's |
| `SMScriptRuntime::DispatchStep(bool&)` | with `SASE_EXPORT`, Export Records (36) runs as fmsased's `SASEScriptRuntime::DispatchStep` runs it (fmsased 0x79bdab): an `FMExportManager` constructed, `PerformExport` on `GetWindow(true)`, its error the step's. Without it, and for every other step, libFMEngine's (36 is error 3 there) |
| `DBFileUser::GetDefaultFileFilter() const` | the filter of paths a script may write. libDBEngine's returns `AlwaysFail` (every export path is error 800); fmsased's `SASEFileUser` returns `SessionTempAndDocDirectoryFileFilter`, the session's temporary folder and the server's Documents folder. With `SASE_EXPORT` the harness accepts every path |

## Probe scripts

A battery's `== Name` blocks are probe scripts. `runbat.sh` saves the
file's schema once (`FMDeveloperTool --saveAsXML`, cached in `$WORK`),
`make_patch.py` turns the blocks into an `FMUpgradeToolPatch` that adds the
scripts (step XML in exactly the shapes FileMaker's own Save as XML writes,
taken from the templates and the MBS example files), and the container
applies it with `FMUpgradeTool --update` to its private copy. FileMaker's
XML importer normalizes the step flags but stores every cached block index
(If/Else/Loop/End Loop step values, Else If's End If index) as 0, so the
scripts would jump wrongly. `scriptrun` therefore first runs `@scan` (added
by `runbat.sh` for every probe script): it opens the script for editing the
way the importer does (`FMScript::Open`, `LoadScript`), calls FileMaker's
`ScanScript(ScriptStepListIter const&)` (ScriptUtility.cpp; the Script
Workspace's index writer, unused in the server build) and saves
(`SaveScript`, `DBCatalogMember::Commit`). The `# scan` line lists
`index:step id=value` with **0-based** step indexes (trace line numbers are
1-based): an End Loop holds the index of its Loop and execution continues at
the step after it. `scriptrun` checks that every block step names a partner
of the right kind and stops (`# scan FAILED`, exit 5) when a script is not
found or a partner is wrong; ScanScript itself returns 3702 for unbalanced
blocks (Commit Transaction without Open), which is only a warning because
such probes are deliberate. The scanned files agree with `om-scripts`'
corpus-validated index rules (`step_params`: 0 differ).

Probe syntax, one step per line in FileMaker's display style (arguments split
at top-level `;`; calculations verbatim):

```
== Script name [full access]
Set Variable [ $x[2] ; 1 + 1 ]
If [ cond ] / Else If [ cond ] / Else / End If
Loop [ Flush: Always|Minimum|Defer ] / Exit Loop If [ cond ] / End Loop
Exit Script [ result ]            Exit Script
Perform Script [ "Name" ; Parameter: calc ]     Perform Script [ By name: calc ]
Perform Script on Server [ "Name" ; calc ; Wait: Off ]
Set Error Capture [ On ]  Allow User Abort [ Off ]  Set Revert Transaction on Error [ Off ]
Open Transaction [ skip validation ; skip auto-enter ; override ESS ]
Commit Transaction        Revert Transaction [ Condition: c ; Error: 5001 ; Message: m ]
Set Field [ Table::Field[rep] ; calc ]   New Record/Request   Delete Record/Request [ With dialog: Off ]
Commit Records/Requests [ With dialog: Off ]   Go to Layout [ "Layout" ]
Go to Record/Request/Page [ First|Last|Previous|Next ; Exit after last: On ]
Pause/Resume Script [ Duration: calc ]   Pause/Resume Script   Show Custom Dialog [ title ; message ]
Halt Script   Beep   Freeze Window   # comment   // disabled step   Step 32   (any id, no parameters)
Go to Portal Row [ First|Last|Previous|Next|By calculation: n ; Select: On ; Exit after last: On ; With dialog: Off ]
Go to Object [ Object Name: calc ; Repetition: calc ]   Go to Object [ "name" ]
Go to Field [ Select: On ; Table::Field[rep] ] ("Select/perform" too)   Go to Field [ ]
Go to Record/Request/Page [ By Calculation: n ]   Delete Portal Row [ With dialog: Off ]
Revert Record/Request [ With dialog: Off ]   Set Field By Name [ target name calc ; value calc ]
Go to Related Record [ Show only related records: On ; From table: "TO" ; Using layout: "Layout" ]
New Window [ Name: calc ; Layout: "Layout" ; Height: n ; Width: n ; Top: n ; Left: n ]   (Document style)
Select Window / Close Window [ Current Window ]   [ Name: calc ; Current file ]
Enter Find Mode [ Pause: Off ]   Enter Browse Mode / Enter Preview Mode [ Pause: Off ]   Perform Find   Unsort Records
Insert Text [ Select: Off ; T::F ; "text" ]   Set Selection [ T::F ; start ; end ]   Perform Quick Find [ calc ]
Move/Resize Window [ Current ; Height: h ; Width: w ; Top: t ; Left: l ]   Set Zoom Level [ 150% ]
Show/Hide Toolbars [ Show|Hide|Toggle ]   View As [ Form|List|Table ]   Save Records as PDF [ $path ]
Set Web Viewer [ Object: "name" ; Go to URL: calc ]
```

Go to Portal Row's "With dialog" is Off unless given, so By calculation
asks nothing in FileMaker Pro. The s35 shapes were checked by a round trip
(`batteries/s35-shapes.txt`: the generated file saved as XML again writes
every step back unchanged; a New Window without a layout is FileMaker's
"original layout"). The window, focus and Preview steps exist for
`tools/calc-oracle`'s desktop probe (`desktop_probe.py`), which runs its
scripts in FileMaker Pro.

Layout triggers are `@trigger` lines (tab-separated, anywhere before
`--- run`): `@trigger <TAB> layout <TAB> event <TAB> script [<TAB> modes
[<TAB> parameter]]`, with the event as FileMaker's XML names it
(`OnLayoutExit`), modes `B`, `F`, `P` (default `B`) and an optional
parameter calculation. A layout named by them gets exactly those triggers.
FMUpgradeTool's patches cannot do that (a `ReplaceAction` of a layout is
accepted and silently ignored), so for such a battery `runbat.sh` has
`make_patch.py --generate` write the whole schema document with the probe
scripts and triggers added, and the container runs a file `FMUpgradeTool
--generateDBFile` builds from it (`run.sh -g`) instead of a patched copy:
same tables, layouts and scripts, no records.

Schema directives change the generated file the same way (tab-separated,
before `--- run`; any `@` line makes `runbat.sh` generate). FileMaker
enforces relationship, portal and validation options only in a file
FMUpgradeTool generated, so these are how s35 sets them:

| directive | change |
|---|---|
| `@generate` | none (a generated file only) |
| `@layout <TAB> name <TAB> TO <TAB> layout` | a new layout on TO: a copy of layout without objects, table view columns or triggers, appended last |
| `@field <TAB> TO <TAB> name <TAB> field` | a new field of TO's table, a copy of field |
| `@filetrigger <TAB> event <TAB> script [<TAB> field name]` | the file's triggers (`Metadata` `ScriptTriggers`); OnWindowTransaction's "field name" option (`scriptParameterFieldName`) as the third field |
| `@validate <TAB> TO::Field <TAB> Always\|DataEntry <TAB> calc` | the field's rules: a strict rule by calculation (the shape the export of a `validation_probe` copy writes) |
| `@relationship <TAB> TO <TAB> other TO <TAB> attr=value ...` | `cascadeCreate`/`cascadeDelete` of TO's side of the relationship |
| `@portal <TAB> layout <TAB> TO <TAB> word` | the options word of layout's portal on TO (`Portal/Options`) |
| `@name <TAB> layout <TAB> portal:TO \| TO::Field <TAB> name` | the object name of the first such object of layout |

Table occurrences, fields, layouts and existing scripts are resolved by name
from the schema. Adding a step kind means adding a builder to `STEPS` in
`make_patch.py`; copy its XML from `FMDeveloperTool --saveAsXML` output of a
file that has the step.

## Run lines and output

After `--- run` (or, without that line, each probe script once):

```
script name <TAB> parameter <TAB> expression <TAB> expression ...
```

Escapes `\n \r \t \\ \uXXXX` as in the calc harness. The expressions are
evaluated after the script ends, in the same session (so `$$` variables,
`Get(LastError)`, `Get(LastErrorLocation)` are the script's). Output:

```
name <TAB> parameter <TAB> error <TAB> result <TAB> LastError <TAB> value ...
>  <TAB> depth <TAB> script <TAB> line <TAB> step id <TAB> step error <TAB> handled <TAB> control <TAB> LastError after [<TAB> watch ...]
!  <TAB> code <TAB> event-log message ("file : script : line : step")
@  <TAB> call <target> state <n> bottom <b> -> error <e> <TAB> depth <d>
```

`error` is what `RunScriptAndClose` returns, `result` the top script's Exit
Script text (the PSOS/schedule result), `LastError` the runtime's
`mLastError` after the script. In a trace line, `depth` is the number of
frames when the step started, `step error` the error the step returned to
`PostStepCleanup` (before `MapError`), `control` 1 for steps PostStepCleanup
treats as control steps (flow steps and steps skipped by a reverted
transaction), `LastError after` the runtime's value after the step. Steps
that never execute (comments, disabled steps) have no trace line.

Directives (lines of the run section):

- `@watch <TAB> expr ...` evaluates expressions after every step and appends
  them to its trace line (`@watch` alone clears), in the file user the
  script was started on (`ScriptRuntimeBase::GetFileUser()` faults in this
  session, so it is not used).
- `@trace off|on`, `@context <hex>` (table occurrence for expressions, stored
  form as in the calc harness), `@account <name> <TAB> <password>`.
- `@path <path>` runs the following lines on another path inside the
  container (a file that cannot be opened); `@path` alone goes back to the
  battery's file.
- `@call <TAB> script <TAB> line <TAB> target <TAB> state <TAB> bottom`: after
  that step has run, start `target` through
  `ScriptRuntimeBase::CallScript(window, key, no parameter, newState,
  queueAtBottom, ...)`, the way a button, trigger or PSOS callback starts a
  script (`newState` 0 continue, 1 halt, 2 exit, 3 resume, 4 pause, 5 suspend
  and resume). `@call` alone clears.
- `@scan <TAB> name ...` (added by runbat.sh).

## Limits

- The runtime is libFMEngine's `SMScriptRuntime`/`SMSession`, not fmsased's
  `FMS::SASEScriptRuntime`/`SASESession` (fmsased is a PIE executable and
  cannot be loaded). fmsased's own `DispatchStep` cases (steps 3, 35, 36,
  42, 117, 132, 144, 152, 157, 225, 200; script-runtime.md 10.2) are
  therefore absent: those steps behave as in the shared runtime, not as on a
  real server. Its file-trigger rule is reproduced (above), and with
  `DOCUMENTS` its Export Records: x01 of `tools/fms-transfer-oracle` gave
  the lab server's files that way, but every export path is accepted, and
  `Get ( DocumentsPath )` and `Get ( TemporaryPath )` stay empty (fmsased's
  file user `FMS::SASEFileUser::GetValue` answers them [inference]), so
  probes write to `/root/Documents/`.
- The file is not hosted: Perform Script on Server returns 3; nothing about
  the PSOS wire protocol can be observed.
- Client-only behavior (alerts, `CheckError` of FileMaker Pro, buttons,
  dialogs, debugger) is not in the server build.
- gdb cannot attach under emulation; a crash prints a backtrace to stderr.
- The same image runs natively on an x86-64 Linux Docker host; s11-s13
  gave byte-identical results there and under Rosetta on the Mac.

## Batteries and results

| battery | file | content |
|---|---|---|
| `s00-smoke` | contacts-en | If/Else If/Else, Loop, Perform Script with a result |
| `s01-lasterror` | contacts-en | Get(LastError) after each kind of step, control steps, calc errors in steps, errors crossing Perform Script, error location |
| `s02-conditions` | contacts-en | condition values of If/Else If/Exit Loop If (failing calculations), nesting, Loop flush options |
| `s03-results-vars` | contacts-en | Get(ScriptResult) through nested scripts, parameters, `$`/`$$` scope, names, repetitions |
| `s04-server` | contacts-en | unsupported steps (3), unknown ids (4) and Allow User Abort, error capture vs event log, Pause, PSOS, Get selectors of the server session |
| `s05-transactions` | contacts-en | Open/Commit/Revert Transaction, revert on error, sub-scripts, Halt/Exit inside |
| `s06-halt-state` | contacts-en | Halt in nested scripts, error capture / abort / revert-on-error state across scripts |
| `s07-transaction-errors` | contacts-en | which errors make a transaction revert |
| `s08-queue` | contacts-en | CallScript states 0-5 and queueAtBottom while a script is paused, running or nested |
| `s09-step-ids` | contacts-en | parameterless steps by id: obsolete, gated, window-only, mobile, out of range |
| `s10-fms-sample-triggers` | fms-sample-en | file and layout triggers around a run (existing scripts, no patch) |
| `s11-queue-nested` | contacts-en | CallScript states and queueAtBottom from a sub-script, while paused, two calls after one step, chained calls |
| `s12-transactions-nested` | contacts-en | Revert Transaction with custom codes in sub-scripts, error order, Halt/Exit in a failed transaction, revert setting from a sub-script |
| `s13-runtime-notes` | contacts-en | points the runtime's implementation notes left open: `?` repetition, error location, Exit after last, Perform Script by name, abort in a sub-script |
| `s14-triggers` | contacts-en (generated, `@trigger`) | layout triggers returning True, False, text, empty, nothing, halting or failing: OnLayoutExit, OnRecordCommit, OnLayoutEnter, OnRecordLoad |
| `s15-stack-guard` | contacts-en | the stack memory guard: recursion with large parameters, `$` variables and results until error 2 (run with `STEP_LIMIT=10000000`) |
| `s16-client-finds` | findprobe | finds on unstored calculations, summary fields and a global field |
| `s17-validation-<rule>` | contacts-`<rule>` (generated, `@trigger`) | validation of a repeating field, one rule per file: not empty, by calculation (`Self`, the field by name, a non-repeating field, the repetition number, an unchanged invalid repetition), maximum characters, unique, overridable and data-entry-only rules |
| `s18-<rule>-<variant>` | s18-`<rule>`-`<variant>` (generated, `@trigger`) | strict data types (numeric only, 4-digit year on a date and a timestamp, time of day), range, value lists (custom, a field's values), existing value, maximum characters and the field types alone, each on a repeating and a non-repeating field, strict and overridable, Always and data entry only |
| `s18-find` | s18-find (generated, `@trigger`) | the window after a find without criteria (400), reading global and normal fields in Find mode and with an empty found set |
| `s19-validation-<rule>` | contacts-`<rule>` (generated, `@trigger`) | validation of a repeating field reading a field that does not repeat: bare, as its repetition 1, through `GetRepetition` and `Extend`, and its repetition 2 |
| `s30-design-*` | contacts-en, test-project, tasks-en, designprobe, fms-sample-en, mbs-events, mbs-atb | the Design functions (calc-functions.md 5.8), made by `design_battery.py` in stages: `disc` (the file's lists), `names-<file>` (each occurrence, base table, layout and value list), `fields-<file>` (each field and layout field object), all evaluated after a run of no script; `in-<file>` the same expressions in a probe script (ValueListItems and unqualified names need it). `designprobe` is the file `design_probe.py` and `tools/file_metadata_probe.py` generate from the Tasks template (`SCHEMA=`): every field kind, comments, serial numbers, occurrences with names that sort apart from their ids |
| `s31-get-server` | contacts-en | every Get selector after each step of a probe script (`@watch`): fresh session, New Record, Set Field, Commit, Go to Layout, Sort, Find mode, Browse, error capture, abort, a transaction (calc-functions.md 6) |
| `s31-get-account` | contacts-en | the account selectors with a real login (`@account Admin`) instead of the security bypass |
| `s31-get-layouts` | fms-sample-en | `Get ( LayoutCount )`, `Get ( LayoutNumber )` with layout folders and separators, the trigger panel selectors |
| `s32-layout-objects-*` | contacts-en, tasks-en | `GetLayoutObjectAttribute` for every attribute of the named objects of Contact Details, Task Details and Task List (buttons, a web viewer, fields, a portal, button bar segments) |
| `s34-containers-*` | contacts-en | container fields: Set Field of decoded data, a picture, text, numbers, dates, a copy, a clear, a value set twice (`setfield`); the stream type and `FNAM` path type for each file name extension (`extensions`, `types`); releases by Delete All Records over all records, part of them and one record at a time (`release`, `release-partial`, `release-each`) and by Truncate Table (`release-truncate`, on the file `om-server`'s `step_table_probe` example makes: FMUpgradeTool's import drops the step's table); stored and auto-enter calculations reading a container (`calc`, on contacts-en with fields `s34_calc_schema.py` adds, `SCHEMA=`); Insert from URL into a container over HTTP, with a loopback server `BEFORE` starts (`http`, `http-types`), and `file://` URLs (`url`); the run copy's library read with `om-hbam`'s `dump_keys` (containers.md, "Implementation notes") |
| `s33-get-focus`, `s33-get-focus-tasks` | contacts-en, tasks-en | the focus selectors, open state, `ModifiedFields` and `RequestCount` after New Record, Set Field, Commit, Revert, Find mode, New Request, Go to Field and Insert Text on the details and list layouts (calc-functions.md 9, "Oracle answers (desktop Get selectors and s33)") |
| `s33-get-privileges` | contacts-en with `SCHEMA=` a copy of its saveAsXML granting fmapp, fmxml, fmextscriptaccess and fmrest to [Full Access] | the extended privilege selectors with a real login |
| `s33-account-privileges` | contacts-en with `SCHEMA=` a copy of its saveAsXML adding accounts Entry, Viewer and Prober (data entry, read-only and a custom set) and extended privileges for them | the privilege selectors after a real login as each account (calc-functions.md 9) |
| `s35-shapes` | tasks-en (generated) | the s35 step builders and schema directives, checked by saving the generated file as XML again (nothing runs) |
| `s35-portals` | tasks-en (generated, an Attachments layout, named portal and portal field) | Go to Portal Row moves, By calculation values, Exit after last, Select; Go to Object and Go to Field into and out of portals; Set Field into a related field with and without an active row and through the creation row; Delete Portal Row; Go to Related Record from a row; the active row after Commit, Go to Record and Go to Layout |
| `s35-portals-nocreate` | as s35-portals, creation off and cascade delete on (Attachments side) | the creation row, Set Field without a related record, Delete Portal Row of the last row, Delete Record of the parent |
| `s35-portals-valid`, `-validentry` | as s35-portals, the related field validated by calculation (Always, data entry only) | where an invalid related value fails: Set Field or Commit Records; what is stored |
| `s35-portals-word<n>` | as s35-portals, portal options word n (0, 1, 256, 273; the template's 257) | the creation row and the rows past the visible ones, Delete Portal Row |
| `s35-windows` | tasks-en (generated) | New Window, Select Window, Close Window, per-window found sets, records and modes, the window selectors |
| `s35-wintx`, `-field`, `-option` | tasks-en (generated, OnWindowTransaction trigger; a field named OnWindowTransaction; the field name option) | which operations fire OnWindowTransaction, its JSON parameter, when it runs |
| `s35-txvalidation` | tasks-en (generated, Description validated Always, Category during data entry) | which step reports a validation error inside a transaction (the step leaving the record or Commit Transaction), the revert, LastError, skip validation |
| `s40-related-autoenter` | related-ae: contacts-en with Contacts A1, A2 (auto-entered `Phone Numbers::Number`, replacing and not) and V1 (validated Always by `Phone Numbers::Number ≠ ""`) added by FMUpgradeTool | when a calculation over a related field is auto-entered or validated: the match field set, the related record made or changed, "Do not replace existing value" (auto-entry.md, "Calculations over related fields") |
| `s41-parent-lookup` | parent-lookup: contacts-en with Contacts::L2 (looked up from `Phone Numbers::Number` starting from Contacts) added by FMUpgradeTool | when a lookup from a parent table keyed on its auto-entered primary key copies: the record made, a related record made, another field set, the key set to its own value (auto-entry.md, "Lookups") |
| `s42-sorted-lookup` | sorted-lookup: assets-en with Assets::LA (looked up from `Assignments::Note` starting from Assets, through the starter's relationship sorted by Assignments::Sort By descending) added by FMUpgradeTool | which of two matching records a lookup through a sorted relationship copies, the first in the sort made first or last (auto-entry.md, "Lookups") |
| `s43-typed-lookup` | typed-lookup: assets-en with Assets::AT (Time) and eight Assignments lookups from Assets (Number, Date, Time, Timestamp, Number and Date into Text, Text into Number and Date) added by FMUpgradeTool | what a lookup of another type than text copies: the stored text as it is, also across types (auto-entry.md, "Lookups") |
| `s46-record-number-ae` | contacts-en with Contacts::RecNum and FoundAE (auto-entered `Get ( RecordNumber )` and `Get ( FoundCount )`, text) added by `s46_record_number_schema.py` (`SCHEMA=`) | what a record a script makes stores: New Record in all records, after moving to the first, after omitting two, in an empty found set, twice in a row, and Duplicate Record (auto-entry.md, "Get ( RecordNumber )") |
| `s36-schedule-run` | contacts-en | a schedule's run (script-runtime.md 13.23): script names in other cases, with spaces, missing, empty or shared by two scripts; parameters; the session's Get answers; results and errors; `$$` variables across runs; real logins (wrong password, unknown, lower-case and disabled accounts); a file that cannot be opened (`@path`) |

`results/` holds the outputs. Expressions of a run line are evaluated
after the script, with no table occurrence unless `@context` sets one; a
probe script that `Evaluate`s them answers in the window's occurrence
instead, and only there do `ValueListItems` and fields named without an
occurrence answer (s30). Evaluating a value list from a field builds that
field's index when it is set to index automatically, which changes its
`FieldType` from `Unindexed` to `Indexed` for the rest of the run. Sample
files: `contacts-en.fmp12` is the
Contacts starter template (`DynamicTemplates/Contacts/en.fmp12`),
`fms-sample-en.fmp12` the FileMaker Server sample `en_FMServer_Sample.fmp12`
(its full access account is unknown, so it is only run, not patched), and
`findprobe.fmp12` the file `FMUpgradeTool --generateDBFile` builds from
`tools/file_metadata_probe.py --no-triggers --find-fields` over the Tasks
template's saveAsXML (`run.sh -l -g <document> tasks-en.fmp12` writes it to
`$WORK`). s16's run copy, with the records its setup script adds, is the
`sumprobe.fmp12` of `tools/fms-sql` q19 and q20.
`contacts-<rule>.fmp12` (s17) is contacts-en with one field given three
repetitions and a validation rule by OpenMaker's writer
(`cargo run -p om-server --example validation_probe -- contacts-en.fmp12
contacts-<rule>.fmp12 <rule>`); FileMaker enforces the rule only in the
file FMUpgradeTool generates from that copy's XML export, which the
battery's never-fired `@trigger` line makes `runbat.sh` run. The findings
are in script-runtime.md section 13.
`s18-<rule>-<variant>.fmp12` is built by `s18_validation.sh` (its header
has the details): contacts-en with four fields given the rule by
`validation_probe spec`, exported with FMDeveloperTool, the range and the
value list filled into that export, and run as the file FMUpgradeTool
generates from it. Its probe script is `s18-probe.txt`.
`s18-find.fmp12` is contacts-en with Website made global
(`validation_probe ... spec 7=1,0,1,10000`).
`contacts-<rule>.fmp12` of s19 (rules `extbare`, `extindex1`,
`extgetrep`, `extextend`, `extindex2`) is built as for s17
(`validation_probe ... <rule>`).
