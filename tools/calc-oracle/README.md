# Local FileMaker calculation oracle

Run on a Mac with FileMaker Pro 26, from any working directory:

```sh
export OM_CAPTURE_APP="/path/to/FileMaker Pro Capture.app"   # the capture copy
export OM_CAPTURE_HOME=/path/to/fm-home                      # its CFFIXED_USER_HOME
/path/to/worktree/tools/calc-oracle/run.sh cases.json out.json
```

| variable | meaning |
|---|---|
| `OM_CAPTURE_APP` | required: the stock-signed copy of FileMaker Pro the runner launches |
| `OM_CAPTURE_HOME` | required: the `CFFIXED_USER_HOME` that copy runs with |
| `OM_ORACLE_ROOT` | the private scratch tree (database, run folders, lock); default `re/work/oracle` in the repository the tool is in |
| `OM_AX_DUMP` | optional: a JXA script that prints a process's window tree, for failure reports |

Input is a UTF-8 JSON array: `[{"id":"example","expr":"1/3"}]`.
Output is an object with a `locale` header and `results` array. Every result
preserves `id` and `expr` and adds `text`, `error`, `number` (text, to avoid
JSON floating-point loss), `length`, `date`, `time`, and `timestamp`.
Conversions are observations, not authoritative type tags; `?` is preserved.
The default whole-run deadline is 180 seconds (`--timeout 600` to increase).
Each Apple event has a 20-second reply limit and each subprocess is bounded.
A running desktop session and existing macOS Automation permission are required;
no human needs to be at the screen during a normal run.

## Database and transport

The local, uncommitted database is `calc-oracle.fmp12` in the scratch tree
(`$OM_ORACLE_ROOT`, by default `re/work/oracle/calc-oracle.fmp12`).
It was created in FileMaker Pro 26.0.3 with the default **Admin, empty password**,
Full Access account. Guest is disabled. FileMaker Network, WebDirect, ODBC/JDBC,
Data API and URL-script extended privileges were left disabled.
Only `fmextscriptaccess` was enabled for Full Access in this disposable file.
Do not host or share it.

The implementation uses a calculation field instead of an `Oracle` script.
The default `Table` contains one record, a Text field `Input`, and a Text-result
calculation `Output` containing [Output.fmcalc](Output.fmcalc). Both fields are
on the default layout. Default audit fields are unused. This small schema can
be recreated manually if the ignored file is lost; the tools do not recreate it.

The runner launches only the stock-signed copy named by `OM_CAPTURE_APP`,
with `CFFIXED_USER_HOME` set to `OM_CAPTURE_HOME`.
After startup readiness it sends a local-file open event directly to its PID.
It writes `Input` and reads the calculated JSON through PID-addressed Apple
events. Explicit Unicode descriptors are essential: JXA's normal `cell.set`
turned `1/3` into unrelated Unicode characters in this version of FileMaker.
No bundle-ID dispatch, script URLs, localhost connections, or network ports
are used. The installed application is not launched or changed.

Before **every** automation operation, the runner inventories `pgrep -fl
FileMaker` and verifies its owned executable path through `ps`. Other processes
are ignored, never adopted or quit. A lock prevents overlapping oracle runs;
an already-running capture copy causes failure instead of reuse. The runner
quits its owned PID in `finally`; if graceful quit fails, it verifies ownership
again before terminating that PID. Inspect a prompt in the capture copy if a
run fails; never answer license, sign-in, or update questions automatically.

Each run retains private snapshots, launch log, PID and results in an ignored
`re/work/oracle/run-*` directory. Shared `com.filemaker.client.pro12` preferences
are exported before launch and after quit and diffed there. CFFIXED_USER_HOME
does **not** isolate them. This session changed recent-file, window/dialog,
toolbar and usage-statistic entries. The runner reports changes and does not
blindly restore a snapshot over another instance's preferences. Snapshots can
contain sensitive existing settings and must never be committed.

## Scope and limitations

Use trusted, local, pure calculation expressions. This is not a sandbox for
untrusted expressions or plug-in functions; never use expressions that contact
servers, read credentials, or otherwise violate the capture safety rules.

* Evaluation takes place in the blank file's one-record `Table` context.
  Record, relationship, layout, script, privilege and host dependent functions
  cannot stand in for their behavior in a real solution.
* `Output` calls `Evaluate` twice: once for the value and once for
  `EvaluationError`. Side effects and nondeterministic functions can therefore
  diverge; Random, UUID and current time results are snapshots, not assertions.
  Repeating an identical input may also return a stored calculation unchanged.
* Each result is a field calculation, not a fresh `EvalOne` sub-script. `$`
  script scope is not reproduced; `$$` and plug-in/session state may leak
  between cases. Wrapper Let names `r` and `e` also exist. Use pure Let/While
  local variables; test script scope separately.
* Empty `Input` is suppressed by FileMaker's default calculation option;
  evaluate `""` (a quoted empty string expression) to test empty text.
* Result coercions lose container content, styling and original numeric/date
  type identity. `GetAsText` preserves FileMaker's formatting, including its
  normal display precision. `SetPrecision` is tested as an expression, not
  assumed to exist as a function.
* The `.fmp12` is intentionally not committed. A clone on another Mac needs
  the schema above, the safe capture copy and the proper local environment.

The committed smoke files contain real captured results; regenerate inputs
with `python3 -I tools/calc-oracle/make-smoke.py`, then invoke `run.sh` as above.
The header records language, raw `Get(FileLocaleElements)`, decimal, date,
time and timestamp samples rather than assuming the shell's locale.

Observed locale: English (`en`, locale ID 21), MDY with `/`, decimal `.`,
thousands `,`, 12-hour timestamp display with AM/PM. The time-value text sample
is `13:02:03.5`; the timestamp sample is `12/31/2024 1:02:03.5 PM`.

## Smoke observations

245 expressions completed; 15 produced nonzero evaluation errors (deliberate
invalid calculations included). These are recorded observations, not a claim
that OpenMaker passes them. Notable examples:

* `1/3` becomes `.3333333333333333`; `SetPrecision(1/3;50)` preserves 50 places.
* `10^400` succeeds and returns `1.0E+400`.
* Both `"10" < "9"` and `10 < "9"` return `1`.
* `GetAsNumber("1e3")` returns `1000`; `"abc12d3.4" + 0` returns `123.4`.
* `Date(2;30;2024)` normalizes to `3/1/2024`; `Time(25;0;0)` is `25:00:00`.
* JSON object keys are sorted in the tested output; formatted JSON uses CR
  and tabs. Tabs, CR, LF and emoji survive the transport exactly.
* `1/0` reports 15; `Evaluate("1+")` reports 1204;
  `Let(x=1;y)` reports 102. `Case`, `Choose` and `If` avoided errors in the
  unchosen branches tested.

## Probe scripts: desktop answers in a real window

The script mode settles what FileMaker Pro answers in a window
(calc-functions.md 6.2 and 6.3): focus, found sets, Preview, layout object
bounds and other open files. It uses the same capture copy with the same
safeguards as the evaluation mode:

- the lock;
- a refusal to adopt a running copy;
- a process-ownership check before every Apple event;
- PID-addressed events only;
- `CFFIXED_USER_HOME`;
- quit in `finally`;
- the preferences diff.

It sends no keyboard or mouse input and no URLs, activates no window,
and opens only local files.

```sh
export FMS_ROOT=/path/to/re/extracted/fms/deb/opt/FileMaker
export FILES=/path/to/re/work/fms-harness/files   # contacts-en, tasks-en
OUT=/path/to/re/work/oracle/desktop-probe
tools/calc-oracle/make-desktop-probe.sh "$OUT"         # Contacts.fmp12, Tasks.fmp12, plan.json
tools/calc-oracle/run.sh --scripts "$OUT/plan.json" "$OUT/out.json" --files "$OUT" --timeout 900
```

**Building the probe files.** `make-desktop-probe.sh` exports the
Contacts and Tasks starter templates (`FILES` holds copies of
`DynamicTemplates/{Contacts,Tasks}/en.fmp12`). `desktop_probe.py` then
edits the export:

- a global field `zOut` for the answers;
- `fmextscriptaccess`, which Apple events need;
- O4's footer and O5's web viewer calculation, which answers a `data:`
  URL;
- one probe script per state.

The script steps are those of `tools/fms-script-harness/make_patch.py`.
FileMaker Server's FMUpgradeTool `--generateDBFile` builds each file in
the harness container, taking the machine-wide lock. FMDeveloperTool
exports it again, and `desktop_probe.py check` confirms that every
script's steps, the field, the privilege and the layout changes were
stored. Scripts have no control flow, because the importer stores block
indexes as 0. A build takes under a minute.

**Running.** The runner copies the files into
`re/work/oracle/desktop-probe-open/`. That folder is fixed, so repeated
runs reuse one recent-file entry. It launches the capture copy, opens
each file with a PID `odoc` event and runs every script of the plan with
a PID-addressed `do script` (`misc/dosc`, a `FileMaker script` of a
`database`). The event returns when the script ends. The runner then
reads `zOut` of the table's first record:

- a JSON header line with the script name and a stamp;
- one JSON line per expression: state, expression, `GetAsText (
  Evaluate ( x ) )` and `EvaluationError ( Evaluate ( x ) )`.

After each script the runner records two read-only System Events
answers: the owned process's window frames and whether it is frontmost.
When a script saves a PDF, the runner also records its page texts
(PDFKit). On a failure it writes a read-only dump of the owned process's
windows (`windows-on-failure.txt`), so that a prompt can be reported; it
never answers one. The session folder keeps `results.json`, the files as
the run left them and the preferences snapshots, as XML with their diff.

Observed while building it (FileMaker Pro 26.0.3):

- `Go to Field [ ]` commits the record, so a probe that leaves a field
  must restore its data.
- A list layout's objects have no bounds until the window has drawn.
  Pause before measuring after Go to Layout, zoom or toolbar changes.
- The capture copy stays in the background (System Events: not
  frontmost), so `isActive` answers 0.
- Printing cuts a layout wider than the page at the page edge.
- Shared preferences: the first runs added recent-file and thumbnail
  entries for the probe files, shifting the existing recent entries down
  without removing any. Later runs changed only FileMaker's license usage
  counters (`FM License Statistics:*`).

## Compile corpus: experimental, incomplete

```sh
./tools/calc-oracle/compile.sh expressions.txt \
  /path/to/re/work/oracle/my-new-corpus
```

This prototype requires a **new** output directory inside the private oracle
scratch tree, a visible desktop and System Events permission. It refuses to
run when **any** FileMaker process is already present, including the capture
copy. Do not close someone else's FileMaker to satisfy that restriction.
It copies the closed local oracle database, writes `fmxmlsnippet` XML for
`Set Variable` steps, tries to create `OracleCompileGenerated` in Script
Workspace, restores the pasteboard, quits its owned PID, then invokes the
read-only `om-hbam`/`om-scripts` extraction example. It has **not** completed
an end-to-end compile run, and `compiled.json` is only written if every input
has extracted formula slots. Source expressions must compile in the blank
file's context. No script is run; expressions are stored as step calculations.
The prototype is retained as a reproducible starting point, not a working
compile service.

Concrete experiment results on FileMaker Pro 26.0.3:

1. In the owned capture file, created `CompileAttempt` with
   `Set Variable [ $e1 ; Value: 1+2 ]` and copied the step.
   FileMaker supplies the pasteboard type
   `CorePasteboardFlavorType 0x584D5353` (`XMSS`), also exposed as
   `dyn.ah62d4rv4gk8zuxnxnq`.
2. The native XML is `fmxmlsnippet type="FMObjectList"`, with
   `Step enable="True" id="141" name="Set Variable"`, a
   `Value/Calculation`, `Repetition/Calculation` containing `1`, and `Name`.
   Generated XML for `$e2`, value `SetPrecision(1/3;50)`, pasted successfully;
   Script Workspace displayed the expected compiled step and saved it.
3. A keyboard-only manual sequence (New Script, name, Return, Paste, Save)
   also succeeded, creating `OracleCompile` with that step. These two sample
   scripts remain in the ignored local oracle file; the evaluation runner
   never executes them.
4. The first shell-driven compile attempt exited at the `workspace` stage
   with an AppleScript error. That first prototype did not preserve the
   error text, so its exact cause is unknown. Startup readiness was then
   strengthened. A separate stock `/Applications/FileMaker Pro.app` PID was
   observed; further GUI automation stopped without touching that PID.
   The prototype now refuses that situation before launching anything.
5. Extraction of the saved manual sample with the repository's
   `ScriptCatalog::decode` failed with:
   `Script CompileAttempt has an unsupported step reference.`
   This is a second, independently reproduced blocker. The example currently
   fails at catalog decode, before yielding formula bytes. The encoding
   workstream can inspect the private `calc-oracle.fmp12` script catalogs;
   no hex streams are claimed here.

To reproduce the read-only extraction failure:

```sh
cargo run -p om-scripts --example compile_oracle -- \
  /path/to/re/work/oracle/calc-oracle.fmp12 OracleCompile
```

The extractor walks each decoded step's
`[17,5,script,5,step,slot,5,{5,6}]` subtree. It emits step-index, slot,
unformatted hex and formatted hex as TSV; `compile.py` associates these with
expressions in `compiled.json`. All slots are retained (including repetition
formulas); it does not pretend every slot is the expression-value slot.
