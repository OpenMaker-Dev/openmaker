# Script runtime: execution, control flow, errors, variables, triggers, transactions, PSOS

Status: research specification for the `om-scripts` runtime and the
`om-server` script engine (draft 1, 2026-10-28; section 13, engine runs,
2026-10-29). It describes how FileMaker 26 executes a script once the step
list is loaded: the frame stack and the step loop, how the cached jump
indexes in stored steps are used, how
`Get(LastError)` is set and cleared, error capture and dialogs, local and
global variables, script parameters and results, pause and abort, script
triggers, transactions, full-access scripts, which steps run on FileMaker
Server, and the Perform Script on Server (PSOS) request on the wire. Step
parameter storage is specified separately in `script-steps.md`; this
document cites storage only where the runtime reads it.

Evidence boundary:

- Primary: FileMaker Server 26.0.3.309 for Ubuntu 24 (`re/extracted/fms/deb/opt/FileMaker/`):
  `lib/libFMEngine.so` (script engine, full DWARF; source paths
  `Draco/FMUserModel/ScriptRuntimeBase.h`), `lib/libDBEngine.so`
  (`RCPerformScriptOnHost`, `RCSendPSOSRetToClient`, variable maps),
  `lib/libFMSLib.so` (`SASEPerformScriptOnHost`), and the daemons
  `Database Server/bin/fmsased` (FileMaker Script Engine, `FMS::SASEScriptRuntime`),
  `fmscwpc` (WebDirect, `iwp::IWPScriptRuntime`), `fmwipd` (Data API,
  `FMWIP::WIPScriptRuntime`), `fmserverd` (`SASEPerformScriptOnHost::CreationFunction`).
- Addresses are Linux `nm` virtual addresses of `libFMEngine.so` unless
  another binary is named (Ghidra image base adds 0x100000). Field offsets
  are from DWARF of the same build. The Mac arm64 addresses in `scripts.md`
  belong to a different binary and are not comparable.
- Public Claris documentation cached under
  `re/work/script-steps/claris-html/content/` (help build 2026-08-27); pages
  are cited by file name.
- Not covered: the FileMaker Pro client's own platform subclass (UI steps,
  dialogs, debugger). The Linux server build does not contain it; where the
  client behaves differently it is taken from Claris docs and marked (doc).

Tags: (obs) seen in code, data or DWARF; (inf) inference from observations;
(doc) stated by Claris documentation; (run) observed by running FileMaker
Server's own script engine (`tools/fms-script-harness`). Sections 1-12 are
static analysis; section 13 records the engine runs, and where a run
contradicted the static reading the earlier text points there.

Scratch and reproduction: `re/work/scripting/runtime/` (decompiler output
`d1/`..`d3/`, DWARF dumps, `tools/` helpers: `dwclass.py` DWARF layouts,
`jumptable.py` switch-table decoder, `rawbytes.py`, `make_compat_csv.py`).

## 1. Runtime objects

### 1.1 `ScriptRuntimeBase` (one per session, DWARF size 0x950)

One runtime object exists per `FMSession` (client) or per script-engine
session (server); `FMSession` creates it lazily (session vtable slot
`CreateScriptRuntime`, cached at session+0xa00) (obs). The members that
carry script semantics (obs, DWARF `Draco::ScriptRuntimeBase`):

| Offset | Member | Meaning | Initial value (ctor 0xa6e560) |
|---|---|---|---|
| 0x010 | `mDataBuffer` (DBData) | scratch result of the step's main calculation (If, Exit Loop If) | empty |
| 0x3a8 | `mSession` | owning session | |
| 0x3b0 | `mDataModel` (`ScriptRuntimeModel`, 1.2) | last error, stack, trigger switches | |
| 0x4d0 | `mRunning` (atomic) | a script is executing (not paused) | false |
| 0x4d1 | `mPaused` | paused by Pause/Resume or a pausing mode step | false |
| 0x4d2 | `mAutoPaused` | paused implicitly (debugger) | false |
| 0x4d4 | `mClauseEvaluateTrue` | the current If/Else If chain already took a branch | false |
| 0x4d5 | `mAllowAbort` | Allow User Abort state | true |
| 0x4d6 | `mAllowAnimation` | Set Layout Object Animation state | false |
| 0x4d7 | `mQueued` | a pushed script has not started yet | false |
| 0x4d8 | `mCaptureErrors` | Set Error Capture state | false |
| 0x4da | `mContinueExecutionLoop` (atomic) | keep stepping in this time slice | |
| 0x4db | `mInMultiStageStep` | an asynchronous step is in progress (1.7) | false |
| 0x4e0 | `mPauseStartTime` (ms tick) | when the current pause began | |
| 0x4e8 | `mPauseTime` (double s) | pause length; -1.0 = indefinitely | 0 |
| 0x4f0 | `mCurrentScriptStep` (`ScriptStep`, 0x44 bytes) | copy of the step being executed: id at +0x14 (0x504), step value at +0x16 (0x506), step flags at +0x18 (0x508) | cleared after each step |
| 0x534 | `mLastExecutedStepID` | absolute id of the previous executed step | 0 |
| 0x538 | `mLastCallScriptTime` | tick of the last CallScript | |
| 0x562 | `mStepAction` | pending action for pause/debugger (inf: 3 is written on an abort path) | 0 |
| 0x568 | `mTriggerState` | chain of completed trigger states waiting for their event action (7.4) | null |
| 0x570 | `mTriggerActive` | some frame on the stack was started by a trigger | false |
| 0x578/0x598 | `mVariableName`, `mVariableRepetition` | target variable of the current step (steps that write to a variable) | cleared after each step |
| 0x630.. | `mStepErrorsLog*` | Set Error Logging state per file | |
| 0x8c8 | `mScriptTransactionMgr` | transaction state (1.4), created on first Open Transaction | null |
| 0x8d4 | `mInLoopCount` | nesting of loops with a non-default Flush option | 0 |
| 0x8d8 | `mIsProDebuggerDataViewerVisible` | forces Loop Flush to Always | false |
| 0x8d9 | `mAbortImmediately` | Set Revert Transaction on Error state | true |

Server subclass `SMScriptRuntime` (size 0x960, ctor in
`SMSession::CreateScriptRuntime` 0xa67390) adds a result buffer pointer
(+0x950) and the final frame pointer (+0x958) used to return the top
script's result to PSOS (obs).

### 1.2 `ScriptRuntimeModel` (+0x3b0, DWARF size 0x120)

| Offset in model | Member | Meaning |
|---|---|---|
| 0x08 | `mPauseOnError` | debugger option |
| 0x09 | `mLastError` (`DBError`: u16 code + u16 extended-info index) | value of `Get(LastError)` |
| 0x0d | `mLastSSLError` | certificate error of the last connection step |
| 0x18 | `mLastErrorLocation` (`ErrorLocation`) | source of `Get(LastErrorLocation)` |
| 0x50 | `mTransactionErrorLocation` | location of the error that failed the open transaction |
| 0x88 | `mMemoryCheckCount` | counter for the stack memory check (2.6) |
| 0x8c | `mMasterTriggerSwitch` | all script triggers enabled |
| 0x8d | `mUserTriggersEnabled` | debugger "disable triggers" switch |
| 0xa8 | `mScriptStack` (vector of frame pointers, bottom first) | call stack |
| 0xc0 | `mCurrentStackFrame` | frame whose steps are executing (normally the top) |
| 0xe8 | `mPSOSResultMap` (frame id -> DBData*) | result buffers for frames queued under another script |

`ErrorLocation` (0x38 bytes): `mLineNumber` (i16), `mScriptName`,
`mStepID` (i16), `mPluginID` (QuadChar), `mExternalID` (i16) (obs).

### 1.3 Stack frames

The stack holds reference-counted `ScriptStackElement`s (obs, DWARF):

| Field (offset) | Meaning |
|---|---|
| `mTargetWindow` (0x10) | window the frame runs in (its layout, found set and current record are the script context) |
| `mAllowAbort` (0x18), `mCaptureErrors` (0x19) | runtime flags saved when this frame was suspended |
| `mOriginalLayoutKey` (0x1a) | layout at frame start |
| `mScriptResult` (0x20) | the value `Get(ScriptResult)` returns while this frame is current |
| `mWebScriptResult` (0x28) | result for web-viewer `FileMaker.PerformScript` callers |
| `mWasPaused` (0x31), `mPauseTime` (0x38), `mPauseStartTime` (0x40) | saved pause state |
| `mRetryStage` (0x44) | multi-stage step progress |
| `mFrameID` (0x48) | unique frame id (key of `mPSOSResultMap`) |
| `mIsButton` (0x50) | frame created by a button |
| `mParentRunWithFullAccess` (0x51) | caller ran with full access (9.1) |

Two kinds exist (obs):

- `ScriptStackElementScript` (0x780 bytes): a script. Adds `mScript`
  (`FMScript` catalog member), `mScriptStepListIter` (step list pointer plus
  the current index, a signed 16-bit value at iterator+8), `mScopedVars`
  (`DBVarMap`, the frame's `$` variables), `mScriptParameter`,
  `mTriggerState` (non-null when the frame was started by a trigger),
  `mRetryStepIndex`, `mScriptKeyPath`.
- `ScriptStackElementStep` (0x120 bytes): a single step executed outside a
  script, i.e. a button whose action is one step. Holds a copy of the step
  (`mTheStep`), the list it belongs to and two "started" flags; it executes
  exactly one step and has no variables of its own.

The current index starts at -1; "next step" increments it before reading
(2.2). A script frame's index therefore always names the step that is
executing, and a jump is expressed by storing `target - 1` (or the target
itself where the next increment is meant to skip it) (obs).

### 1.4 `ScriptTransactionMgr` (DWARF size 0x50)

| Offset | Member | Meaning |
|---|---|---|
| 0x00 | `mWindow` | window whose record transaction is used |
| 0x08 | `mError` | first error recorded inside the transaction |
| 0x0c | `mIsOpen` | Open Transaction succeeded and not yet ended |
| 0x10 | `mTransactionStackFrame` | frame that opened it |
| 0x18 | `mIsAborted` | reverted (explicitly or by error); remaining steps are skipped |
| 0x1a | `mEndStepIndex` | index of the matching Commit Transaction (Open Transaction's step value) |
| 0x1c | `mTransactionId` | id copied from the window's DB transaction |
| 0x20 | `mNeedsToKillAllLocks` | |
| 0x28 | `mNewTableAliasKeyList` | tables created by Import Records into a new table (removed on revert) |
| 0x40 | `mLayoutKeyBeforeImport` | |
| 0x46 | `mSkip` | Open Transaction failed: steps inside are skipped |
| 0x47 | `mSkipValidateDataEntry` | Open Transaction "Skip data entry validation" |
| 0x48 | `mOldDirtyWrite` | saved window setting |

### 1.5 `TriggerState` (DWARF size 0x78)

Created when a trigger fires (`FMWindowModel::CallScriptTrigger` 0xa4b8b0,
section 7): session, source/trigger window id, trigger file id,
`mTriggerID` (event id, 7.1), `mTriggerFlag`, `mTriggerActionPtr` (the
pending default action of the event), `mRetryTriggerAction` (the trigger
script's boolean result), `mInitialized`, `mTriggerStateChainPtr`,
`mActiveObjectState` (focus at fire time), `mCommitChoice`, `mConfirm`,
`mIsScriptStep`, and three panel keys for OnPanelSwitch (obs).

### 1.6 Server runtimes

| Process | Runtime class | Base | Used for |
|---|---|---|---|
| `fmsased` (FileMaker Script Engine) | `FMS::SASEScriptRuntime` | `DSScriptRuntimeBase` -> `SMScriptRuntime` -> `ScriptRuntimeBase` (inf from vtables and shared helper names) | PSOS and schedules |
| `fmscwpc` | `iwp::IWPScriptRuntime` | same family | WebDirect |
| `fmwipd` | `FMWIP::WIPScriptRuntime` | same family | Data API (and, inf, XML Custom Web Publishing) |

All three override `DispatchStep(bool&)`, the platform hook of section
2.3, and fall back to `SMScriptRuntime::DispatchStep` (0xa65090) and then
the shared `ScriptRuntimeBase::DispatchStep_Private` (0xa7b3b0) (obs).

### 1.7 Multi-stage steps

A step handler may return the internal code 3001: the runtime then marks
`mInMultiStageStep`, stops the current time slice and clears the error; the
same step is re-entered later (asynchronous work such as URL transfers or
LLM calls) (obs, `PostStepCleanup` 0xa7dd00). Internal code 3000 means
"retry this step after the pending trigger action": a retry action is
attached to the topmost frame's trigger state (obs). Neither code is ever
stored in `Get(LastError)` (4.2).

## 2. Execution model

### 2.1 Run loop

`ScriptRuntimeBase::DoRunLoop` (0xa768f0) is driven from the session's idle
processing (client) or from `SMSession::Idle` (server, 0xa675d0) (obs). One
call executes steps until any of these holds (obs):

- the runtime is no longer running or `mContinueExecutionLoop` was cleared
  (pause, multi-stage step, halt);
- the current frame's file or window has gone away: the frame is exited as if
  it finished (`ExitStackFrame(abort=1, result=true)`);
- 2000 ms have elapsed since the call started (the loop yields to the event
  loop and resumes on the next idle call; there is no step budget).

After every step it performs pending trigger actions (`DoTriggerAction`,
7.4) and starts queued scripts (`StartQueuedScripts`). On the server,
`SMScriptRuntime::CompleteScript` (0xa64400) calls the session idle routine
in a loop until the runtime is neither running, queued nor paused, so a
server script runs synchronously to completion (obs).

### 2.2 Fetching the next step

`DoNextStep` (0xa76d70) asks the current frame for the next step
(`SetupNextStep`, frame vtable slot 0x50), which returns a `StepType`
(DWARF enum: `kNoStep` 0, `kEnabledStep` 1, `kIgnoredStep` 2) (obs):

1. Increment the frame's index. If it is now at or past the step count:
   `kNoStep`. The runtime then exits the frame as a normal end of script
   (`ExitStackFrame(0, true)`), unless a trigger action is pending.
2. Otherwise the step is `kIgnoredStep` (skipped silently: no error, no
   change to `Get(LastError)`, no PostStepCleanup) when any of:
   - its stored id is negative (disabled step; `ScriptStep::IsStepEnabled`);
   - its id is 89 (`#` comment);
   - the frame's script is not executable by the account: script access
     below "read only" (`DBAccessLevel` < 2, i.e. no access) and the script
     is not marked "run with full access" and the caller did not run with
     full access (9.1).
3. Otherwise `kEnabledStep`: the step descriptor (28 stored bytes plus the
   reference keys) is copied into `mCurrentScriptStep` and `Execute` runs.
   Before that, `CorrectUserMode` (0xa733d0) takes the frame's window out
   of window mode 2 (inf: Layout mode; modes 0 Browse, 1 Find, 3 Preview);
   if that fails the frame ends as if it had finished (obs).

Steps are never skipped because of their compatibility flags; incompatible
steps reach the dispatcher and fail there (section 10) (obs).

Before `Execute`, when the frame's script has the full-access flag, the
file user's privileges are raised to full access for this one step and
restored after it (9.1) (obs).

### 2.3 Executing one step (`Execute`, 0xa771a0)

The checks run in this order (obs); each early exit still goes through
PostStepCleanup (2.4) with the error shown:

| # | Condition | Result |
|---|---|---|
| 1 | `ScriptStep::IsStepObsolete` (2.5) | error 4 "Command is unknown" |
| 2 | `IsServerAbort()` (server: administrator disconnected the script session) | error 1, then the script is aborted |
| 3 | the frame is a button single-step frame (not a script), the step accepts a target field (per-step applicable-options table `sFlagsTable` 0x10ff520, bit 0x1 of the id's u32), no field spec is stored (flag 0x1 clear), id not in {17, 47, 144, 202, 242, 244, 245, 246}, and the window has no active field | internal error 4100 (not a documented code; it never reaches `Get(LastError)`, 4.2) |
| 4 | a transaction is open and either it is marked aborted or it holds an error while Set Revert Transaction on Error is On; and the step is not Open Transaction (205) or Commit Transaction (206) | step skipped like a control step (no error change) |
| 5 | otherwise | `PreDispatchStep` hook, then the dispatch below |

Dispatch by absolute step id (obs):

- Id 0: error -1 "Unknown error".
- Steps handled directly by `Execute` (no platform hook): If 68, Else 69,
  End If 70, Loop 71, Exit Loop If 72, End Loop 73, Commit Records 75,
  Allow User Abort 85, Set Error Capture 86, comment 89, Halt Script 90,
  Exit Script 103, Else If 125, Add/Delete/Reset/Enable Account 134-137,
  Set Variable 141, Go to Object 145, Set Web Viewer 146, Set Layout Object
  Animation 168, Set Session Identifier 208, Set Revert Transaction on Error
  223, Configure Persistent Data 238.
- Every other id: the platform hook `DispatchStep(bool& handled)` runs
  first. If it returns error 4 ("not mine"), the shared
  `DispatchStep_Private` runs. Its jump table (0x10e632c, ids 1..247)
  routes every implemented common step; ids without a case get error 3
  "Command is unavailable" if the id is below 248 and the step is not
  obsolete, else error 4.
- When Commit Transaction (206) fails with a code other than 3000/3001,
  `Execute` stores the code in LastError at once (logged unless it is a
  custom 5000-5499 code); for non-custom codes the step is then treated as
  a control step by PostStepCleanup, so it is not reported twice (obs).

The second output of the hook (`handled`) tells PostStepCleanup that the
handler already reported the error itself (the step's own error is then not
re-reported, and capture logic uses the stored LastError) (obs).

Account steps: Reset Account Password maps internal 8003 to 310 (obs).

### 2.4 After each step (`PostStepCleanup`, 0xa7dd00)

Arguments: the step error, the handler's `handled` flag, and a
"control step" flag that `Execute` sets for If, Else, End If, Loop, Exit
Loop If, End Loop, Halt Script, Exit Script, Else If and for steps skipped
by an aborted transaction (obs).

1. Unless a multi-stage step is in progress, `mLastExecutedStepID` =
   absolute id of this step.
2. If not a control step and not `handled`:
   - error 0: `SetLastError(0, log=true)`. This is what clears
     `Get(LastError)` after a successful step.
   - error 3000: attach a retry action (1.7).
   - error 3001: suspend for a multi-stage step, error becomes 0.
   - any other error: the session's `HandleError` stores it with
     `SetLastError(err, log=true)` (4.2) (obs, `FMSession::HandleError`
     0xa30610).
3. If the stack is not empty: the error to judge is the stored LastError
   when the step was `handled` or error capture is on, else the step's error.
   - Any code other than 1, and code 1 while some frame on the stack still
     has steps to run: virtual `CheckError(err)` (client: alert unless
     captured, 4.4; server: event log, 4.9).
   - Code 1 (user cancel) when no frame has steps left: no `CheckError`;
     if a transaction is open and the runtime is not paused, and Set Revert
     Transaction on Error is On or the step was Revert Transaction, the
     transaction's error becomes 1 and its error location is recorded.
4. Unless multi-stage: `PostExecute`, clear `mCurrentScriptStep`.
5. Clear the step's target variable name and repetition.

Control steps therefore never change `Get(LastError)` (obs; matches (doc)
`get-lasterror.html`: "If, Else, Else If, End If, Loop, Exit Loop If, End
Loop, Exit Script, and Halt Script" do not clear it). Disabled steps and
comments do not either, because they never execute (2.2).

### 2.5 Valid, obsolete and gated step ids

- Valid ids are 0..247 (`ScriptStep::IsStepIDValid` 0xe7ef80: absolute value
  below 248) (obs). FileMaker Pro 26.1.1 accepts 0..250 (Mac `FMEngine`
  0x523c34: below 251), for its steps 249 and 250 (script-steps.md 12.4)
  (obs); its obsolete and gated ids are those below.
- Obsolete ids (`IsStepIDObsolete` 0xe7f1a0): 15, 52, 54, 58, 59, 78, 100,
  153, 176, 198, 204 (obs). They fail with error 4.
- Feature-gated ids: 224 (Execute URL) and 239 (Validate Scripts) count as
  obsolete unless a debug bit (0x82 resp. 0x7c) is set, so in the shipping
  26.0.3 build they fail with error 4 (obs). Claris has no pages for them.
- Id 186 is the plug-in (external) script step; its plug-in id and the
  function id (the step value) are recorded in the error location (obs).

### 2.6 Limits

- No step-count limit and no fixed recursion depth (obs). Each CallScript
  checks `ScriptRuntimeModel::OutOfStackErrorCheck` (0xa72d40): once the
  stack holds 100 or more frames, every 100th check estimates the stack's
  memory (about 0x788 bytes per script frame plus its parameter, 0x128 per
  step frame, plus all local variables and their values); above 1 GiB the
  call fails with error 2 "Memory error" (obs). (run 13.14: the first call
  made with 100 frames on the stack and every 100th after it look; the
  estimate covers the frames already on the stack, parameters and `$`
  variables at 2 bytes per code point plus a fixed part each, not results.)
- Script parameters and results passed to and from PSOS are limited to
  1,000,000 characters (11.2) (obs; (doc) `running-scripts-on-server.html`).
- OpenMaker Server's limits on a script (steps, time, scripts at once;
  `ScriptLimits`, Implementation notes (PSOS)) are its own and are not
  FileMaker behavior. They stop a calculation in the middle of a step too.

## 3. Control flow

### 3.1 Cached jump indexes

The 16-bit step value (descriptor +0x16) of flow steps holds a step index
computed by the Script Workspace when the script is saved. The runtime uses
these cached values directly; it never searches for the matching step (obs:
no "find matching" helper is called from the flow handlers). Indexes are
0-based positions in the step list.

| Step | Stored step value | Second stored index | Runtime use |
|---|---|---|---|
| If (68) | index of the next same-level Else If / Else / End If | | when false: continue at that step |
| Else If (125) | index of the next same-level Else If / Else / End If | numeric slot 0 (`[00 00, 03]`, 4-byte int): index of the matching End If | evaluated only if no earlier clause was taken; otherwise jump to End If |
| Else (69) | index of the matching End If | | when an earlier clause was taken: continue at End If |
| End If (70) | (none used) | | clears the clause flag |
| Loop (71) | index of the matching End Loop | | not used for jumping (inf: kept for the editor) |
| Exit Loop If (72) | index of the matching End Loop | | when true: continue after End Loop |
| End Loop (73) | index of the matching Loop | | continue at the step after Loop |
| Go to Record/Request/Page (16), Go to Portal Row (99) with "Exit after last" | | numeric slot 0: index of the enclosing End Loop, or -1 | after the last record: continue after that End Loop, or exit the script when -1 |
| Open Transaction (205) | index of the matching Commit Transaction | | stored as `mEndStepIndex` (8.2) |

Corpus check (obs, shipped templates via `re/work/scripting/steps/steps.tsv`):
Else If steps carry `0000/03 = <4-byte end-if index>`; Loop 21 / Exit Loop
If 24 hold 28 and End Loop 28 holds 21; every Go to Record and Go to Portal
Row step stores `0000/03 = ffffffff` (none of the corpus steps uses Exit
after last inside a loop).

Jump arithmetic (obs, `DoIfStep` 0xa78600, `DoElseIfStep` 0xa786f0,
`DoElseStep` 0xa787f0, End Loop case in `Execute`,
`SetNewExecutionStep` 0xa78d80, `ProcessExitLoop` 0xabc7a0), with
`count` = number of steps in the frame's list and the index rule of 1.3:

- "Continue at step v" (If false, Else If chain, Else skip): the frame index
  becomes `v - 1` when `v` is between 1 and 0x8000, capped at `count`; a
  value above 0x8000 (negative as i16) also sets the index to `count`.
- "Continue after step v" (Exit Loop If, End Loop to Loop, exit-after-last,
  Open Transaction end): the index becomes `v`, or `count` when `v` is below
  -1 or not below `count`.
- An index of `count` ends the script normally at the next fetch.

Stale or corrupt indexes (inf from the arithmetic; FileMaker relies on the
editor to keep them right):

- A value at or beyond the step count ends the script silently.
- A value of 0 in If/Else/Else If gives index -1, so execution restarts at
  step 0 (an infinite loop if the condition stays false). (run: an Else If
  holding End If index 0 restarts the script at step 0, 13.13.)
- A value pointing at an unrelated step continues there with no
  diagnostics.
- OpenMaker should recompute the indexes when it saves a script and may
  validate them on load; to be byte compatible it must write exactly the
  values above. FileMaker's writer is `ScanScript` (ScriptUtility.cpp); its
  output matches the rules above, and FileMaker's XML importer
  (FMUpgradeTool) writes 0 instead (run, 13.13).

### 3.2 If / Else If / Else / End If

State: `mClauseEvaluateTrue` (one flag per runtime, not per nesting level)
and `mLastExecutedStepID` (obs).

- If: evaluate calculation slot 0; the flag = boolean result; if false,
  continue at the step value.
- Else If: if the previously executed step was If or Else If and the flag
  is false, behave exactly like If (evaluate, set flag, maybe jump).
  Otherwise (an earlier clause ran, or control arrived from elsewhere) jump
  to the End If index from numeric slot 0.
- Else: if the previously executed step was If or Else If and the flag is
  false, fall into the Else body; otherwise continue at the step value
  (End If).
- End If: flag = false.

Because a taken branch ends with a jump from the next Else If/Else (whose
previous step is a body step, not If/Else If), one flag suffices for
nesting (inf; run 13.3). The calculation's error is discarded by the step
and the step does not touch `Get(LastError)` (obs: control step; run 13.3).
Boolean conversion is `DBData::GetAsBoolean` (calculation spec); a failing
calculation yields `?`, whose boolean value is 1, so a condition that fails
is true (run 13.3; the earlier reading "evaluates to false" was wrong).

### 3.3 Loop / Exit Loop If / End Loop

- Loop: reads the Flush option when flag 0x100 (kOption) is set (list value
  at slot 1). Values 2 and 3 (Minimum, Defer; inf order Always=1,
  Minimum=2, Defer=3 as listed in `loop.html`) put the window's record
  transaction into deferred-flush mode and increment `mInLoopCount`; any
  other value, or when the debugger's data viewer is visible, uses 1
  (Always). The option is ignored while a script transaction is open (obs,
  `DoLoopStart` 0xa78870).
- Exit Loop If: evaluate slot 0; if true continue after End Loop and run the
  loop-exit bookkeeping (`DoLoopEnd` 0xa78d00: decrement `mInLoopCount`; at
  0 process deferred notifications and restore Always). If the jump crosses
  the open transaction's Commit Transaction (the transaction was opened in
  this frame and its end index lies before the target) the transaction is
  reverted (obs).
- End Loop: continue after the Loop step (the Loop step itself is not
  re-executed, so its Flush option is applied once per loop entry) (obs).
- Exit after last (Go to Record/Request/Page, Go to Portal Row with flag
  0x2000 kExit): see 3.1 (obs `ProcessExitLoop`).

### 3.4 Exit Script

`ExitScript` (0xa73b20) (obs):

1. If flag 0x4000 (kCalculation) is set, evaluate slot 0 (the Text Result):
   - for a frame that was not started by a trigger, the result is stored in
     the calling script frame's `mScriptResult` (the nearest script frame
     below; a button single-step frame is skipped);
   - on the server, if the exiting frame is the PSOS/schedule top frame,
     `SMScriptRuntime::OnFinalScriptResult` copies the text into the PSOS
     reply buffer instead;
   - for a trigger frame or a frame without caller, the result's boolean
     value becomes the frame's return value (7.4).
2. Pop a button single-step frame if it is current, then `ExitStackFrame`.

Without a Text Result the caller's `mScriptResult` is left unchanged
(inf from the code path; (doc) `exit-script.html` "stored in the main script
until another sub-script that returns a value is called").

`ExitStackFrame` (0xa75840) (obs): saves the runtime flags into the frame,
drops temporary full access, hands a trigger frame's state to the pending
trigger chain (7.4) with its boolean result (a False result also sets
`Get(LastError)` to 20 "Command/operation canceled by triggered script"),
and then either halts (bottom frame: `OnExitFinalStackFrame` + `HaltScript`)
or pops to the caller. When the exited frame was a trigger frame, the
caller's saved Allow User Abort and Error Capture states are restored;
after a normal sub-script return they are not, so a sub-script's Set Error
Capture / Allow User Abort change remains in force in the caller (obs).

### 3.5 Halt Script

`HaltScript` (0xa74730, case 90 calls it after `ClearFileClosingFlags`)
(obs): resets Allow User Abort to On, Error Capture to Off, the step action,
Set Layout Object Animation to Off and Set Revert Transaction on Error to
On; ends temporary full access; reverts an open script transaction; clears
the whole stack (all callers and queued frames); clears running, paused,
queued and multi-stage state; notifies observers. `Get(LastError)` is
unchanged (control step). The same reset runs when the bottom frame ends
normally, so these states return to their defaults after every top-level
script (obs; (doc) `set-revert-transaction-on-error.html` "When the current
script ends, the state returns to On").

### 3.6 Perform Script

`DoPerformScript(isPSOS)` (0xa8b2c0) handles 1 and 164 (obs):

1. Resolve the target (`GetPerformScriptFileRefKeyScriptKey`): either the
   stored file reference and script key ("From list"), or by name, a
   calculation whose text is `script` or `datasource::script`
   ((doc) `perform-script.html`: script name not case sensitive; data source
   and script names case sensitive in the `::` form). Failure: the
   resolution error, default 100 "File is missing".
2. Find or open the target file's window (`WindowFromFileRef`); the
   sub-script runs in that window's context.
3. Parameter: if flag 0x4000 is set, evaluate slot 0 in the calling
   window's context into a new value (evaluated once, at call time).
4. Perform Script (1): queue mode = 0 (Continue, i.e. push on top and run
   now) when called from a script; when the current frame is a button
   single-step frame, the mode is the step value, or 4 (Pause) when the
   step value is 0 (obs; button storage of the "current script" option is
   for `script-steps.md`). Then `CallScript` (3.7).
5. If the calling script runs with full access, the new frame's
   `mParentRunWithFullAccess` is set (9.1).
6. PSOS (164): see 11.

### 3.7 CallScript and the queue modes

`CallScript` (0xa72f70) (obs), errors in this order:

| Check | Error |
|---|---|
| not triggered, no step running and `OkToPerformScript()` false | 1 |
| script key does not exist, or the catalog entry is a folder or separator | 104 "Script is missing" |
| account lacks access to the script (no global access, script privilege "no access") | 9 "Insufficient privileges" |
| target window's file not open/usable, or the window is in mode 2 (inf: Layout mode) and leaving it failed | 5 "Command is invalid" |
| stack memory check (2.6) | 2 "Memory error" |

On success it creates the script frame (copying the current Allow User
Abort and Error Capture states into it), attaches an optional result buffer
(PSOS server side), queues it (`QueueScript`), and optionally presets
`Get(LastError)` for the new script (`SetLastError(code, force)`; used for
PSOS callback scripts, 11.4).

`QueueScript` (0xa735b0) / `SetupRuntimeState` (0xa73890) apply the
"current script" option (obs; values from (doc)
`options-for-starting-scripts.html`):

| Value | Name | Runtime effect on the running script |
|---|---|---|
| 0 | Continue | none; the new script is pushed (or queued, below) |
| 1 | Halt | `HaltScript` first (stack cleared) |
| 2 | Exit | exit the current script (`ExitStackFrame(1, true)`), returning to its caller |
| 3 | Resume | resume a paused script (or, when not paused, save state and fall through to Pause) |
| 4 | Pause | mark the runtime paused indefinitely (`mPauseTime` = -1) and suspend |
| 5 | Suspend and Resume | (doc only; no separate branch was identified; run 13.11: acts as 0 in the server runtime) |

When the stack is empty at queue time, the runtime first resets
`Get(LastError)` to 0, the last error location, the SSL error and the
transaction error location (obs). A frame is pushed on top (runs next) when
the caller asked for immediate execution or the stack is empty; otherwise it
is inserted below the running frames (`ScriptStackPush(frame, atBottom=1)`)
and runs after the current script finishes (obs; used for delayed triggers,
OnTimer and PSOS callbacks).

### 3.8 Parameters, results and script name

- `Get(ScriptParameter)`: the current script frame's `mScriptParameter`,
  fixed at call time (obs `GetScriptParameter` 0xa87560). A button
  single-step frame has none.
- `Get(ScriptResult)`: the current frame's `mScriptResult` (obs 0xa875a0).
  It is set by a sub-script's Exit Script with a result (3.4), by a
  completed PSOS with Wait for completion (11.3; cleared by an asynchronous
  PSOS), and by the PSOS callback (11.4). Frames start with no result.
- `Get(ScriptName)`: name of the current script frame
  (`ScriptStackElementScript::GetName` 0xb0aa00) (inf: the Get selector
  itself was not traced).

## 4. Errors

### 4.1 Representation

`DBError` is a u16 code plus a u16 index into a table of extended error
objects (`ExtendedError`, `DBFileError`, `ErrorString`) that carry detail
text (obs). `Get(LastErrorDetail)` is built from the extended object and the
last ODBC/SSL error (`GetLastErrorDetail` 0xa71d10); `Get(LastErrorLocation)`
from `mLastErrorLocation` (4.6).

### 4.2 `SetLastError(err, log, force)` (0xa6f120)

(obs)

1. Ignored unless a script is running or `force` is set.
2. Ignored for codes 3000-4999 and codes 5500 and above: internal codes
   never become `Get(LastError)`. Codes below 3000 and the user range
   5000-5499 pass.
3. The code goes through the virtual `MapError` (4.5).
4. It is stored unless the stored LastError is non-zero, the new code is 1
   (user cancel) and `force` is not set: a cancel never hides an earlier
   error ((doc) `get-lasterror.html`).
5. On store: observers are notified; for a non-zero code the error location
   (4.6) is recorded; an ODBC error text is captured if pending; the
   `OnSetLastError` hook runs.
6. If `log`: `LogErrorState` writes the Set Error Logging entry (4.8).

Consequences for implementers: every executed non-control step sets
LastError (0 on success); control steps, comments, disabled steps and
transaction-skipped steps leave it; a new top-level script starts with 0
(3.7).

### 4.3 Error codes produced by the runtime itself

| Code | When (obs unless marked) |
|---|---|
| -1 | step id 0 |
| 1 | server abort; CallScript refused by `OkToPerformScript` |
| 2 | stack memory over 1 GiB (2.6) |
| 3 | step not implemented for this runtime (10); PSOS when the file is not hosted; Open Transaction while one is open; Commit/Revert Transaction without an open transaction or outside its scope; PSOS reply length out of range |
| 4 | obsolete or unknown id (2.5); also the platform hook's "not mine" signal |
| 5 | invalid variable name (5.2); target window not usable in CallScript |
| 9 | no privilege to run the script |
| 20 | a trigger script returned False (3.4) |
| 100 | Perform Script target file could not be resolved (default) |
| 104 | script missing, folder or separator |
| 112 | server: no window for the file (`SMScriptRuntime::ExecuteScript`) |
| 513 | PSOS parameter longer than 1,000,000 characters |
| 736 | PSOS callback result longer than 1,000,000 characters |
| 812 | server: PSOS refused because the script engine is at its concurrent-script limit (`SASEPerformScriptOnHost::StartScript` in libFMSLib) |
| 5000-5499 | Revert Transaction with a custom error code (8.3) |
| 4100 | internal "no target field" (2.3 check 3); not stored in LastError |
| 3000, 3001 | internal retry / multi-stage (1.7) |

### 4.4 Error capture and dialogs

- Set Error Capture (86) sets `mCaptureErrors` from flag 0x20000 (kTurnOn);
  Allow User Abort (85) sets `mAllowAbort` from the same flag and notifies
  the UI (obs). Both are runtime-wide, inherited by sub-scripts, persist
  after a sub-script returns, are restored after a trigger script, and are
  reset by Halt / end of the top script (3.4, 3.5).
- After an error, PostStepCleanup calls the virtual `CheckError` (2.4). The
  client implementation is not in the Linux build. From the shared helpers
  (obs) and docs (doc), a compatible client:
  - shows no alert when capture is on, except for errors that are never
    suppressed: `NeverSuppressError` (0xa6ebc0) is true for internal codes
    3000 and above outside 5000-5499, and for 800, 801, 804, 805, 808, 811,
    814 and 200 raised by Export Records (36) when "With dialog" is on
    (flag 0x80 clear);
  - uses a continue/cancel style alert for the codes of
    `SharedErrorsThatCauseConfirmAlert` (0xa6eb50): 2, 4, 100, 104, 204,
    300, 404, 406, 802, 809, 810, 814 (inf: "confirm" alert, text unknown);
  - an unknown step (error 4) stops the script when Allow User Abort is On
    and is skipped when Off ((doc) `allow-user-abort.html`; the server code
    does exactly this, 4.9).
- Server, WebDirect and Data API runtimes never show dialogs and behave as
  if capture were on ((doc) `set-error-capture.html`, `running-scripts-on-server.html`).

### 4.5 MapError (0xa6ec20)

Maps internal engine codes to documented ones before storing (obs, decoded
switch table 0x10e5ad8):

| Internal | Public | Internal | Public |
|---|---|---|---|
| 2008 | 212 | 2045 | 100 (after a debug message) |
| 2009, 2020 | 409 | 2047 | 711 |
| 2023, 2056, 2059 | 3 | 2048 | 730 |
| 2024 | 100 | 2050 | 130 |
| 2034 | 720 | 2051 | 600 |
| 2035 | 718 | 2052, 2062 | 401 |
| 2040 | 201 | 2053 | 400 |
| 2054, 2068 | 102 | 2055 | 407 |
| 2058 | 821 | 2060, 2061 | 714 |
| 802 | 100 when the file error detail is 0x4fb5, else 802 | others | unchanged |

OpenMaker produces public codes directly and needs this table only if it
adopts the same internal numbering (inf). `iwp::IWPScriptRuntime` overrides
`MapError` (not decoded).

### 4.6 `Get(LastErrorLocation)` and `Get(LastErrorDetail)`

- Location (obs `ErrorLocation::GetString` 0xa6e150): empty when no line is
  recorded; otherwise `script name` CR `step name` CR `line number`. The
  line number is the 1-based step index (index + 1) in the current frame;
  the step name is the localized step name (for plug-in steps, id 186, the
  plug-in's step name from plug-in id and function id).
- For transactions the separate transaction error location is reported
  ((doc) `get-lasterrorlocation.html`: failing script, Commit Transaction
  step and line; for custom 5000-5499 reverts the custom string).
- Detail: ODBC driver text (errors 1408/1409), SSL certificate text, plug-in
  text, cURL messages, record-lock holder, transaction failure description,
  custom revert message ((doc) `get-lasterrordetail.html`;
  obs `GetLastErrorDetail`, `GetLastTransactionError` 0xa722a0).

### 4.7 Set Revert Transaction on Error

See 8.4.

### 4.8 Set Error Logging (200)

Client: requires full access; logs every non-zero step error of scripts in
the file to `ScriptErrors.log` (tab-separated: timestamp, session id, file,
account, `script [index]`, `step [line]`, error code, custom debug info
calculation evaluated at log time) until the file closes or logging is
turned off ((doc) `set-error-logging.html`; obs `DoSetErrorLogging`
0xa962a0, `LogErrorState`, `PrepareStepErrorLogFile`, `CheckLogRollover`).
Server script engine: the step returns error 3 unless the server-side
setting `sAllowServerScriptErrorLogging` is on (obs, fmsased).

### 4.9 Server error reporting

`SMScriptRuntime::CheckError` (0xa65640) (obs): for a mapped error below
3000 or in 5000-5499 it formats a line from the file's base name, the script
name and the step name (`ScriptStep::GetName`) and passes it with the code
(run 13.2: `file : script : line : step name`, independent of Set Error
Capture) to `SMSession::LogOrCountError` (event log with repeat counting,
`SMSession::FlushErrorCache`). If the code is 4 (unknown step), Allow User
Abort is On and the script is not merely queued, the script is aborted and
all triggers are disabled for the session (`mMasterTriggerSwitch` = 0).
Error 3 does not stop a server script; the step is skipped and the script
continues ((doc) `running-scripts-on-server.html`).

## 5. Variables

### 5.1 Scopes and lifetime

- `$name`: local to a script frame. Each `ScriptStackElementScript` owns a
  `DBVarMap` (`mScopedVars`); a sub-script starts with an empty map and the
  caller's map is untouched (obs). Variables vanish when the frame ends.
  The lookup uses the nearest script frame at or below the current frame
  (`GetScopedVars` 0xae28b0), so a button single-step frame sees its
  caller's variables (obs).
- `$$name`: global per file and per session: `DBFileUser::GetGlobalVars`
  (client and FMSE file users; on the host side `NetHostInternalFileUser`)
  (obs); cleared when the file closes ((doc) `using-variables.html`). Each
  PSOS runs in its own FMSE session, so it starts with no `$$` variables
  ((doc) `running-scripts-on-server.html`).
- Let variables without `$` are calculation-local (calculation spec).
  `Let ( $x = ...)` writes the current frame's map through the same
  `DBVarMap` API (inf).

### 5.2 Names and repetitions

`VariableHelper` (0xac47d0) builds the map key as
`name + "[" + repetition + "]"` for every access (repetition formatted as a
number, default 1), validates it with `DBVarMap::ValidVariableName`
(libDBEngine 0xf53510: at least two characters, leading `$` or `$$`, the
base name a valid calculation name, a bracketed repetition at the end) and
selects the global map when the second character is `$`, the local map
otherwise (obs). An invalid name fails the step with error 5 (obs). So
`$x` and `$x[1]` are the same variable and each repetition is an independent
entry (inf).

### 5.3 Set Variable (141) and other writers

`DoSetVariable` (0xa7a030) (obs): the name is text data slot 0, the
repetition calculation slot 2 (empty means 1), the value calculation slot 1.
`SetVariable` (0xac49c0):

- an empty value with replace semantics deletes the variable (an empty
  variable and a missing one are indistinguishable);
- otherwise the value, with its own data type, replaces the old one (Set
  Variable always replaces) and is converted for the file
  (`DBData::Translate`);
- steps that insert into a variable without replacing (Insert Text etc. with
  a variable target) append text to an existing value unless that value
  has data type 6 (inf: container), which is replaced (replace=false path)
  (obs; which steps use it is for `script-steps.md`).

Calculation errors in the value are not reported by Set Variable as a step
error (the evaluation error is dropped) (obs; the value becomes whatever the
evaluator returned).

## 6. Abort, pause and script state

### 6.1 Allow User Abort

See 4.4. Client: Esc / Cmd-period stops the script when On (doc). Server:
users cannot stop server scripts; the administrator can disconnect the FMSE
session, which `SMScriptRuntime::IsServerAbort` (0xa65030, session flag
+0xd01) reports, making the next step fail with 1 and abort (obs). The flag
also governs unknown-step handling (4.9).

### 6.2 Pause/Resume Script (62)

`DoPauseResume` (0xac4400) (obs):

- Executed from a button single-step frame (no script frame current): acts
  as "resume": exits the step frame and resumes a paused script.
- In a running script: LastError = 0; duration = -1 (indefinite) unless flag
  0x4000 is set, in which case calculation slot 0 is evaluated as a number
  of seconds (a non-numeric result means indefinite); the pause start tick
  is recorded; the runtime becomes paused (`mPaused`, not running) and the
  script waits.
- Resume happens when the user continues (client UI), when another script
  is started with Resume/Exit/Halt (3.7), or when the duration elapses
  (`DoPauseIdle`).
- Server: `SMScriptRuntime::GetPauseTime` (0xa651b0) turns an indefinite
  pause into 0.1 s, and `SMScriptRuntime::OnIdle` (0xa651e0) sleeps in
  slices of at most 100 ms until the time has elapsed, aborting if the
  session was disconnected (obs). WebDirect and Data API have their own
  handlers (fmwipd routes 62 specially; doc says avoid it there).

### 6.3 Freeze Window, animation

Set Layout Object Animation (168) sets `mAllowAnimation` from kTurnOn (obs).
Pause/Resume unfreezes a frozen window (doc).

## 7. Script triggers

### 7.1 Event ids

DWARF enum `TriggerEventID` (obs) with the user-facing names
(`script-triggers.csv` order differs; this id is what the runtime uses and
what `TriggerState::mTriggerID` holds). "Delayed" = `ScriptTrigger::IsDelayedTrigger`
(0xfea3c0) true (obs); delayed triggers run after the event is processed
and are queued behind running scripts; the others run before the event and
their result can cancel it ((doc) trigger pages, "When the script runs").

| Id | Trigger | Delayed | Cancels on False (doc) |
|---|---|---|---|
| 1 | OnObjectEnter | yes | no |
| 2 | OnObjectExit | no | yes |
| 3 | OnObjectModify | yes | no |
| 4 | OnObjectSave | no | yes (object stays saved) |
| 5 | OnObjectKeystroke | no | yes |
| 6 | OnObjectValidate | no | yes |
| 7 | OnPanelSwitch | no | yes |
| 8 | OnObjectAVPlayerChange | yes | no |
| 101 | OnLayoutEnter | yes | no |
| 102 | OnLayoutKeystroke | no | yes |
| 103 | OnRecordLoad | yes | no |
| 104 | OnRecordCommit | no | yes |
| 105 | OnRecordRevert | no | yes |
| 106 | OnModeEnter | yes | no |
| 107 | OnModeExit | no | yes |
| 108 | (old view change) | yes | |
| 109 | OnLayoutExit | no | yes |
| 110 | OnViewChange | yes | no |
| 111 | OnLayoutSizeChange | yes | no |
| 112 | OnGestureTap | no | yes |
| 113 | OnExternalCommandReceived | no | no (doc: after event) |
| 201 | OnFirstWindowOpen | yes | no |
| 202 | OnLastWindowClose | no | yes |
| 203 | OnTimer (Install OnTimer Script) | yes | |
| 204 | OnWindowOpen | yes | no |
| 205 | OnWindowClose | no | yes |
| 206 | OnFileAVPlayerChange | yes | no |
| 207 | location (region monitor) | no | |
| 208 | PSOS callback script | yes | |
| 209 | OnWindowTransaction | yes | no |

Keyboard commit: [Committing data in records](https://help.claris.com/en/pro-help/content/committing-data.html)
(doc) identifies numeric-keypad Enter / Ctrl+Enter on Windows and Enter /
Fn-Return on macOS, separately from Return. [OnObjectSave](https://help.claris.com/en/pro-help/content/onobjectsave.html)
(doc) runs after validation and saving of changed object data, before
field exit; False cancels the initiating event while leaving the object
saved and its field active.

One local FileMaker Pro 26.0.3 observation (2026-11-17, #1070/#1181):
a user changed an ordinary Text field from `old` to `new`, then sent one
macOS keypad Enter (key code 76). Marker scripts for OnObjectSave,
OnObjectExit and OnRecordCommit appended `|S`, `|E` and `|C` to a global
Text field with Set Field and explicitly returned True. Before any other
click, navigation, acknowledgement or close, the first settled window
showed `|S|E|C`; accessibility focus had left the edited text field for the
Browse content area, with no selection/caret range available there.
Normal close added no marker. The closed file held `new` and the trace in
its global storage `[8001, 08, 07]`, raw XOR bytes `2609261f2619`.
Full active field definitions, occurrences, layouts/objects/triggers and
script catalogs were unchanged by path; only nine data/bookkeeping paths
changed or appeared. Evidence: the private native result packet, RESULT
SHA-256 `c44597c4b6842b9ecfb59470f7c6f5cc700308ff20a5f42a741e0e54cf271929`,
with pinned input/application hashes, pre/post-key accessibility records,
screenshot and full active-path diff. The global marker writes are fixture
instrumentation, not an uninstrumented application observation.

Scope/unknowns: this establishes Save/Exit/Commit order and focus departure
for one ordinary local Text-field keypad-Enter action. OnObjectValidate
was not instrumented; its position before Save follows the documented
validation/save contract. Hosted files, portals, other field types,
Windows and other commit/shortcut paths were not measured by this case.

### 7.2 Storage

A layout or layout object carries a `ScriptTriggerCache` of `ScriptTrigger`
records (DWARF size 0x40): `mTriggerID` (u16 event id), `mScriptKey`,
`mFileRefKey`, `mScriptParamCalcFormula`, `mTextP`, `mFlags` (u32, enabled
modes: 1 Browse, 2 Find, 4 Preview; enum `kEnableBrowseMode`/`kEnableFindMode`/
`kEnablePreviewMode`, 7 = all), `mOption1`, extra bytes (obs). The stored
cache and record (`ScriptTrigger::Read` 0xfe9de0, `ScriptTriggerCache::Read`
0xfea760), the parameter formula and the file triggers (document
preferences `[2, 18]`) are specified in [file-metadata.md](file-metadata.md)
sections 3 and 7.

### 7.3 Firing (`FMWindowModel::CallScriptTrigger`)

(obs)

1. The window's suppression mask (window+0x318) is tested against the
   caller's flag; a suppressed trigger does not fire.
2. The trigger record with the event id is looked up in the layout (or the
   object's) cache.
3. Mode filter: Browse (window mode 0) needs flag 1, Find (1) flag 2,
   Preview (3) flag 4 (`TriggerAllowedInCurrentMode`).
4. `AllowTriggers` (0xa724e0): false when the master trigger switch is off;
   when the debugger disabled user triggers, only triggers caused by a
   running script step fire. Triggers are NOT suppressed merely because a
   script is running: steps such as Go to Layout or Go to Object fire them
   ((doc) `script-triggers.html`), with the exceptions listed in
   `actions-dont-activate-triggers.html` (bulk operations; Set Field, Set
   Field By Name, Set Web Viewer, Replace Field Contents, Import, Relookup
   do not fire OnObjectModify, etc.).
5. A `TriggerState` is created (source and trigger window, file, event id,
   flags, focused object), the script is called through
   `ResolveScriptFileAndCallScript` with the trigger's parameter
   calculation evaluated now, and the call error goes through
   `HandleError`; `Get(LastError)` is then reset to 0.
6. Window-level triggers additionally require `OKToFireWindowTrigger`
   (0xa48160: no script running or queued, not paused, and a per-file
   state check of the window's file and its parent window passes; inf: no
   modal file operation in progress).

### 7.4 Event actions and cancellation

The default action of a pre-event trigger (commit, go to layout, close
window, go to object, delete row, find, ...; classes `CommitTrigger`,
`GotoLayoutTrigger`, `CloseWindowTrigger`, `SaveFieldTrigger`, ...) is
captured in the `TriggerState` (`CaptureTriggerAction`) instead of being
performed immediately (obs). When the trigger script's frame ends
(`ExitStackFrame`), its state moves to the runtime's pending chain with
`mRetryTriggerAction` = the script's boolean result: Exit Script without a
result, or ending without Exit Script, means True; `Exit Script [False]`
(any result whose boolean value is false) means False and sets
`Get(LastError)` = 20 (obs). After each step, `DoTriggerAction` (0xa75fb0)
post-processes pending states and, for those whose result is True,
performs the captured action (`TriggerAction::PerformAction`); False drops
it, cancelling the event. States created while another script frame is on
the stack are re-attached to that frame and processed when it ends (obs).
Run (13.17): when a script step caused the event, the step returned 3000
and the performed action is the step running again; only triggers that run
before their event store 20 for a False result (also for a text or empty
result); a post-event trigger's result is ignored.

## 8. Transactions

### 8.1 Open Transaction (205)

`DoBeginTransaction` (0xa96930) (obs):

1. Exit (commit) the active object/record in the window; an error stops
   here (3000 retries).
2. If a transaction is already open: error 3 when it belongs to another
   frame (nested Open Transaction in a sub-script: (doc) "skipped and
   return error 3"), error -1 otherwise (run 13.10: a second Open
   Transaction in the same frame also returns 3).
3. Start: `ScriptTransactionMgr::BeginTransaction(window)` marks it open,
   clears error/abort/skip state, binds the window's DB transaction, and
   remembers the opening frame. The transaction error location is cleared.
4. Options from step flags:
   - 0x100 (kOption): Skip data entry validation (`mSkipValidateDataEntry`);
   - 0x200 (kOption2): Override ESS locking conflicts (DB transaction
     "allow dirty writes", the same field Commit Records' 0x200 option sets);
   - 0x1000: Skip auto-enter options (DB transaction "skip auto enter").
   (obs for the field writes; the option names are matched by the
   `ScriptTransactionMgr::SetAllowDirtyWrites`/`SetSkipAutoEnter` setters
   and `open-transaction.html`.)
5. If starting failed with an error, `mSkip` is set and the steps inside
   the transaction are skipped (2.3 check 4) (obs; (doc) "If it can't exit
   the active record, Open Transaction returns an error and the script steps
   within the transaction are skipped").
6. `mEndStepIndex` = the step value (index of Commit Transaction).

### 8.2 Commit Transaction (206) and the skip rule

`InternalCommitOrAbortTransaction(commit=true)` (0xae0cf0) (obs):

- error 3 if no transaction manager exists, it is not open, or (for commit)
  the current frame is not the opening frame;
- when the transaction was skipped or reverted, nothing is committed and the
  recorded error is returned;
- otherwise: commit the active record, exit popover and portal; record any
  error as the transaction error; commit the DB transaction (passing the
  Skip-validation option); on a commit error the DB transaction is aborted
  and the transaction error location recorded; in all cases the manager is
  closed.
- (doc) Commit does not fire OnRecordCommit/OnRecordRevert; auto-enter runs
  at commit unless skipped; on success OnWindowTransaction (209) fires with a
  JSON parameter describing the committed records.

While a transaction is open and has been reverted, or has an error with
Set Revert Transaction on Error On, every step other than Open Transaction
and Commit Transaction is skipped without changing LastError (2.3 check 4).
That is how "revert jumps to the matching Commit Transaction" is realized
(obs).

### 8.3 Revert Transaction (207)

`DoAbortTransaction` (0xa96ce0) (obs):

- flag 0x100 (kOption): a Condition is stored; when it evaluates false the
  step does nothing;
- otherwise revert (`InternalCommitOrAbortTransaction(false)`, allowed from
  a sub-script of the opening frame) and mark the transaction aborted;
- flag 0x200 (kOption2): calculation slot 1 is the custom error code; if it
  is a number in 5000-5499 it becomes the step error and the transaction
  error, with calculation slot 2 (text) as the custom message for
  `Get(LastErrorDetail)`/`Get(LastErrorLocation)`; other values are ignored;
- (doc) Revert Transaction exits the active record, does not revert serial
  numbers or Truncate Table, and does not fire OnRecordRevert.

### 8.4 Set Revert Transaction on Error (223) and automatic revert

`mAbortImmediately` = kTurnOn (obs); default On; reset to On by Halt / end
of the top script (3.5). With it On, an error recorded in the transaction
(`SetTransactionError` 0xa7e270: any non-zero error except 3000 while the
transaction is open and the runtime not paused) makes the following steps
skip to Commit Transaction, which then reverts and returns the error (obs).
Only the data layer records errors there (callers: `InternalSetField`,
`DataEntryModel::CreateRow`/`DeleteRow`/`DeletePortalRowInternal`/
`DeleteAllRows`/`OpenRow`/`ReplaceRecords`/`RelookupRecords`,
`ActiveObjectBase::SaveField`/`GrabWriteAccess`, import, Begin/Abort/Commit)
(obs); an ordinary step error such as 104 or 101 does not revert (run 13.10).
User cancel (1) inside a transaction also records error 1 (2.4). Halt
Script, a cancelled script and exiting a loop past the Commit Transaction
revert the transaction (obs 3.3, 3.5).

### 8.5 Get(TransactionOpenState), nesting, other windows

- `Get(TransactionOpenState)` is 1 while the manager is open (inf; calc
  spec should read the runtime's manager).
- Only one script transaction exists per runtime; a sub-script runs inside
  the caller's transaction and its own Open/Commit Transaction steps return
  3 (obs 8.1/8.2; (doc) `open-transaction.html`).
- Changes made in other windows are outside the transaction ((doc)).
- `ScriptRuntimeBase::IsTransactionOpenInWindow` and
  `ForceCommitTransaction` support automatic commit when Manage Database,
  Save a Copy as or Re-Login run during a transaction ((doc) list in
  `open-transaction.html`; obs symbols).

## 9. Privileges and context

### 9.1 Run script with full access privileges

The flag is bit 0 of the script catalog member's private flags
(`DBCatalogMember::GetPrivateMemberFlags`, FMScript vtable slot 0x150) (obs).
Behavior (obs):

- Before each step of such a script the file user's privilege set is
  replaced by full access (`DBFileUser::SetPrivileges`, override flag 0x100
  on the file user) and restored after the step (`FMFileUser::SetOverrideFullAccess`),
  also on Halt and frame exit.
- A sub-script called from it records `mParentRunWithFullAccess`; this only
  lets the sub-script's steps execute even if the account has no access to
  that script (2.2). The sub-script itself runs with the account's own
  privileges unless it is also marked full access (inf from the per-step
  elevation).
- `ScriptRuntimeBase::IsRunningWithFullAccess` (0xa7e310) tests the file
  user's override flag; Set Error Logging and account steps require it
  (doc).

### 9.2 Window, layout, found set and record

The script context is the frame's target window: its layout, mode, found
set, sort and current record (obs: frames store only a window and the
original layout key; `ScriptRuntimeBase::GetWindow` resolves the frame's
window). A sub-script in another file runs in a window of that file found
or opened by `WindowFromFileRef`; context changes made by any script are
visible to its caller afterwards (inf). Server scripts start in the FMSE
session's window for the file on the file's default (or last) layout with
all records (doc: "don't have the client's context").

A file a script in it is performed from is opened for it: "There is no
need to open an external file when using a script in it—FileMaker Pro
opens it for you" (doc, `perform-script.html`). The page does not say
whether its window shows. Select Window and a hidden file (doc,
`select-window.html`, documented, not measured):

- Current Window "brings the active window of the file that contains the
  script to the foreground".
- From another file, it "does not open a window of a related file when
  the related file is opened in a hidden state, such as when a file is
  opened because it is the source file of a related field"; the page's
  remedy is Open File, then Select Window by name.
- From within that file, it shows it: "Select Window can make visible a
  hidden related file if the script step is executed from within the
  hidden file."
- By name it searches every open file unless Current file only is set,
  first names, then UUIDs; "Window name or UUID selection is not case
  sensitive."

A hosted solution's navigation relies on the third rule: a button performs
a script in a second file (`Refresh Window`, then `Select Window [ Current
Window ; Current file ]`), then selects that file's window by name. It is
in daily use with FileMaker Pro (owner's report, not measured).

## 10. Client versus server step support

### 10.1 The per-step compatibility table

`sStepsUnstoredFlagsTable` (libFMEngine 0x10ff900, 248 u32, one per step id)
holds each step's compatibility as `ScriptStep::StateFlags` bits (obs, DWARF
enum): 2 Mac, 4 Windows, 8 Server, 0x10 iOS (FileMaker Go), 0x20 Custom Web
(Data API and CWP), 0x40 Instant Web (WebDirect), 0x80 not usable as a
button single step (set for If, Else, End If, Loop, Exit Loop If, End Loop,
Allow User Abort, Set Error Capture, comment, Else If, Set Layout Object
Animation, Open/Commit/Revert Transaction, Set Revert Transaction on Error).
`kAllDeviceCompatible` = 0x7e. `ScriptStep::GetUnstoredStepOptions` reads it
(no caller in the server build; inf: the Script Workspace uses it to grey
out steps).

FileMaker Pro's Mac `FMEngine` holds the same table: 26.0.3's x86_64 slice
at file offset `0x8a7dd0`, with the 248 values of libFMEngine's, and
26.1.1's (SHA-256 `e306cd52...`) at `0x8d7d80` with 251 (obs, 2026-11-19:
found by those values). In 26.1.1, 248 is 0 (no client), 249 Replace Field
Contents by Name and 250 Set Variable by Name are 0x7e (every client), and
96 Save a Copy as Add-on Package gains the Server bit (0x0e; Claris: it
runs on FileMaker Server in 26.1.1); the other 247 are unchanged.

The `IsServerCompatible`, `IsWebDirectCompatible`, `IsCustomWebCompatible`
and `IsWebCompatible` functions (0xe86920 etc.) do not test a step: they
test the global `ScriptStep::m_Compatibility` (0x12d9634), the platform the
Script Workspace's compatibility filter currently shows (true when that
mode includes the bit and is not "all", 0x7e) (obs). The per-script
compatibility choice is stored on the script (`FMScript::GetCompatibility`).

Agreement with Claris: across all named steps the table's Server bit
matches the Claris Server column except Configure Machine Learning Model
(Claris Partial; table and Linux code: not supported); Go and WebDirect
differ only for Move/Resize Window (Claris Partial). The Custom-web bit
matches the Data API column except Check Selection and Open Sharing, and
differs from the CWP column for the data-file steps, Export Field Contents,
Save Records as PDF and Set Error Logging (obs; see the CSV notes).

### 10.2 What actually happens on the server

The table is not consulted at run time (obs). A step runs on the server if
some dispatcher handles it (2.3):

| Layer | Steps |
|---|---|
| `Execute` | 68-73, 75, 85, 86, 89, 90, 103, 125, 134-137, 141, 145, 146, 168, 208, 223, 238 |
| `FMS::SASEScriptRuntime::DispatchStep` (fmsased 0x79bb20) | 3, 35, 36, 42, 117, 132, 144, 152, 157, 225; 200 only when server error logging is allowed; 132 returns 3 when a debug bit is set; Save Records as PDF serializes on a process-wide lock |
| `SMScriptRuntime::DispatchStep` | 44 (Exit Application; inf: closes the session's files via `FMSession::CloseAllFileUsers`), 62 (Pause/Resume), 138 (Re-Login, through a session virtual) |
| `DispatchStep_Private` | every id not in its default list (`re/work/scripting/runtime/dsp_private_jt.txt`) |
| none | error 3, the step is skipped and the script continues |

Steps whose table bit says "not server" but that still reach a shared
handler: 92, 97, 146, 166, 180 (window operations; result on the server not
verified) and 161, 167, 169, 177-179, 202 (the shared handler returns 3).
WebDirect (`fmscwpc`) explicitly returns 3 for 32 and 188-199; the Data API
engine (`fmwipd`) for 11, 29, 92, 97, 98, 101, 131, 158, 159, 161 and
188-199 and handles 10, 62, 117, 132, 144, 157, 200, 225 itself (obs,
decoded jump tables).

`docs/re/catalogs/script-step-compat.csv` lists per id: the table bits,
the Claris columns, the server script-engine dispatch outcome and notes.
For OpenMaker Server, implement exactly the steps whose table Server bit is
set and return 3 for the rest (inf: matches FMSE behavior except the few
reachable-but-unsupported UI steps listed above, which are no-ops or 3).

## 11. Perform Script on Server

### 11.1 Client side

`DoPerformScript(isPSOS=true)` (obs):

1. Resolve script key and parameter as for Perform Script (3.6).
2. The file must be hosted and connected (file user state 3 and connection
   state 3), else error 3.
3. Build `RCPerformScriptOnHost` on the file's connection and call
   `RCPerformScriptOnHost::Call(scriptKey, async, param, &result)`
   (libDBEngine 0x111b240), `async` = step flag 0x100 (kOption). The
   Script Workspace shows this flag inverted as "Wait for completion"
   (`ParameterListFactory::AppendPerformScriptOnServerParameters` passes
   label 448 "Wait for completion" with flag 0x100 and sets the inversion
   field of the boolean parameter) (obs for the runtime, inf for the UI
   inversion): flag clear = wait.
4. The returned error is the step error (so it becomes `Get(LastError)`),
   and the current frame's `mScriptResult` is replaced by the returned
   result (null when asynchronous).
5. When waiting, the client then waits for forwarded change notifications
   and processes them, so records changed by the server script are current
   (`DBEngineController::WaitForNotificationFowarding`,
   `DBTableMgr::ProcessNotifications`) (obs).

### 11.2 Request on the wire (opcode 30)

`RCPerformScriptOnHost` is `RCTransaction` opcode 30 ("Perform Script on
Server", registered name-only in DBEngine; DWARF enum `kOpCode` = 30)
(obs). It travels as a CORBA `Perform` call with the usual 20-byte header
(network-protocol.md 4.1). Body, in push order (the server pops from the
end, network-protocol.md 4.2) (obs, `Call`):

| # | Field | Encoding |
|---|---|---|
| 1 | `Call` argument 8: in PSOS with callback the client callback id (the key of the client's callback map) (only if the connection's protocol version is 9 or more) | u32 BE |
| 2 | `Call` argument 7 (same condition; inf: the callback file-user id) | u32 BE |
| 3 | script parameter | `Push(Text, max 1,000,000)` (styled text) or an empty unistring when there is no parameter |
| 4 | script key bytes | the `HBSerialKey<7>` content bytes, then a 1-byte length (Pascal string pushed bytes-first) |
| 5 | flags | u16 BE; bit 0 = asynchronous (do not wait) (DWARF `kAsyncFlag` = 1) |

A parameter longer than 1,000,000 characters fails before sending with 513
(obs). For plain PSOS both ids are 0 (inf); PSOS with callback fills them
(11.4). Which of the two is the file-user id is inferred from the server's
`ClientInfor` fields (`rCallBackFileUserID`, `rClientCallBackID`).

Reply (header-less result stack, network-protocol.md 5.6.3), in push order
(obs, server `Perform` 0x1118b50; client pops in reverse):

| # | Field | Encoding |
|---|---|---|
| 1 | extended error text | unistring (popped by `PopExtendedError` when the code carries detail) |
| 2 | script result | unistring, max 1,000,000 (client pops it as Text when it waited, discards it when async) |
| 3 | error | u16 BE |

The client treats a reply body of length 0-1 or over 1 MiB as error 3
(obs).

### 11.3 Server side

(obs unless marked)

- `fmserverd` registers `SASEPerformScriptOnHost::CreationFunction` for
  opcode 30. `RCPerformScriptOnHost::Perform` finds the requesting session's
  host file user (error 100 when the file is not open in that session; a
  second failure path sets 109, not analyzed), pops the fields, allocates
  a script id (`sNextSID`), records a `ClientInfor` (session id, file serial,
  connection, callback ids, finished flag, finish time, result, error) in
  `sSIDtoClientInforMap`, and calls the virtual `StartScript(sid, async,
  fileUser, guestFile, scriptKey, param)`.
- `SASEPerformScriptOnHost::StartScript` (libFMSLib) refuses with 812 when
  the number of executing scripts reached `sLimitOfScriptsExecuting`, then
  hands the request, with the client's authentication, to the script engine
  process (`fmsased`), which opens the file in a new FMSE session as the
  same account ((doc) "use the same account as the client").
- In the script engine, `SMScriptRuntime::ExecuteScript(fileUser, name,
  param, resultBuf)` (0xa64ba0): error 112 if the file user has no window,
  104 if the script is not found (`ConvertScriptNameToKey`), else the
  parameter text becomes a text value, `CallScript` runs it with mode 0, the
  bottom frame is remembered as the final frame and `CompleteScript` runs it
  to completion. The returned error is the runtime's `Get(LastError)` at the
  end of the script; the result is the top script's Exit Script text
  (`OnFinalScriptResult`). (run 13.1: so observed, except after an abort on
  error 4, where 4 is returned and `Get(LastError)` reads 1.)
- Synchronous requests: the database server's `Perform` waits on a message
  queue for the `ScriptFinishedMsg` (message code 1: result text and error);
  a timer message (code 100) keeps the wait alive between checks. It then
  builds the reply (11.2).
- Asynchronous requests reply immediately (inf from the code path: the wait
  is skipped when the async flag is set).

### 11.4 Perform Script on Server with Callback (210)

(obs `DoPSOSWithCallback` 0xac14d0 unless marked)

- The client resolves the server script and the callback script
  (`GetCallbackScriptFileRefKeyScriptKey`), records a
  `PSOSCallBackScript` (callback script, its parameter calculation, the
  State option) in the file user under a new callback id, and calls
  `RCPerformScriptOnHost::Call` asynchronously with the callback ids.
- When the server script finishes, the server sends `RCSendPSOSRetToClient`
  (opcode 105, "Send PSOS result to client") to the client's session over
  the back-channel (`PSOSCallBack::SendPSOSRetToClient`, dispatched on
  `PSOSCallBackDispatchQueueGroup`) (obs). Body in push order (obs,
  `RCSendPSOSRetToClient::Call` 0x111d890): extended error text
  (unistring), result (unistring, at most 1,000,000 characters, else error
  736), error (u16 BE), client callback id (u32 BE).
- The client (`RCSendPSOSRetToClient::Perform` 0x111e890) pops these, moves
  the callback to the ready list (`FMFileUser::MoveCallBackScriptToPSOSCallBackReadyList`),
  later to the window's trigger queue, and runs the callback script as a
  delayed trigger (event id 208) when `OKToRunPSOSCallbackScriptTrigger`
  allows, applying the State option (3.7) (obs symbols; inf ordering). The
  callback script starts with `Get(LastError)` preset to the server
  script's error (CallScript initial error, 3.7) and `Get(ScriptResult)` =
  the server result ((doc) `perform-script-on-server-callback.html`);
  its parameter calculation is evaluated when it starts (doc).
- If sending fails, `RCPerformScriptOnHost::HandleSendPSOSRetFailure` and
  `UpdateCallBackMap` keep the result for a reconnect; (doc) reconnecting
  within 10 minutes still runs the callback. `EngineMgr::StartUpdatePSOSCallbackMapTimer`
  (fmserverd) expires entries (obs symbol).
- The step is not available on the server itself (table: Pro/Go/WebDirect
  only; error 3 there).

### 11.5 Server-side session context and Get selectors

(doc unless marked; `running-scripts-on-server.html`)

- Each PSOS runs in a separate FMSE client session: no client layout, found
  set, sort, record, global field values or variables; files are opened by
  the session (OnFirstWindowOpen / OnLastWindowClose fire); other files
  must be on the same host and already opened by the client or have
  auto-login credentials.
- Error capture is effectively always on; errors go to the event log (4.9).
- Hardware Get selectors (for example `Get(SystemIPAddress)`) describe the
  server; `Get(ApplicationVersion)` contains "Server" (doc example);
  file access is limited to the Documents and temporary folders; steps with
  "With dialog" On may fail.
- PSOS with Wait for completion off lets the server script run on its own
  thread; a PSOS issued from a server script starts another FMSE session.

## 12. Implementation summary (algorithm)

An implementer can reproduce the runtime with these rules (derived from the
sections above; not a transcription):

```
run_slice():                         # called from the host's idle loop
  start = now()
  while running and continue_flag:
      if current frame's window/file is gone: exit_frame(abort=true, result=true); break
      next_step()
      if continue_flag: perform_pending_trigger_actions(); start_queued_scripts()
      if now() - start >= 2000 ms: break

next_step():
  kind = frame.advance()             # index += 1; NoStep / Ignored / Enabled (2.2)
  if kind == NoStep: exit_frame(abort=false, result=true); return
  if kind == Ignored: return         # disabled, comment, inaccessible script
  with temporary full access if script.full_access: execute(step)

execute(step):
  err, handled, control = run_checks_and_dispatch(step)   # 2.3, 3.x
  post_step(err, handled, control)                         # 2.4
```

Required host hooks: window/record operations, dialogs (client only), the
`CheckError` policy (client alerts vs server log), pause/resume UI, trigger
firing from layout events, and the PSOS transport (11).

## 13. Engine answers (script oracle)

Runs of FileMaker Server 26.0.3's own script engine (2026-10-29) with
`tools/fms-script-harness` (README there): libFMEngine's server runtime
(`SMScriptRuntime`) in a server session (`SMSession`), each run started the
way fmsased starts a schedule (`SMSession::RunScriptAndClose`), with a trace
of every executed step (its own error, whether PostStepCleanup treated it as
a control step, `Get(LastError)` after it) and the server's event-log lines.
Probe scripts were added to a private copy of the Contacts starter template
with FMUpgradeTool and their block indexes written with FileMaker's own
`ScanScript`; the file-trigger runs use the FileMaker Server sample. The
layout-trigger runs (s14) use a file FMUpgradeTool generates from the
template's XML with the probes and triggers added (its patches do not
replace layouts); the find runs (s16) a file generated the same way from
the Tasks template with the file-metadata.md probe structures, and the
validation runs (s17) Contacts copies generated the same way from their
XML export (the harness README). Battery and result files:
`tools/fms-script-harness/{batteries,results}/`, cited below by battery
name. Everything in this section is (run) unless marked.

Scope: the shared runtime as the server runs it. Not covered: FileMaker
Pro's `CheckError` (alerts) and other client-only paths; the steps fmsased
implements itself (3, 35, 36, 42, 117, 132, 144, 152, 157, 200, 225; the
harness has libFMEngine's runtime, not `FMS::SASEScriptRuntime`, and runs
only 36 as fmsased does, with `DOCUMENTS`: script-steps.md,
"Implementation notes (import/export)", Oracle); PSOS on the wire (the
file is not hosted).

### 13.1 A server run (s04, s10)

- Order: the file's window opens and OnFirstWindowOpen runs (with any layout
  triggers it causes), then the requested script, then OnWindowClose when
  the file closes. This needs fmsased's rule for file triggers
  (`SASEScriptRuntime::AllowFileTriggers` returns the master trigger switch;
  the base rule in libFMEngine also wants the debugger's user-trigger switch
  or a running step and suppresses them) (obs for the rule).
- Session context at the start of the script: window name = file name
  without extension; the layout the file opens on (Contacts: Contact
  Details; FMS sample: Startup Screen until its OnFirstWindowOpen script goes
  to Tasks); Browse mode; `Get(ScriptName)` the script;
  `Get(ApplicationVersion)` `Server 26.0.3`; `Get(WindowCount)` `?`;
  `Get(ErrorCaptureState)` 0, `Get(AllowAbortState)` 1,
  `Get(RevertTransactionOnErrorState)` 1, `Get(TransactionOpenState)` 0,
  `Get(ScriptAnimationState)` 0, `Get(LastMessageChoice)` 0. (Host-related
  selectors reflect the unhosted harness: `Get(MultiUserState)` 0,
  `Get(HostApplicationVersion)` empty.)
- The run's result is the top script's Exit Script result as text in the
  file's locale (`Date ( 1 ; 2 ; 2020 )` gives `1/2/2020` in en-US); a
  sub-script's result never is, and a run whose top script ends without
  Exit Script, or is halted, returns no result. The returned error is the
  final `Get(LastError)` (104, 201, 5001 in the batteries), except after an
  abort on error 4 (13.7).
- Each run is a new session: `$$` variables set by one run are gone in the
  next; record changes stay in the file.

### 13.2 `Get(LastError)` (s01, s04, s09)

- If, Else If, Else, End If, Loop, Exit Loop If, End Loop, Exit Script and
  Halt Script leave it unchanged; comments and disabled steps do not
  execute at all. Every other executed step sets it, 0 on success (Set
  Error Capture, Allow User Abort, Set Variable, Go to Layout, Pause/Resume
  Script, Perform Script, New Record/Request, Commit Records/Requests).
- Codes seen: Go to Record/Request/Page [Next] on the last record or in an
  empty found set 101; Set Field without a current record 101; Set Field
  into a calculation field 201; Perform Script by name of a missing script
  104; Delete Record/Request and Commit Records/Requests without a record 0.
- Calculation errors inside a step's parameters are not step errors: Set
  Variable, Set Field, a Perform Script parameter and an Exit Script result
  that evaluate `GetField ( "x" )` (error 102) or `Evaluate ( "1+" )`
  return 0 and use the value `?`.
- `Set Variable [ $a ; Get ( LastError ) ]` sees the previous step's code
  (the value is evaluated before the step stores its own 0).
- Perform Script stores 0 when it starts the sub-script; the sub-script's
  steps then overwrite it, so after the return the caller sees the error of
  the sub-script's last non-control step (also when the sub-script ended
  with Exit Script).
- Step id 0 returns -1 from its handler (2.3), which is never stored (as an
  unsigned code it lies in the range 4.2 ignores) and not logged.
- `Get(LastErrorLocation)` is `script ¶ step name ¶ line` (1-based) and
  stays readable after the script; `Get(LastErrorDetail)` is empty for 104.
- Event log (`SMScriptRuntime::CheckError`, 4.9): every stored error (3, 4,
  5, 101, 104, 201, 5001 seen) produces `file : script : line : step name`
  (unknown ids as `<unknown> [15]`), whether Set Error Capture is On or
  Off. A failing Commit Transaction is logged only with a custom code
  (5001); with 3 or 201 it is not (it passes PostStepCleanup as a control
  step, 2.3; for 201 the failing Set Field was logged). Error capture
  changes nothing else on the server: steps continue after errors either
  way.

### 13.3 Conditions (U3; s02)

If, Else If and Exit Loop If take the boolean of the calculation's result.
A failing calculation (`GetField ( "x" )`, `Evaluate ( "1+" )`, `1/0`,
`Length ( GetField ( "x" ) )`, an `Evaluate` of an unknown field) yields `?`,
and the boolean of `?` is 1 (calc oracle: `GetAsBoolean ( "?" )` = 1,
`If ( GetField ( "x" ) ; 1 ; 0 )` = 1, while `"abc"`, `"a?"`, `"-"` are 0 and
`"a1"` is 1), so the branch is taken and Exit Loop If exits. The condition's
error never reaches `Get(LastError)`. A field reference the importer could
not resolve evaluates as empty (false). Nested If/Else If blocks with the
one clause flag (3.2) branch correctly, including an inner Else If inside a
taken If and the outer Else If after it being skipped.

### 13.4 Loops (U4; s02)

Loop's stored flush values are 1 Always, 2 Minimum, 3 Defer (FileMaker's
XML writer names the stored values so). FileMaker's importer sets kOption
0x100 on every Loop, Always included. End Loop continues at the step after
Loop (the Loop step is not re-executed); a true Exit Loop If continues after
End Loop; nested loops behave independently.

### 13.5 Results and parameters (s03)

- `Get(ScriptResult)` is empty at the start of a top script.
- Exit Script with a result sets the direct caller's `Get(ScriptResult)`.
  Exit Script without a result, and a sub-script that ends without Exit
  Script, leave the caller's previous value (a `b` from an earlier
  sub-script survives both); `Exit Script [ "" ]` clears it.
- A result set two levels down reaches only the middle script; the top
  script still sees empty after the middle one exits without a result.
- Results and parameters keep their data type: a date result plus 1 is the
  next day; `Exit Script [ 0012.50 ]` gives the number 12.5; a date
  parameter plus 1 is the next day.
- `Get(ScriptParameter)` belongs to each frame: a sub-script called without
  a parameter sees empty.

### 13.6 Variables (s03; U11 in part)

- `$` variables belong to the frame: a sub-script sees none of its caller's,
  its own are gone after it returns, and the caller's are unchanged. `$$`
  variables are shared by all frames of the session.
- `Let ( [ $w = 5 ; $$lw = 6 ] ; ... )` evaluated by a step sets the current
  frame's `$w` and the global `$$lw`.
- Names are not case sensitive (`$Abc`, `$abc`, `$ABC` are one variable;
  likewise `$$GLOB`, `$$glob`).
- `$v` and `$v[1]` are one variable; other repetitions are independent;
  setting a repetition to `""` makes it empty; a calculated repetition
  `1+1` is 2; repetitions 0 and -1 are valid and distinct (no error);
  repetition 1.7 is repetition 1 (truncated).
- Set Variable with the names `$`, `$$` or `$1x` fails with 5 (logged); the
  name `$a b` is accepted.

### 13.7 Steps on the server (U7; s04, s09)

| Result | Steps |
|---|---|
| 3, logged, the script continues (also with Allow User Abort On) | Open Help 32, Open Manage Database 38, Freeze Window 79, Show Custom Dialog 87, Beep 93, Insert from Device 161, Refresh Object 167, Close Popover 169, AVPlayer Play / Set Playback State / Set Options 177-179, Refresh Portal 180, Configure Machine Learning Model 202, Perform Script on Server with Callback 210; Perform Script on Server 164 (file not hosted) |
| 0, no effect seen | Go to Next/Previous Field 4, 5, Show/Hide Text Ruler 92, Set Zoom Level 97, Show/Hide Menubar 166, Cancel PDF 247 (all without parameters) |
| 5 | Set Web Viewer 146 without an object |
| 4, logged | obsolete 15, 52, 54, 58, 59, 78, 100, 153, 176, 198, 204; Execute URL 224 and Validate Scripts 239 (gated); 248 (out of range) |

- Error 4 with Allow User Abort On ends the run at that step (no further
  step runs); `RunScriptAndClose` returns 4 and `Get(LastError)` then reads
  1. With Allow User Abort Off the script continues with `Get(LastError)` 4.
  Error 3 never stops a script.
- Pause/Resume Script with a duration (0.1 s, 0.3 s) or indefinitely
  returns 0 and the script continues by itself.
- Exit Application (44) ends the script; its step error is 13 and is not
  stored (inf: the session's files are closed by then, so no script is
  running and SetLastError ignores it, 4.2).

### 13.8 Error capture, Allow User Abort, revert on error (s06)

At the start of every top script the states are 0 (capture), 1 (abort), 1
(revert on error). A sub-script sees the caller's states, and states set in
a sub-script stay in force in the caller after it returns. Halt Script and
the end of the top script reset all three.

### 13.9 Halt Script (s06)

Halt Script in a sub-sub-script ends every frame: no step after any of the
Perform Script calls runs. `Get(LastError)` keeps the last stored code (104
in the probe), the run has no result, and `$$` variables set earlier stay.

### 13.10 Transactions (U9; s05, s07)

- `Get(TransactionOpenState)` is 1 from Open Transaction to Commit
  Transaction, in sub-scripts too, else 0. `Get(RevertTransactionOnErrorState)`
  is 1 by default, 0 after Set Revert Transaction on Error [Off], and 1
  again after the script.
- Commit Transaction or Revert Transaction (also with an error code) without
  an open transaction: 3. A second Open Transaction in the same frame: 3,
  and the first one stays open (8.1 inferred -1). Open Transaction in a
  sub-script while the caller's is open: 3. Commit Transaction in a
  sub-script: 3, the transaction stays open and the opener commits it.
- Revert Transaction returns 0 itself; the following steps up to Commit
  Transaction are skipped (they pass PostStepCleanup as control steps, so
  `Get(LastError)` is unchanged); Commit Transaction then reverts. A Revert
  Transaction in a sub-script reverts the caller's transaction and the
  rest of the sub-script and of the caller up to the Commit are skipped.
- `Revert Transaction [ Error: 5001 ; Message: "custom message" ]`: Commit
  Transaction returns 5001 (logged at the Commit), `Get(LastError)` 5001,
  `Get(LastErrorLocation)` = `script ¶ Commit Transaction ¶ its line`,
  `Get(LastErrorDetail)` = `script ¶ Revert Transaction ¶ its line ¶ custom
  message`. Codes outside 5000-5499 (4999, `"abc"`) leave 0 (inf: the
  revert itself still happens; the probe had no records to show it). `Revert Transaction [ Condition: 0 ]` does nothing.
- With revert on error On, only data-layer errors mark the transaction
  (8.4): Set Field into a calculation field (201), in the script or in a
  sub-script, skips the remaining steps of every frame up to Commit
  Transaction, which returns 201 (stored by Execute, treated as a control
  step, not logged), `Get(LastErrorLocation)` the Commit step,
  `Get(LastErrorDetail)` `script ¶ Set Field ¶ line`, and the records are
  reverted. Errors of Perform Script (104), Go to Record (101) and
  unsupported steps (3) do not: the steps continue and Commit commits. With
  revert on error Off the 201 skips nothing and Commit commits.
- The transaction is reverted by Halt Script, by Exit Script of the top
  script, when the top script ends without Commit Transaction, and when an
  Exit Loop If jumps past the Commit Transaction.

### 13.11 Starting a script while one runs: the current-script option (U5; s08)

`CallScript(window, B, no parameter, newState, queueAtBottom)` after step 2
of a script A (running, paused by Pause/Resume Script, or a sub-script of
A):

| newState | queueAtBottom 0 | queueAtBottom 1 |
|---|---|---|
| 0 Continue, 3 Resume, 4 Pause, 5 Suspend and Resume | B runs at once on top of the current frame; then the current script continues; B's Exit Script result becomes the current frame's `Get(ScriptResult)` | A and its callers run to the end first, then B; the run's result is A's |
| 1 Halt | every frame is halted, then B runs; the run has no result | same |
| 2 Exit | the current frame exits (as Exit Script without a result); B runs on top of its caller, B's result goes to that caller, then the caller continues (a top-level A just ends) | same for A |

CallScript returned 0 in every case. 4 has no visible effect on the server
(server pauses are short); 5 has no behavior of its own (U5).

### 13.12 Triggers (s01, s06, s10)

A layout trigger fired by a step (OnLayoutEnter by Go to Layout) runs right
after that step as a frame pushed above the caller, before the caller's
next step. When one step fires two triggers, two frames are pushed and the
one pushed last runs first (FMS sample: Sort Task List at depth 3, then
Create Task in List View at depth 2; Go to Layout calls OnRecordLoad before
OnLayoutEnter, so OnLayoutEnter's script runs first, 13.17). Go to Layout to
the layout the window already shows fires nothing. Triggers fire during
server scripts. What a trigger's result does: 13.17.

### 13.13 Cached block indexes (3.1)

FileMaker's XML importer (FMUpgradeTool `--update`, the same reader as
Save-as-XML round trips) stores every cached index as 0. FileMaker's
`ScanScript(ScriptStepListIter const&)` (ScriptUtility.cpp; exported, no
caller in the server build; it calls `InternalScanScript`, which also
writes the Else If End If index as numeric data and creates missing step
data keys) writes the values; on the scanned copies they equal the rules of
3.1 as implemented in `om-scripts` (`step_params`: 7/7 step values and 1/1
numeric index). ScanScript returns internal error 3702 for unbalanced
blocks (Commit Transaction without Open, Open without Commit). With the
importer's zeros, an Else If reached after a taken branch jumps to index -1
and the script restarts at step 0.

### 13.14 Queue modes from sub-scripts, while paused, and chained (s11)

Calls made after step 2 of a sub-script S of the request's script A (A:
step 1, Perform Script S, a step logging `A3` with `Get(ScriptResult)`,
`Exit Script [ "a" ]`; S: three steps, `Exit Script [ "s" ]`):

- `queueAtBottom` 1 with states 0, 3, 4 and 5: B runs after A and its
  callers have ended (`A1 S1 S2 S3 A3(s) B`); the run's result is A's. B
  sees the `Get(LastError)` the ended script left (104 when A ended on a
  failed Perform Script) and an empty `Get(ScriptResult)`: A's Exit Script
  result went to the request (it is the final frame, 13.1), not to the
  frame below it.
- State 1 (Halt), bottom 1: every frame is halted, then B runs; the run has
  no result. State 2 (Exit), bottom 1: S exits at once without a result (A
  continues and sees none: `A3()`), B runs after A; the result is A's.
- While S is paused (Pause/Resume Script without a duration), bottom 0:
  states 0, 3 and 4 run B at once on top of S, then S continues; 1 halts
  every frame and runs B (no result); 2 exits S and runs B on top of A, and
  B's result reaches A (`A3(b)`). State 2 with bottom 1 exits S, A ends,
  then B runs.
- Two calls after one step: both at once, the second (C) runs first and
  its result goes to the first (B sees `c`); both at the bottom, B runs
  before C and C sees B's result (`b`): the frame below a script receives
  its Exit Script result; B at once and C at the bottom: C runs after A
  and sees no result.
- A call from a script that a call started follows the same rules (D at
  once, C on top of D after D's first step: `D1 C() D2`; D at the bottom,
  then C with state 2 after D's first step: D exits, C runs).
- `CallScript` returned 0 in every case.

### 13.15 Transactions across sub-scripts and custom codes (s12)

- `Revert Transaction [ Error: 5002 ; Message: "sub message" ]` in a
  sub-script (also two levels down) of the frame that opened the
  transaction: every following step of every frame up to that frame's
  Commit Transaction is skipped, the sub-script's Exit Script included (no
  result reaches the caller); Commit Transaction returns 5002 (logged at
  the Commit), `Get(LastErrorLocation)` is the caller's Commit,
  `Get(LastErrorDetail)` = `sub-script ¶ Revert Transaction ¶ line ¶ sub
  message`; the record created in the transaction is gone.
- A custom code without a message: the detail is the location alone (no
  trailing return).
- Two Revert Transaction steps: the second is skipped; the first's code and
  message stand.
- A data error (Set Field into a calculation, 201) with revert on error On,
  then `Revert Transaction [ Error: 5006 ]`: the revert is skipped and
  Commit returns 201 (detail the Set Field), also when both are in a
  sub-script. With revert on error Off the 201 skips nothing: the revert
  runs, Commit returns 5006 (logged, detail the Revert Transaction and its
  message) and the records are reverted.
- An error code outside 5000-5499 (4999): Revert Transaction returns 0 and
  still reverts (the following steps are skipped, the new record is gone);
  Commit returns 0.
- Halt Script and Exit Script inside a transaction that a data error
  failed are skipped like any other step; Commit returns 201 and the steps
  after it run (`Exit Script [ "end " & Get ( LastError ) ]` gives `end
  201`).
- `Set Revert Transaction on Error [ Off ]` in a sub-script stays in force
  for the caller's transaction (13.8): a 201 in the caller then skips
  nothing and Commit commits; the state reads 1 again after the script.

### 13.16 Points of the implementation notes (s13)

- A Set Variable repetition that evaluates to `?` (`$v[GetField ( "x" )]`)
  is error 5, logged, and assigns nothing.
- `Get(LastErrorLocation)` is cleared by the next step that stores 0: a Set
  Variable evaluating it right after a 104 sees the location, the one after
  that sees empty.
- Go to Record [Next; Exit after last] in a loop leaves the loop on an
  empty found set and after the last record (3 records: 3 passes) and
  stores its 101 there, logged with the step's name and the line of the End
  Loop (`work : rn exit after last empty : 5 : Go to Record/Request/Page`
  for the step at line 2 of a loop ending at line 5), which is also
  `Get(LastErrorLocation)`'s line: the frame's index after the jump. Outside
  a loop (stored index -1) it ends the script; its 101 is stored nowhere
  (no script is left), the run has no result and returns 0.
- Perform Script by name compares without case (`RN SUB` runs `rn sub`); a
  trailing space does not match (104); `work::rn sub`, the file's own name
  as data source, is 104.
- An unknown step (15) in a sub-script with Allow User Abort On aborts
  every frame: the run returns 4, `Get(LastError)` reads 1, the caller's
  remaining steps do not run.
- The request script's `Exit Script [ "" ]`: the result is empty.

### 13.17 Triggers whose script returns False, True, text or nothing (s14)

Probe file: Contacts generated with OnLayoutExit and OnRecordCommit on
Contact Details (the layout a session opens on) and OnLayoutEnter and
OnRecordLoad on Contact List, each running a probe script that logs and
ends as the run asks.

- A step whose event a pre-event trigger handles (Go to Layout and
  OnLayoutExit of the layout shown, Commit Records and OnRecordCommit)
  returns the internal 3000 (1.7; not stored, not logged) and the trigger
  script runs at once on top of its frame (one level deeper, also when the
  step is in a sub-script). Calling the trigger resets `Get(LastError)` to
  0: the trigger script sees 0 even right after a step that stored 104.
- The trigger lets the event happen when its script ends with `Exit Script
  [ True ]`, without Exit Script, or after a failing step (104) without
  Exit Script: the step then runs again in its frame (a second trace line
  for the same line, its own error, 0, stored) and the triggers of what it
  does fire then. `Exit Script [ False ]`, `[ "abc" ]` and `[ "" ]` cancel
  the event: the trigger frame's exit stores 20 (not logged), the step is
  not run again, and the caller continues at its next step with
  `Get(LastError)` 20 (the layout is unchanged; in a sub-script the caller
  sees the 20 after the sub-script ends). The trigger's result never
  reaches the caller's `Get(ScriptResult)`.
- Halt Script in a trigger script halts every frame: the event does not
  happen and the run has no result.
- A cancelled Commit Records (20) leaves the new record in the found set;
  it is saved by the time the session ends (the next run counts it), as
  after a halted OnRecordCommit.
- Post-event triggers (OnLayoutEnter, OnRecordLoad) run after the step.
  Their False or text result is ignored and stores no 20; Halt Script in
  them halts every frame. Both were called by the same step, so the second
  sees the `Get(LastError)` the first one's script left (104).
- Go to Layout calls OnRecordLoad before OnLayoutEnter; each frame is
  pushed on top, so OnLayoutEnter's script runs first (depth 3), then
  OnRecordLoad's (depth 2), then the caller. OnRecordLoad fires on entering
  a layout also with an empty found set (`Get(RecordNumber)` 0), and after
  Go to Record [First] moved to record 1. `Get(LayoutName)` is the old
  layout in OnLayoutExit and the new one in OnLayoutEnter.
- A new session's window shows the last record of the found set (5 of 5).

### 13.18 The stack memory guard (2.6; s15)

Probe scripts recurse with Perform Script, each frame holding a text of L
characters as its parameter, as a `$` variable or as its script result
(`$$` variables keep the probes' state, which the guard does not count),
until a call fails; the start script's parameter is `L ¶ max`. Paired
lines straddle a boundary by one to ten characters.

- The failing call returns 2; the script goes on (`$$failerr` 2) and the
  frames return normally.
- When: the guard looks at the first call made while the stack holds 100
  or more frames and at every 100th such call after it (a counter of those
  calls, not the depth). A plain recursion fails at 100 × k frames (599
  frames of `sg param` plus the start script for L = 1,000,000, 1,099 for
  500,000, 2,199 for 250,000); a recursion that also calls a script
  without a parameter before recursing ("sg mixed") is checked every 50
  frames, always at that call (it failed at 549 frames, the recursion call
  never).
- What: the estimate covers the frames already on the stack, not the one
  being created (a 2,690,000-character parameter passes at 200 frames
  although 200 copies are over 1 GiB). Fitted to the boundaries (L between
  487,099 and 487,100 at 1,100 frames, 894,866 and 894,868 at 600,
  2,696,420 and 2,696,428 at 200), with T = 2^30 and failure above it:
  a script frame 1,544-1,588 bytes; a parameter or `$` value 1,228-1,268
  bytes plus 2 per character; a `$` variable 28-50 bytes more than its
  value (`$v` with L = 487,075/487,085, a second variable `$w` = `a` with
  486,400/486,450, a 1-character parameter beside it 486,450/486,460); a
  fixed part of 340-600. A frame with a parameter costs about 2,816
  (0xb00) where 2.6 read 0x788 plus the parameter.
- Characters are code points: 250,000 emoji fail at the depth of 250,000
  `x`, and 250,000 `e` with a combining acute at that of 500,000 `x`,
  although `Length` counts each as one character.
- Script results are not counted: 3,000 frames each holding a 500,000-
  character result pass, as do `$$` variables.
- FileMaker's real memory per frame is far above its estimate: a
  recursion with an empty parameter (a scratch run, not in the battery)
  had used 22 GB when it was stopped, so the guard bounds data, not
  frames.

### 13.19 Finds on unstored calculations, summary and global fields (s16)

Probe file: the file-metadata.md probe (Tasks template,
`tools/file_metadata_probe.py --no-triggers --find-fields`) with unstored
calculations U = `N * 2` (number) and UT = `Task & " " & Status` (text),
both with "do not evaluate if all referenced fields are empty", a global
text field G, the summaries of file-metadata.md 5.2, and the records of
4.3 (e 5, a 1, d 4, b 2, the record without Task and N, c 3; U is 10, 2, 8,
4, empty, 6). Each probe: Enter Find Mode, Set Field into the field, Perform
Find (error capture on).

- An unstored calculation takes a criterion (Set Field returns 0) and is
  found with every find operator on its value, record by record: U `8` d;
  `>5` e d c; `#` (one digit) a d b c; `=` the record without N (U is
  empty there, not 0); `*` the other five; `==8` d; `4...8` d b c; `!`
  nothing (401); UT `pend` e a (empty) c; `==a Pending` a; `d*` d b;
  `"b Done"` b. With a criterion on a stored field the two are ANDed (U
  `>3` and Task `b`: b).
- A summary field takes no criterion: Set Field into one in Find mode is
  201 and stores nothing; Perform Find is then 400 when nothing else was
  set (S_Total `15`, `14`, `*`, `=`, S_RunTotal, S_Count, S_List, S_Frac
  alike), and finds by the other criteria when there are some (S_Total
  `15` and Task `a`: a). The found set before the find does not matter
  (a find for e first, then S_Total `5`: 400).
- A global field takes no criterion either: Set Field into one in Find
  mode sets its value (0; G reads the new value in Find mode and after the
  find) and adds no criterion: G alone is 400 whatever was set (`x`, `y`,
  `*`, `=`); with Task `a` the find finds a whatever G holds.
- After the 400 the window is in Browse mode.

### 13.20 Validation of repeating fields (s17)

Contacts with one text field given three repetitions and one rule, strict
and validated Always unless named otherwise; each probe is a new record (or
the last record) with Set Field into repetitions, then Commit Records,
error capture on (`tools/fms-script-harness/README.md` for the files).
Errors are Set Field's, then the commit's.

- Not empty is the field's as a whole: no repetition set 509; repetition
  1 alone 0; repetition 2 alone 0 (repetition 1 empty); clearing
  repetition 1 or 2 of a record whose repetition 3 holds a value 0.
- By calculation, `Length ( Self ) <= 3`: an over-long value in
  repetition 1, in repetition 2 beside a short repetition 1, or in
  repetition 2 alone 507; short values in both 0; setting repetition 3
  empty 0. `Get ( CalculationRepetitionNumber ) = 1`: repetition 1 0,
  repetition 2 507: the calculation runs in the repetition it checks.
  `Length ( First Name ) <= 3` on First Name: 507 whether repetition 1 or
  2 is long, so the field named in its own formula reads the repetition
  being checked. `not IsEmpty ( Company )` (Company has one repetition)
  with Company set: repetition 2 alone 0, so in repetition 2 a
  non-repeating field reads its value (a repeating calculation field would
  read it as empty there, calc-values.md 8.6).
- Only changed repetitions: a record given an invalid repetition 1 under
  Open Transaction's "Skip data entry validation" (rule only during data
  entry) commits a valid repetition 2, and a change of another field, with
  0.
- Maximum characters (3): Set Field of 6 characters into repetition 1 or
  2 is 511 itself and the field keeps its value (the commit then has
  nothing to refuse, 0).
- Unique (value index): repetition 2 equal to another record's repetition
  1, repetition 1 or 3 equal to another's repetition 2: 504; the same
  value in two repetitions of one record: 0.
- Overridable rules (strict off) refuse as strict ones do (not empty 509,
  by calculation 507 in repetitions 1 and 2), and so do rules for data
  entry only (509): a script's Set Field is data entry.
- Empty values, `not IsEmpty ( Self )` validated Always: a new record
  that leaves the field alone 0, one that sets it to empty 0, clearing a
  value 507: a changed repetition is checked even when empty, an untouched
  one is not, validated Always or not. With "Validate only if field has
  been modified" off (`0x2`, XML `alwaysValidate`): an invalid repetition 1
  stored under "Skip data entry validation" fails a later change of
  another field or of repetition 2 (507), a new record that leaves the
  field empty 507, one that sets only repetition 1 0 (the empty
  repetitions 2 and 3 are not checked).
- A validation calculation that divides by zero: 507 for a value, 0 for a
  field left alone.

A copy whose definitions OpenMaker's writer changed enforced none of these
(even the over-long repetition 1 committed) until FMUpgradeTool generated
the file from its XML export: FileMaker keeps bookkeeping of its own for
validated fields, which the writer's copy lacked (not investigated; it
matters for a server that edits field definitions itself).

### 13.21 Strict data types, ranges, value lists, existing values; Find mode without criteria or record (s18)

Probe files (`tools/fms-script-harness/s18_validation.sh`, battery
`s18-<rule>-<variant>`): Contacts with four fields given one rule, A
First Name and C Title without repetitions, B Last Name and D Job Title
with three; variant a validates A and B strictly and Always, C and D
overridable and only during data entry, variant b the other way round. As
for 13.20 OpenMaker's writer sets the definitions (data type, validation
word, repetitions), FMDeveloperTool exports the copy as XML, the range and
the value list (keys the writer could not write) are filled into the
export in the shapes FileMaker writes (`<Range to="10" from="1">`,
`<ValueListReference id="3" name="Title" ...>`), and FMUpgradeTool
generates the file that runs. Each probe is New Record (or the last
record), Set Field into fields or repetitions, Commit Records; errors are
Set Field's and the commit's, and the value read after each Set Field
shows whether it was stored.

- Maximum characters (511), "Strict data type" numeric only (502), 4-digit
  year date (500, on a date or a timestamp field), time of day (501), in
  range (503) and member of value list (506, custom values and a field's
  values alike) are Set Field's own errors: the field keeps its value and
  the commit has nothing to refuse. They apply only to a field validated
  Always, strict or overridable alike; validated only during data entry,
  a script's Set Field stores any value (maximum characters too; 13.20
  tried it Always only). Every repetition is checked; empty values pass.
  - Numeric only: `abc`, `12abc` 502; `1.5`, `-3`, `1e3` accepted (`1e3`
    kept as typed).
  - 4-digit year: the value as Set Field gives it: `1/2/26` and `abc`
    500, `1/2/2026` and `1/2/0026` accepted; on a timestamp `1/2/26 10:00`
    500.
  - Time of day: `25:00`, `-1:00`, `abc` 501; `10:30` accepted.
  - In range 1...10 (a number field): `11` and `abc` 503, `5` accepted.
  - Value lists: Title (`Mr.`, `Mrs.`, `Ms.`, `-`, `Prof.`, `-`): `Dr.`
    506, `mr.` accepted (case does not matter); Company (the values of
    Contacts::Company): `Zed` 506; once a record's Company is `Acme`,
    `Acme` and `acme` are accepted.
- Existing value is the commit's error 505 (Set Field 0), for a field
  validated only during data entry too; Open Transaction [ Skip data entry
  validation ] skips it there but not for a field validated Always. A
  value exists when another record holds it in any repetition (repetition
  2 `Acme` beside another record's repetition 1 `Acme`: 0); case does not
  matter; empty passes.
- The field types alone never refuse a script's value: `abc` in a number
  field is stored as typed, in a date, time or timestamp field it reads as
  `?`; `25:00` is a time.
- Storage in the generated files: the range is `[T, 3, 5, F, 21]` (from)
  and `22` (to), text; the value list reference `[T, 3, 5, F, 07]`, 13
  bytes: a key string of the list's id (`02 01 03` is list 3), then 10
  bytes not decoded (schema-path.md 5.5 names `21` the value list and
  `22` the range; the files say otherwise). FileMaker turned on the value
  index (`0x80000000`) of fields validated as existing values.
- Find mode (`s18-find`, Contacts with Website made a global field):
  Perform Find without criteria is 400 and the window is back in Browse
  mode, its found set and current record as they were (3 records on record
  3; a found set of 1 record stays); `Get ( RequestCount )` is then the
  find's request count (1, or 2 after New Request), 0 before any find. In
  Find mode a global field reads its value and a normal field the current
  request's criterion (`b` after Set Field of `b`, empty before), both
  without error (EvaluationError 0, not 1224); with an empty found set
  (after a 401) a global reads its value and a normal field is empty. A
  global set in one run was set in the next: the harness's file is not
  hosted (FileMaker keeps a hosted file's global values per session,
  Claris "Global fields"). The harness also showed a new session's window
  on the previous session's found set, as it keeps the file open between
  runs; OpenMaker does not model that.

### 13.22 A non-repeating field in a later repetition's validation (s19)

Contacts with First Name given three repetitions and validated Always
and strictly by a calculation reading Company, which does not repeat
(`tools/fms-script-harness/batteries/s19-validation-<rule>.txt`, files
built as for 13.20). Each probe is a new record, Set Field of Company
`Acme` (or none) and of First Name's repetition n, then Commit Records;
errors are Set Field's, then the commit's.

| validation | rep 1 `Acme` | rep 2 `Acme` (also with rep 1) | rep 3 `Acme` | rep 2 `Zed` | rep 2 `x`, no Company |
|---|---|---|---|---|---|
| `Self = Company` | 0 | 0 | 0 | 507 | 507 |
| `Self = Company[1]` | 0 | 0 | 0 | 507 | 507 |
| `Self = GetRepetition ( Company ; 1 )` | 0 | 0 | 0 | 507 | 507 |
| `Self = Extend ( Company )` | 0 | 0 | 0 | 507 | 507 |
| `IsEmpty ( Company[2] )` | 507 | 507 | 507 | 507 | 507 |

So while repetition n is validated a field that does not repeat reads its
value however the formula names it, and its repetition 2 does not exist
(the calculation fails, which accepts nothing: 507, as 13.20's division by
zero). Set Field itself succeeded in every probe. OpenMaker evaluates the
validation of repetition n with `om_calc::eval::Context::extend_fields`
(a bare reference to a field that does not repeat reads repetition 1 in
every repetition); before, it moved the field's value into repetition n,
so `Company[1]`, `GetRepetition` and `Extend` read empty there (the import
review's C7). `crates/om-server/src/scripts/oracle_tests.rs` replays all
35 runs.

### 13.23 Schedule runs (s36)

Every run line of the harness is `SMSession::RunScriptAndClose` with a
file, an account, a script name and a parameter, as fmsased's schedule
thread starts a FileMaker script schedule
(`tools/fms-script-harness/batteries/s36-schedule-run.txt`, contacts-en;
the `@path` directive runs a line on another path).

- Script name: matched without case (`SCHED CONTEXT` and `Sched Context`
  run `sched context`, whose stored name `Get ( ScriptName )` answers),
  and not trimmed: a leading or trailing space, an unknown name and the
  empty name return 104 with `Get ( LastError )` 0 and no event-log line
  (no script starts). Of two scripts whose names differ only in case,
  the first in the script list runs for either spelling.
- Parameter: the text as given. `5 + 5` is `5 + 5` (`GetAsNumber` 55,
  `Evaluate` 10); returns and non-ASCII characters are kept; without a
  parameter `Get ( ScriptParameter )` is empty (`IsEmpty` 1).
- Session: `Get ( AccountName )` and `Get ( UserName )` are the account
  as the login names it (`admin` for the file's `Admin`), `Get (
  WindowName )` the file name without extension, `Get (
  ApplicationVersion )` `Server 26.0.3`, `Get ( ScriptResult )` empty,
  `Get ( LastError )` 0, the opening layout in Browse mode, error capture
  0, allow abort 1; a sub-script sees the same session.
- Result and error as 13.1: the top script's Exit Script text (a date in
  the file's format), none after Halt Script, without Exit Script or with
  an empty result; the final `Get ( LastError )`, so 0 when a step
  succeeded after a failing one (whose event-log line, `work : sched
  error : 1 : Go to Record/Request/Page`, is written anyway); 4 after an
  unknown step (`Get ( LastError )` 1).
- `$$` variables set by one run are gone in the next.
- Login: a wrong password, an account the file does not have and the
  file's disabled account (`[Guest]`, `enable="False"` in the template's
  XML) all return 212, before any session: no script runs.
- A file that cannot be opened (a missing path, a folder) returns 802.

Not reachable with the harness: fmsased's own schedule code (the task
queue, time limits, the schedule's own event-log messages, email). The
implementation takes those from the Claris documentation ("Implementation
notes (schedules)"); U15 lists the probes that would settle them on a
FileMaker Server.

### 13.24 Portal rows (s35-portals)

Task Details of the Tasks template (generated files, `s35-portals*`; the
probes and every answer are in `tools/fms-script-harness/results/s35-findings.md`).
The Attachments portal is first in stacking order; its relationship allows
creating Attachments; its option word is 257.

- No portal active: row 0, no active field. Go to Portal Row uses the first
  portal in stacking order: First row 1 (the row's first field active,
  repetition 1), Previous 101 (nothing active). With Select On the row is
  active without a field (`ActiveLayoutObjectName` the portal, repetition
  0).
- Rows: the related records, then one empty **creation row** when the
  relationship allows creating records and the portal scrolls (option bit
  `0x1`); a portal that does not scroll reaches only its visible rows.
  Last goes to the creation row. Next at the last row and Previous at row 1
  are 101 and keep the row; By calculation truncates (1.5 is 1, `"2"` is
  2), and 0, -1, past the last row, `abc` and empty are 101 and change
  nothing. Exit after last leaves the loop with 101 as LastError.
- Go to Object: a portal becomes the active object (row 0, or the row it
  had); a field in a portal is entered in the active row, else row 1; a
  field in a portal without rows is 101 (the portal active); no such
  object 116; an empty name 5; a repetition the field lacks 102 (nothing
  active). Go to Field into a portal's field enters row 1 (or keeps the
  active row); a field not on the layout 102.
- In FileMaker Pro 26.0.3's window (#866: disposable copies of the Assets,
  Inventory and Meetings starters, observed on screen and through the
  accessibility tree): Go to Object on a tab panel brings it to the front,
  and on a portal inside a tab panel brings that panel to the front and
  focuses the portal as a whole (`AXGroup` "Portal View"), no row or field.
  A pop-up menu in a portal row that a script enters (Go to Object on its
  name: row 1; Go to Portal Row [Last] then Go to Field [Select/perform]:
  the creation row) takes the focus with its list closed. Slide panels,
  panels inside panels, and Go to Field into a field on a back panel were
  not measured.
- Set Field into a related field writes the active row's record, else the
  first related record, else (the creation row, or no related record)
  creates one when the relationship allows it, its match field set to the
  parent's key; without creation 101. The related records belong to the
  parent's open record: Commit Records commits them together, a related
  record's validation failing the whole commit (507; Set Field 0, also for
  "only during data entry" rules), Revert Record drops them. Commit Records
  leaves the portal (row 0).
- Delete Portal Row without an active row is 3; with one it deletes the
  row's record (the creation row: nothing) and no row is active
  afterwards; the deletion is the parent's: Revert Record brings it back.
  None of the option words tried (0, 1, 256, 257, 273) stopped it; none
  has `0x4`, "Allow deletion of portal records" (layout-model.md 5.3.2),
  so the step does not need that option.
- Go to Related Record from the active row: that row's record is current
  in the related layout (Show only related records on or off).
- Go to Record keeps the active row number on the next record (101 after
  moving when that record lacks the row); Go to Layout away and back
  clears it, Go to Layout to the current layout keeps it.

### 13.25 Windows (s35-windows)

- New Window copies the current window's layout, found set, record and mode;
  without a name it is named after the current window, `W2 - 2`; names may
  repeat; with a layout it goes there and the layout's OnLayoutEnter runs
  before the next step. `WindowNames` lists the windows front to back.
- Found set, current record, layout and mode are per window: an omit and a
  move in W2, or Find mode there, leave W1 as it was.
- Close Window [ Current ] makes the window that was current before it
  current (not the newest); Close Window and Select Window by a name no
  window has are 112; closing another window by name keeps the current one.
  Closing the file's last window is 13 on the server and stops the script.
- The server's windows are 640 x 480 at 0, 0; New Window's bounds are taken
  as given; `Get ( WindowUUID )` is 32 hex digits, one per window.

### 13.26 OnWindowTransaction (s35-wintx)

- Fires once per commit that writes records: Commit Records of a new or
  changed record (also Set Field of the same value), Delete Record, Delete
  All Records (one firing for all), Commit Transaction (one firing for every
  record of every table), a parent's commit with its related records
  (Delete Portal Row lists only the related record). Not for a commit with
  nothing changed, a revert, or a failed commit (507).
- Parameter: FileMaker's formatted JSON, `{ "<file without extension>" : {
  "<base table>" : [ [ "New" | "Modified" | "Deleted", record id, content
  ], ... ] } }`, base tables in key (alphabetical) order, records in the
  order they were changed. The content is the record's value of a field
  named OnWindowTransaction (or the trigger's field name option, looked up
  in every table), a JSON object or array as itself, anything else (a
  number too) as a string, empty without such a field; a deleted record's
  value is read before the deletion.
- Timing: not at the commit. Each commit queues the trigger's script below
  the running script; the queued scripts run, oldest first, when the
  running script and its callers end. LastError after the commit is 0.
  (U10.)

### 13.27 Validation inside a transaction (s35-txvalidation)

A field validated by calculation, Always or only during data entry; Set
Field of an invalid value inside Open Transaction, then a step that leaves
the record (Commit Records, Go to Record) or none, then Commit Transaction:
Set Field 0, the leaving step 0 (Go to Record moves), Commit Transaction
507 and everything reverted. Open Transaction's "Skip data entry
validation" skips the data-entry-only rule (committed) but not the Always
one (507). Outside a transaction Commit Records and Go to Record are 507
and Go to Record stays.

## Unknowns

- U1. Client `CheckError`, the FileMaker Pro alert texts and buttons for
  confirm-type errors (4.4), and how the client presents error 4100 (no
  target field) are not in the server build. Needs the Mac client binary or
  the oracle. Observed so far: the alert for a cancelled step (error 1;
  Implementation notes (desktop client), Errors and dialogs).
- U2. Exact `MapError` behavior for internal code 2045 (a debug message is
  printed; result 100 inferred) and `iwp::IWPScriptRuntime::MapError`.
- U3. Settled (run, 13.3): a failing condition leaves `Get(LastError)`
  unchanged and is true (its value `?` is true as a boolean).
- U4. Settled (run, 13.4): 1 Always, 2 Minimum, 3 Defer.
- U5. Server runtime settled (run, 13.11): option 5 behaves exactly as 0
  (Continue). Whether the FileMaker Pro client treats it differently (button
  or web-viewer code) is still unknown.
- U6. Button single-step frames: the mapping of the stored step value to
  the current-script option (0 treated as Pause) needs confirmation against
  buttons saved with each option.
- U7. Settled (run, 13.7): 92, 97, 166 return 0; 180 returns 3; 146
  without an object returns 5.
- U8. PSOS: the order of the two callback ids in the request and whether
  plain PSOS sends zeros; the semantics of the second reply string (extended
  error) for non-extended errors (probably empty); timer interval of the
  synchronous wait; behavior when the client disconnects during a wait.
- U9. Values settled (run, 13.10): 1 while open (sub-scripts included),
  else 0; revert-on-error 1 by default, 0 after Off, 1 after the script.
  The calc-side code path was not traced.
- U10. Settled (run, 13.26): the JSON (formatted, tables in key order)
  and the moment: queued below the running script, run when it ends.
- U11. Which steps use the append-to-variable path (5.3) is open.
  Repetition handling observed (run, 13.6): 0 and -1 valid, fractions
  truncated, `$v` = `$v[1]`; invalid names `$`, `$$`, `$1x` give 5.
- U12. WebDirect and Data API runtimes were only scanned for their dispatch
  tables; their pause, error and abort overrides were not analyzed.
- U13. Not reachable with the script oracle: fmsased's own step handlers
  (13 scope), PSOS on the wire and callbacks (U8), user cancel (error 1)
  inside transactions, button single-step frames (U6), and the client's
  alerts (U1). The FileMaker Pro oracle (`tools/calc-oracle`) is the route
  for those: paste `fmxmlsnippet` step lists into a scratch file's script
  and run them with AppleScript `do script`.
- U14. Triggers the script oracle can run but s14 did not: OnModeEnter /
  OnModeExit (mode steps), OnRecordRevert, object triggers, mode filters,
  when a trigger's parameter is evaluated, OnRecordCommit together with
  OnLayoutExit for one Go to Layout, and a trigger script causing its own
  event. A battery's `@trigger` lines add them (13.17).
- U15. Schedules on a real FileMaker Server (fmsased's own code, beyond
  13.23). Probe on a FileMaker Server 26 with a probe database of its own
  (a copy of contacts-en with the s36 scripts and a script `wait` that
  runs `Pause/Resume Script [ Duration: 90 ]`), schedules made in the
  Admin Console, reading `Event.log` (type, id and text of every line)
  and the Schedules page's status after each:
  (a) `wait` every 1 minute: does the next start wait, run beside it or
  get skipped, and what is logged; (b) `wait` with a 1-minute time limit,
  "End the schedule if the timeout is reached" off, then on: the lines
  and the status (and the option's default in a new schedule);
  (c) `sched error` (a step error, then success) and `sched error then
  ok` changed to end on the failing step: the step-error line of a
  schedule and Succeeded or Failed; (d) the database closed, then paused,
  when the schedule fires; (e) the schedule's account disabled, then its
  password changed; (f) "Run now" on a disabled schedule and on one that
  runs; (g) a schedule's own email notification: when it is sent and
  what it says. Each settles a point "Implementation notes (schedules)"
  marks as OpenMaker's choice.
- U16. Whether FileMaker Pro opens a file for a script performed in it
  hidden or visible (9.2: the help says neither), and whether Select Window
  run from within a file also shows one of its windows hidden by Adjust
  Window [ Hide ] rather than a hidden file (the help speaks of a hidden
  related file). Route: the FileMaker Pro oracle with two local files, A
  performing a script in B (closed) that does nothing, then one that runs
  Select Window [ Current Window ], each followed in A by Select Window
  [ Name: "B" ], recording `Get ( LastError )`, `WindowNames` and B's
  visibility after each step.

## What other workstreams need

- `script-steps.md`:
  - Per-step compatibility comes from the binary table
    `sStepsUnstoredFlagsTable` (libFMEngine 0x10ff900, u32 per id 0..247;
    bits 2 Mac, 4 Win, 8 Server, 0x10 Go, 0x20 Data API/CWP, 0x40 WebDirect,
    0x80 not button-compatible). `docs/re/catalogs/script-step-compat.csv`
    has it next to the Claris columns; it matches Claris except the
    handful of steps listed in 10.1.
  - Numeric data slot 0 (`[.., 00 00, 03]`, 4-byte int, `ScriptStep::GetNumericData<int>`
    = step data key + `HBInt16Key(slot)` + `KUnique` (03)) stores a second
    index: the matching End If for Else If, the enclosing End Loop (or -1)
    for Go to Record/Request/Page and Go to Portal Row. It is written even
    when "Exit after last" is off (corpus: always `ffffffff`).
  - `GetListValue` reads list options at step data key + `HBInt16Key(slot)`
    (Loop Flush is list slot 1, guarded by flag 0x100).
  - Flags used by the runtime: kTurnOn 0x20000 for Set Error Capture, Allow
    User Abort, Set Layout Object Animation, Set Revert Transaction on Error;
    kCalculation 0x4000 for Exit Script result, Perform Script parameter,
    Pause duration; kExit 0x2000 for Exit after last; kOption 0x100 for PSOS
    async (shown inverted as "Wait for completion"), Loop Flush present,
    Open Transaction skip validation, Revert Transaction condition;
    kOption2 0x200 for Commit Records / Open Transaction override ESS
    locking, Revert Transaction custom error; 0x1000 for Open Transaction
    skip auto-enter; 0x80 kNoInteract on Export Records changes error
    suppression.
  - Calculation slots: Set Variable value 1, repetition 2, name = text data
    slot 0; Revert Transaction condition 0 (inf), error code 1, message 2;
    If/Else If/Exit Loop If/Exit Script/Pause duration/Perform Script
    parameter slot 0.
  - Step 186 is the plug-in step; ids 224 and 239 are feature-gated
    (error 4 in 26.0.3); the obsolete id list is in 2.5.
- `calc-functions.md`: `Get(LastError)`, `Get(LastErrorDetail)`,
  `Get(LastErrorLocation)` (format in 4.6), `Get(ScriptParameter)`,
  `Get(ScriptResult)`, `Get(ScriptName)`, `Get(ErrorCaptureState)`
  (= `mCaptureErrors`), `Get(AllowAbortState)` (= `mAllowAbort`),
  `Get(ScriptAnimationState)`, `Get(TransactionOpenState)`,
  `Get(RevertTransactionOnErrorState)` (= `mAbortImmediately`) must read
  the runtime state described here; `Let ( $x ...)` writes the current
  frame's variable map with the `[repetition]` key rule (5.2).
- `network-protocol.md`: opcode 30 request/reply and opcode 105 layouts
  (11.2, 11.4); error 812 on script-engine capacity.
- `layout-model.md`: trigger records (`ScriptTrigger`: event id, script
  key, file reference, parameter formula, mode flags 1/2/4).
- OpenMaker runtime implementers: section 13 is ground truth from the
  engine; new questions can be put to `tools/fms-script-harness` (a probe
  script is a few lines of FileMaker-style steps in a battery file, layout
  triggers are `@trigger` lines).
- Script hosts (desktop client, server): queue a layout switch's
  OnRecordLoad before its OnLayoutEnter and fire OnRecordLoad also with an
  empty found set (13.17); keep holding a step's event under an action
  token for a pre-event trigger and return success: the runtime turns the
  step into 3000 and stores the action's outcome as the step's when the
  trigger lets it happen.
- OpenMaker Server: run PSOS synchronously per request in a fresh session,
  return `(extended text, result, error)` with the error of 13.1/13.7
  (`Runtime::final_error`: 4 after an abort, else `Get ( LastError )`);
  return 3 for steps without the Server bit except those of 13.7; abort on
  error 4 only when Allow User Abort is On.
- Coordinator (shared docs): `architecture.md` 4.1 should name the server
  runtimes (`FMS::SASEScriptRuntime` in fmsased, `iwp::IWPScriptRuntime`
  in fmscwpc, `FMWIP::WIPScriptRuntime` in fmwipd) and note that
  `ScriptStep::IsServerCompatible` reflects the Script Workspace filter,
  not a step property; README could list this document and the new
  catalog.

## Implementation notes

`om_scripts::runtime` (2026-10-28; corrected 2026-10-29 against the script
oracle, section 13) implements this document. Where the document leaves a
point open, the runtime settles it as follows; points marked (unconfirmed)
still need a FileMaker run, points marked (run 13.x) were settled by the
engine.

Differential test

- `crates/om-scripts/tests/oracle.rs` replays every run line of the
  batteries in `tools/fms-script-harness/batteries` on the runtime as
  `Client::Server` over `MemoryScriptHost` holding the probe file's subset
  (the Contacts template: table, fields, layouts, the Contact List
  OnLayoutEnter trigger, Sort by Name; `tests/support/contacts.rs`), with
  the probes built by the codec's edit API (`tests/support/battery.rs`). It
  compares the request's error and result, the final `Get ( LastError )`,
  the run line's expressions, every executed step (depth, script, line,
  id, step error, control step, `Get ( LastError )` after it), the event
  log and the `@call`s with the engine's results, and the probes' block
  indexes with `ScanScript`'s. A battery's `@trigger` lines (s14) replace
  the named layouts' triggers in the host, as they do in the generated
  probe file. Every run of s00-s09 and s11-s14 matches (256 runs). s10 runs
  the FileMaker Server sample, which is not reproduced in memory; its file
  triggers are fired by the server host around a request, not by the
  runtime (13.1). Remaining differences are listed in the test (`KNOWN`).
- Host answers the fixture supplies rather than the runtime: the server
  session's `Get` selectors (13.1), `ExecuteSQL` (om-sql over the memory
  records), Cancel PDF doing nothing, Perform Script on Server failing with
  3 (the file is not hosted). `MemoryScriptHost` follows the engine where
  the probes touch it: Set Field into a calculation or summary field is
  201 (before the record check), Delete Record without a record is 0, Go
  to Layout to the layout shown fires nothing, a layout switch calls
  OnRecordLoad (also without a record) before OnLayoutEnter, Go to Record
  [Next/Previous] in an empty found set finds no further record (Exit
  after last applies), and a new session's window shows the last record
  (13.16, 13.17).

Execution model

- Frames live on a heap stack; queued frames (delayed triggers of the
  user's events, timers, callbacks, `ScriptCall::queue_at_bottom`) are
  inserted at the bottom as `QueueScript` does, so they run when the frames
  above end and see `Get ( LastError )` as the ended script left it; a
  script's Exit Script result goes to the frame below it, so a queued frame
  sees the result of the one that ran above it, except the final frame's,
  which goes to the request (run 13.14). Nothing recurses on the Rust stack
  per script call; 20,000 nested Perform Script calls are tested.
- The script `Runtime::perform_script` starts on an idle runtime is the
  final frame (`SMScriptRuntime`, 11.3): its Exit Script result is
  `Runtime::final_result` and goes to no caller; a sub-script's result,
  and that of a script started beside it (a call that halted or exited it
  first, one queued below it), never is (run 13.1, 13.11, 13.14). The
  final result and error start over with each request on an idle runtime,
  not when the scripts halt (scripts queued below the request's still run
  after it). `Runtime::final_error` is the request's error: the final `Get
  ( LastError )`, or 4 after an unknown step aborted the script (run 13.7,
  also from a sub-script, 13.16).
- The stack memory guard follows 13.18 (`runtime/stack.rs`): a counter of
  script calls made while the stack holds 100 or more frames
  (`script_frame`, so Perform Script, host-started scripts and triggers);
  the first and every 100th such call estimate the frames on the stack: a
  script frame 1,568 bytes, a button's step frame 0x128 (2.6, not
  probed), a parameter or `$` value 1,248 bytes plus 2 per code point (a
  container its reference text and stream bytes; numbers, dates and times
  the fixed part alone), each `$` variable 28 bytes more plus 2 per code
  point of its name, and 512 once. Above `Options::stack_memory_limit`
  (FileMaker's 1 GiB by default) the call fails with 2 after the 104, 9
  and 5 checks and before the parameter is copied. The values lie inside
  the ranges the runs allow and reproduce all 21 lines of s15 exactly.
  A recursion without parameters or variables reaches about 685,000
  frames before error 2 (with a short parameter about 381,000); OpenMaker's
  frames are far smaller than FileMaker's, and the server's step budget
  and time limit bound such a script further.
- There is no step or depth limit. `Options::step_budget` is a host safety
  budget per top-level script (unlimited by default) and
  `Options::time_slice` returns from `Runtime::run` like the 2000 ms slice.
  `Runtime::step` executes one step (a debugger's Step); with
  `Runtime::set_tracing` every executed step is recorded as the oracle
  traces it (`Runtime::take_trace`).
- Multi-stage steps (internal 3001): a host method returns
  `HostError::Pending`; `run` returns `Waiting`. `Runtime::complete_step`
  finishes the step with its error (post-step cleanup as usual); running
  again without a completion re-enters the step, as FileMaker does.
- A host that lacks a capability returns `HostError::Unsupported`, reported
  as FileMaker's error 3 for a step without a handler.
  `Options::stop_on_unsupported` halts with the step's location instead (the
  desktop client, the corpus report).
- The compatibility table (`script-step-compat.csv`) is applied to every
  step before dispatch for every client, so a Mac client also gets 3 for the
  iOS-only steps. On `Client::Server` the steps the engine reaches without
  the table's Server bit behave as the engine does (run 13.7): Show/Hide
  Text Ruler, Set Zoom Level and Show/Hide Menubar do nothing and return 0,
  Set Web Viewer finds no object (5; unconfirmed for a named web viewer on
  the server's layout), and Exit Application closes the session (the host
  is told), ending every script with its 13 unstored. Custom Web and
  WebDirect keep the table (their dispatchers differ, 10.2).
- The 4100 check (a button's single step that needs a target field, 2.3
  check 3) is left to the host: it returns its own error. (unconfirmed)
- Loop's Flush option is read but not applied; the host API has no record
  flush modes yet.
- Run with full access: `ScriptHost::script_access` says whether a script
  is executable and runs with full access; the runtime raises the file user
  through `set_full_access` around each step of such a script.

Errors

- Every nonzero step error reaches `ScriptHost::error_alert` on a client
  unless error capture is on (never-suppressed codes always reach it) with
  `confirm` set for `SharedErrorsThatCauseConfirmAlert`. Cancel halts when
  Allow User Abort is On; error 4 halts after the alert when Allow User
  Abort is On. Which errors FileMaker Pro reports silently (for example 101
  from Go to Record [Next] at the last record) and the alert texts are the
  host's choice (U1, unconfirmed); the desktop host stays silent on 101.
- Server-type clients (Server, Custom Web, WebDirect) never alert and
  error capture changes nothing for them; codes below 3000 and 5000-5499 go
  to `ScriptHost::log_error` whether capture is on or off (run 13.2). An
  unknown or gated step (4) with Allow User Abort On aborts the script,
  turns triggers off and leaves `Get ( LastError )` 1 (run 13.7).
- Internal codes (3000-4999, 5500 and up, and -1, which is 65535 as
  FileMaker's unsigned code) are never stored; only the never-suppressed
  ones reach `CheckError`, and the server logs none of them. Step id 0 thus
  leaves `Get ( LastError )` as it was (run 13.2); a client alerts it even
  with capture on (inf from `NeverSuppressError`, unconfirmed).
- Control steps never reach `CheckError` (no alert, no log line): a failed
  Commit Transaction is stored at once and reported only with a custom code
  (run 13.2, 13.10; inf for the client's alert).
- If, Else If and Exit Loop If take the Boolean of the condition's value: a
  calculation that fails gives `?`, which is true (the branch is taken, the
  loop exits); `Get ( LastError )` is not touched (run 13.3; the earlier
  "false" was wrong).
- Calculation errors in a step's parameters are not step errors: the step
  goes on with `?` and stores 0 (run 13.2).
- `Get ( LastErrorLocation )` is cleared when `Get ( LastError )` becomes 0
  (run 13.10: empty after a 104 inside a transaction that then committed;
  run 13.16). The location (and the log line) of a step's error is the
  step's name at its frame's index after the step: Go to Record's 101 after
  Exit after last left a loop is located at the End Loop (run 13.16).
  After a failed Commit Transaction it is the Commit Transaction step;
  `Get ( LastErrorDetail )` is the location (script, step name, line) of
  the step that failed the transaction, followed by the message for a
  custom revert code (run 13.10; the earlier reading had them the other way
  round).
- `MapError`'s 802 -> 100 case needs a file error detail the host API does
  not carry; 802 is kept.

Variables, parameters, results

- Script parameters and Exit Script results keep their data type: a date
  parameter plus 1 is the next day, a result `0012.50` is the number 12.5
  (run 13.5; the earlier "become text" was wrong). Perform Script on
  Server's parameter and result are text (the wire, 11.2).
- Variable names: `$` or `$$`, then a calculation name: not starting with a
  digit or whitespace, without calculation delimiters, brackets, `$`, `::`
  or trailing whitespace (`$`, `$$` and `$1x` are 5, `$a b` is accepted;
  run 13.6). A bracketed repetition in Set Variable's name is error 5. A
  repetition that evaluates to `?` is error 5 and assigns nothing (run
  13.16; calc-values.md 8.3's Let rule assigns nothing without an error).
- Insert steps (Insert Text, Insert Calculated Result, Insert Current
  Date/Time/User Name) into a variable append text unless "Select entire
  contents" is on, a container being replaced (5.3, U11, unconfirmed);
  result steps (Insert from URL, data files, Execute Data API, Get Folder
  Path, host-generic steps) replace the variable.
- A button's single-step frame uses the variables of the script below it.
- `Get ( LastMessageChoice )` is 0 until a dialog is answered (run 13.1).

Script calls

- Perform Script by name: list value `[00 1]` = 1 with the name calculation
  in slot 2 (corpus), or a calculation in slot 1 without a stored key
  (the CSV's form, used for PSOS). A plain name is compared without case
  and a trailing space does not match (104) (run 13.16); `source::script`
  resolves the data source, where a name the file has no data source for
  (its own name included) is 104, and compares the script name with case
  (unconfirmed). A layout button's Perform Script reads the script key in
  `mReserved` even without `kScriptKey`.
- A host-started script (`ScriptCall`) applies its "current script" option
  to the running scripts first (3.7): 1 halts them, 2 exits the current
  frame, 3 resumes, 4 pauses (a server host resumes after its short pause,
  so 4 has no visible effect there), and 0 and 5 do nothing; then the
  script runs at once on top, or after the running ones with
  `queue_at_bottom` (run 13.11; from sub-scripts, paused scripts and called
  scripts alike, 13.14).
- A button's queue option (3.6 step 4, U6) applies to the script running
  below the button frame: 0 Pause, 1 Halt, 2 Exit, 3 Resume.
- Pause/Resume: the host blocks in `ScriptHost::pause` or returns Pending,
  which pauses the frame until `Runtime::resume`. An indefinite pause on a
  server-type client is 0.1 s. A canceled pause with Allow User Abort On
  stores 1 and halts.
- Perform Script on Server: the host returns the server's result and its
  `Runtime::final_error`, which becomes the step's error; the frame's
  result is replaced (cleared without waiting). A parameter over 1,000,000
  characters is 513 before the call.

Transactions

- Only the data layer fails an open transaction (`SetTransactionError`,
  8.4): an error of a step that writes records (New/Duplicate/Delete
  Record, Delete All Records, Delete Portal Row, Open Record, Commit
  Records, Set Field, Set Field By Name, the insert steps, Cut, Paste,
  Clear, Import Records, Replace Field Contents, Perform Find/Replace,
  Relookup), or a user cancel. Perform Script's 104, Go to Record's 101, an
  unsupported step's 3 and a sub-script's own Open or Commit Transaction
  leave it alone: the steps go on and Commit Transaction commits (run 13.10;
  the earlier "every error" was wrong).
- Open Transaction while one is open, in this frame or a caller's, is 3 and
  the open one goes on (run 13.10; not -1). Commit Transaction without one
  or outside the opening frame is 3, stored at once, not logged.
- A pending Open Transaction that completes starts the transaction; a
  pending Commit Transaction keeps the transaction open until the commit is
  entered again or completes (1.7).
- With Revert on Error Off, Commit Transaction commits and returns the
  commit's own error.
- Revert Transaction marks the transaction reverted and returns 0 itself,
  also with a custom code; it stays open (its steps skipped) until Commit
  Transaction closes it and returns the recorded error (the custom code,
  logged at the Commit, or 0) (run 13.10).
- Halt Script and Exit Script inside a failed transaction are skipped like
  every other step (the skip rule of 2.3 has no exception for them); the
  Commit Transaction then returns the error and the steps after it run
  (run 13.15).
- Revert Transaction is skipped when the transaction already failed: the
  first error stands (a second custom code, or a custom code after a data
  error with revert on error On). With revert on error Off a data error
  skips nothing and a later custom code becomes the Commit's error. A code
  outside 5000-5499 still reverts, and Commit returns 0 (run 13.15).
- Set Revert Transaction on Error in a sub-script applies to the caller's
  transaction (the state persists, run 13.15).

Triggers

- `Runtime::fire_trigger` takes the event, the trigger record's script,
  parameter calculation and modes, the window's mode and an action token
  for the event's default action. Triggers a step causes are collected by
  `ScriptHost::take_triggers` right after the step and called before its
  error is stored, as FileMaker calls them inside the step: calling them
  sets `Get ( LastError )` to 0, then the step's own error is stored (run
  13.17).
- A trigger a step fires runs right after that step as a frame on top of
  its script, delayed or not; two triggers of one step run last-fired first
  (run 13.12; the earlier "a delayed trigger a step causes runs after the
  whole script" was wrong). A host queues a layout switch's OnRecordLoad
  before its OnLayoutEnter, so OnLayoutEnter's script runs first, and
  OnRecordLoad also when the found set is empty (run 13.17). Pre-event
  triggers of the user's own events
  run on top of any running or paused script, delayed ones queue below a
  running script. Window and file events are suppressed while any frame is
  on the stack.
- Only a trigger that runs before its event can cancel it; a False result
  of a trigger that runs after its event (OnLayoutEnter, OnRecordLoad, ...)
  is ignored and stores no error 20 (run 13.17).
- A pre-event trigger that a step caused and that holds the event's action
  holds the step: the step returns 3000 (not stored, not reported) and the
  trigger frame remembers it. When the frame ends, the action reaches
  `ScriptHost::trigger_action` after the step: `proceed` for a true result,
  no Exit Script result, or an end without Exit Script; then the step runs
  again, which for the runtime is the action's outcome stored and traced as
  the step's (and the triggers the action causes are called as the step's).
  False, a text such as `"abc"` and an empty result cancel: 20 is stored,
  the action is dropped and the step does not run again (run 13.17; the
  earlier "the action is performed and the step's first error kept" was
  wrong). A trigger that does not fire (mode, master switch, call failure)
  has its action performed at once, and its outcome is the step's. The
  action of the user's own event is performed through `trigger_action` as
  before (no step to run again). A trigger script that is halted never
  reaches its exit, so its action is dropped (run 13.17). The runtime does
  not re-attach actions to an outer frame (7.4's last sentence).

## Implementation notes (PSOS)

OpenMaker's Perform Script on Server (2026-10-28; transactions, record
rules, privileges and limits revised 2026-10-29; related records, sorts,
stored finds, repetitions and the file's locale 2026-10-29; unstored
calculations and summary fields 2026-10-29; five review fixes 2026-10-29:
privileges per step on an open record, saves that fail, validation of
every repetition, schema changes while a script runs, aborts inside a
step):
`om-protocol::script`
(op30, op105), `om-server` (`scripts`: the engine and its `ServerHost`),
`om-session` (`perform_script_on_server`, `poll`) and `om-provider`
(`DataProvider::perform_script_on_server` and friends); the record rules
are the desktop client's, shared in `om-data` (`rules`). Nothing here was
captured from FileMaker; every point below marked (inf) needs the capture
listed at the end.

Wire (11.2, 11.4)

- The parameter's `Push(Text)` is taken as the unistring of its characters
  (styles are not sent); no parameter is the empty unistring, so the
  server's `Get ( ScriptParameter )` is empty text then (inf).
- The script key is the catalog member's key segment as `[17, 5, S]` stores
  it (minimal big-endian bytes, what `uint_segment` writes), then its 1-byte
  length; at most 7 bytes (`HBSerialKey<7>`) (inf for ids of 0x80 and up,
  where the serial-key and stored forms could differ).
- Callback ids: argument 8 (the client's callback id) is pushed first,
  argument 7 second; `om-session` sends the server's file id as argument 7
  and both as 0 for plain PSOS (inf). They are sent when the Login reply
  named version 9 or more; OpenMaker Server, which names 9, reads them and
  also accepts a request without them (a client that has not logged in).
- The reply always carries the extended error text, empty (U8, inf); the
  result is cut to 1,000,000 characters (inf); the client clears it when it
  did not wait. A callback result over 1,000,000 characters is sent as 736
  with an empty result.
- op105 travels as a CORBA `Perform` request on the client's callback
  object (object key `FMRPO`) over the bidirectional connection, as
  `RCSendPSOSRetToClient::Perform` on the client suggests: odd GIOP request
  ids (GIOP 1.2: the accepting side's), the client's connection id, an empty
  `sessKey`, the default `maxreturn`; the transaction header is a
  file-scoped one (opcode 105, a server-wide counter as transaction id,
  session id 1, the file's id, idle time 0). No LocateRequest precedes it
  (FileMaker Server sends one before `Notify`). The client answers with a
  `Perform` reply whose result stack is `u16 0`; the server does not wait
  for it (all inf). The call is sent right after the server's next reply to
  that client, or within 100 ms while the client is quiet (the connection
  reads with a timeout to look for waiting calls).

Server engine (11.3, 11.5)

- At most 100 scripts execute at once by default, waiting and not; one
  more is refused with 812 in the op30 reply, and a refused callback
  request sends no op105. A script that does not wait is refused the same
  way when the system will not start a thread for it (OpenMaker's choice
  of the host-capacity error; what FileMaker Server answers then is not
  observed). `ScriptLimits` (`Server::with_script_limits`,
  `om-server --scripts/--script-steps/--script-seconds`) also bounds each
  script: by default 100,000,000 steps (the runtime's step budget) and 30
  minutes. FileMaker has no such limits (2.6); they are OpenMaker's, so
  that a script that never ends cannot keep its locks, transaction and
  slot. A script's calculations build text values of at most 256 MiB
  (`Server::with_calc_limits`, `--calc-text-mib`; scripting.md,
  Calculation limits), also OpenMaker's.
- Every executing script is registered with an abort flag
  (`ServerHandle::scripts`, `abort_script`, `abort_file_scripts` for a file
  that closes, `shutdown`). The session checks it, and the time limit,
  before every step (`abort_requested`, `IsServerAbort` in 2.3) and every
  100 ms of a pause (6.2). An aborted script, or one past its step limit,
  stops as an administrator's disconnect stops it: error 1 (which, as
  `SetLastError` does, never hides an earlier error, so a loop that failed
  with 201 still reports 201), Halt Script's revert of the open
  transaction, then the session reverts its uncommitted record instead of
  committing it, and releases every lock. Shutdown stops accepting
  connections (open ones stay), refuses new scripts with 820 ("File is
  being closed", OpenMaker's choice) and waits for the aborted ones.
- Long work inside one step checks the same flag and time limit, so an
  abort does not wait for the step: ExecuteSQL every 256 rows it scans,
  joins, filters, groups, aggregates or projects (`SqlSource::interrupted`,
  through om-data's `CalcSource::interrupted` from the session; the query
  ends with FQL's engine error 1, "Operation aborted", and the script with
  the abort's 1), finds that test candidates and sorts every 256
  records (error 1), and Import and Export Records before every row and
  record (error 1; the import's rows are dropped, its own transaction or
  taken out of the script's, and the export writes no file; before, an
  import ran on past the limit and committed every row). Before, a three-way join kept a script past its time
  limit and past shutdown's wait for minutes. A calculation asks
  (`CalcHost::interrupted`) every 16 custom function calls, `While`
  iterations, `Evaluate`s and unstored calculations, and `Position`,
  `PatternCount` and `Substitute` about every million characters they
  compare or copy: it ends with error 1 and the result `?` (which
  `EvaluationError` does not catch), the step goes on with that value,
  and the script stops before its next step with the abort's 1. A record
  rule's calculation (auto-enter, stored calculation, validation) stopped
  this way refuses the commit, so the record stays uncommitted and is
  reverted, and Set Next Serial Value refuses (1) once the script is
  aborted. Before, one formula of nested `While`s (50,000 by 50,000
  iterations, about an hour) kept a script past every limit. FileMaker
  has no stop inside a calculation; error 1 there is OpenMaker's choice.
  The registry is checked for "shutting down" under its own lock, so a script that starts while the
  server shuts down is either aborted with the others or refused (820);
  a slot is released even when its thread panics, and a session dropped
  without closing (a panic) still releases its locks. Shutdown logs the
  scripts that outlive its wait. Recalculation after a schema change runs
  in op73, never inside a step, so no script waits on it except for the
  store's lock.
- A script that does not wait goes on when its client disconnects
  (FileMaker runs it to the end; its callback result is dropped), within
  its limits. A waiting script runs on the requesting connection's thread,
  which does not read the connection meanwhile (U8), so a disconnect does
  not stop it either.
- Step errors go to the server's log as `LogOrCountError` does (4.9): the
  same error at the same step repeated is logged once with its count, at
  most 100 lines per script, then only counted (the total is logged at the
  end); the session keeps the last 64 entries. Before, every error was kept
  in memory.
- 100 when the file is not open in the requesting session or the session
  has not logged in to it (no host file user).
- A waiting request runs on the requesting connection's thread; its reply's
  back-channel carries the change notifications of the script's commits
  (11.1 step 5). A request that does not wait is answered with 0 at once
  and runs on its own thread; its 104 or 112 only reaches a callback.
- Each request is a session with a connection id of its own (drawn from the
  connection-id set and released at the end). It is the lock user (one
  transaction, 1) and the sender of its notifications, so every registered
  client is told of its commits, the requester included. Other users see
  its locks as held by `<server name> (script engine, <account>)`.
- Privileges (9, 11.3): the session runs with the requesting account's,
  as FileMaker Server's engine opens the file "as the same account"
  (11.3). An account in one of the file's verified built-in sets
  (local-files.md 2.3, session-identity.md 2.7) has that set's
  privileges: [Full Access] everything; [Data Entry Only] every script
  executable, every record read, created, edited and deleted;
  [Read-Only Access] every script executable and every record read, with
  creating, changing, deleting, setting a global field and importing
  refused with 200, the code a client's own write under the set gets
  (inferred: FileMaker Server's codes for a server script under these
  sets are unmeasured, with
  [#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)). Truncate
  Table needs [Full Access] whatever the account may delete (Claris's
  Truncate Table help), and Set Next Serial Value, which changes a
  field's definition, does too (inferred); both are 9 otherwise. An account in a custom set fails closed
  (`Privileges::Unknown`): the set's script, layout and record limits are
  not applied to server scripts yet. Its scripts are not executable
  (op30 answers 9 and nothing runs) unless set to run with full access
  privileges: then the script runs and each of its steps has full access
  (2.2, 9.1). A sub-script it calls may execute
  (`mParentRunWithFullAccess`) but runs with the account's own privileges
  unless it is also set to: there, and wherever the account is not
  elevated, creating, changing, deleting, truncating, setting a global
  field or a next serial value and finding fail with 9, and reading a
  field is calculation error 9 (`?`, not FileMaker's `<No Access>` text).
  Each edit is checked against the
  privileges of the step making it: a record an elevated caller opened
  (changed and not yet committed) is still refused to its sub-script
  (before, the open record let the sub-script write; review fix).
  Committing or reverting the open record needs no privileges of its own
  (inferred: they write or drop only what an allowed step changed). The flag is bit 0 of the script's
  private member flags `f1` (9.1, script-steps.md 1); no sample file sets
  it, so the key is inferred from its name (`Script::runs_with_full_access`).
  The client-side default of `ScriptHost::script_access` (everything
  executable) is unchanged: the desktop client gets data privileges from
  the server.
- The window is on the first layout in catalog order whose occurrence
  exists, with all records of its table in creation order; the file's
  "switch to layout" option is not decoded. A file without such a layout
  is 112.
- Records change as FileMaker Pro changes them: a step that first changes a
  record locks it (write lock; refused: 301) and copies it; leaving the
  record (Go to Record/Layout, a mode or found-set step, New Record, Commit
  Records) or the end of the script (closing the window) commits it with
  the file's rules (below) and op70's machinery (`writes::commit_records`):
  new records added, edits written whole with `fc` one higher, deletions
  removed, each commit checked first (table, locks, issued ids, records
  that must exist) and written all or nothing under one hold of the store.
  A commit the rules refuse fails the step with their error and the record
  stays open, as the script changed it (the rules are applied to a copy,
  so committing it again applies them again); at the end of the script it
  is reverted.
- Saves: a commit counts once the file on disk holds it. The store writes
  the commit's records (keeping their old versions in a journal), saves
  the whole file (a new copy flushed and renamed over the old,
  hbam-writer.md 6.2) and, when that fails, puts every record back and
  fails the commit, with the records still locked: the server never holds
  a commit the file lacks (OpenMaker's rule; FileMaker Server's handling
  of a failed write is not observed). A script's step gets FileMaker's
  error (OpenMaker's choice: 809 "Disk/volume is full" for a full disk,
  quota or size limit, 810 "Disk/volume is locked" for a read-only file
  system, else 800 "Unable to create file on disk"): Commit Records (and
  every step that commits by leaving the record) with the record still
  open, Commit Transaction with the transaction reverted, Delete Record
  with the records kept, Set Next Serial Value with the old value kept.
  Before, the save's failure was only logged and the step reported
  success over changes only memory held. op70 follows the same rule: a
  client's commit that cannot be saved is taken back and logged; its
  reply cannot say so (write-path.md 3.4), so the client learns it by
  reading the records back (OpenMaker's client does, docs/record-writes.md).
  Saves of changes that may wait (op71's reserved ids, op72's serials,
  indexes built on demand, schema and container uploads) still keep them
  in memory for the next save and log the failure as an error when the
  disk failed. The store never takes what the file cannot hold (key
  segments of 1 to 219 bytes, paths of 16 segments, a page's room, no key
  with both a value and children): index keys are cut as FileMaker cuts
  them (query-program.md 7.1), and every write is checked against the tree
  as it is (`om_hbam::writer::put_refusals`) before it is made. A write
  beneath a node that holds a value, or beyond the limits, is refused with
  its operation's refusal (op13 logs it, op86 fails with `ffff`, op71
  `UNAVAILABLE`, op72 a missing field, op88 bad data, op87 the failed
  count, GenerateMemberKey `ffff`) and changes nothing; a commit that
  holds one is taken back whole and fails (a script's with 800). After a
  failed save only field indexes are ever taken out (each whole, rebuilt by
  the next find that reads it); anything else the file cannot hold is kept
  as it is and logged, and saves fail rather than delete data. A file that
  loads with content beyond the limits is served read-only. A commit
  writes each table's record list `[T, 0d]` and each changed index's
  counts once, at its end (kept in its journal, so a failed save puts
  them back too), and the lock table is indexed by path and connection:
  a commit of n records costs about n, where it cost n² and held the
  store for tens of seconds in a 20,000-row import (2026-10-29). A commit
  that panics puts back what it wrote (`Store::guarded`), and the store
  and lock table are taken through `Hosted::read_store`, `write_store`
  and `lock_table`, which go on after a lock a panicking request
  poisoned; before, every later request panicked until a restart. A partial
  copy is removed; a failure to flush the directory after the rename is
  only logged (the new file is already in place).
- Open Transaction keeps the changes in an overlay keyed by (table,
  record): the record's latest version with every field changed since
  Open Transaction, or its deletion. Reads, `Get` selectors, found sets
  (Show All Records, Show Omitted Only, Go to List of Records), finds and
  deletes see it, so a record changed, left and changed again keeps both
  edits (it reopens from the overlay; Revert Record goes back to the
  overlay's version). Finds run on the file's indexes for the records the
  transaction did not change and on a scratch store of the changed ones
  (`Store::scratch`, indexes built on demand). Locks of changed records
  are kept until the transaction ends. Commit Transaction applies the
  rules to every changed record (auto-enter at commit, as
  `open-transaction.html` says; with the transaction's Skip options) and
  writes them all in one commit (a row Import Records staged already had
  the import's own rules, script-steps.md "Implementation notes
  (import/export)", and is written as they left it, only cells a later
  step changed getting the transaction's); a refusal reverts the whole transaction
  and is Commit Transaction's error (507 for a validation calculation,
  for example), across tables. Revert Transaction, Halt Script, an abort
  and the end of the script drop the overlay (the record ids New Record
  reserved are not given back). Stored calculations of a changed record
  read their old value until Commit Transaction recomputes them
  (unconfirmed). Truncate Table inside a transaction is reverted with it,
  although FileMaker cannot revert it (`open-transaction.html`;
  deviation).
- Record rules: the desktop client's planner (`om_data::rules`, auto-entry.md,
  docs/record-writes.md), with `Entry::script`: auto-enter values (creation
  values for new records, modification values on every write, constants,
  auto-enter calculations), stored calculations recomputed when a field
  they read changes, validation (not empty 509, by calculation 507,
  maximum characters 511, unique values 504 checked against the field's
  value index, or a scan, and the session's own changed records; for
  repeating fields see "Validation" below). A script may set any normal
  field that is not a container,
  in any of its repetitions, also one whose auto-entered value prohibits
  modification during data entry: scripts ignore that option (the Data
  API's `prohibitMode` "script"). Commit Records' and Open Transaction's
  "Skip data entry validation" skip the fields validated only during data
  entry; fields validated Always are always checked
  (`commit-records-requests.html`); "Skip auto-enter options" skips
  auto-enter but not stored calculations. Duplicate Record copies the
  normal fields' values; creation values and calculations are computed for
  the copy. Its container values are copied as their library references
  and, when the copy is committed, each takes a reference to its member
  (+1 in the same saved commit, as FileMaker Pro's op87 does, containers.md
  5.1); before, the copy shared a member counted once, so a client
  releasing either record's value deleted the other's data (found in the
  review's re-check). A record a script deletes (Delete Record, Delete All
  Records, Truncate Table, also inside a transaction, at Commit
  Transaction) releases each of its container values in the same saved
  commit, as FileMaker Pro's op87 -1 does for a client's delete
  (containers.md 7.3, "Implementation notes"): the member is deleted with
  its last reference; a save that fails puts the counts, the deleted
  members and the library header back (2026-10-29; before, the members
  stayed as orphans). A
  table with a rule the planner cannot evaluate (lookups other than the
  exact-match form, previous-record entry, repeating calculations, calculations over
  related, global, summary, container or unstored fields, plug-ins, custom
  functions that read fields, `Get` selectors that need a window) refuses
  New Record and every change with 3 before a record is reserved or
  locked; a calculation that fails when evaluated refuses the commit with
  3. Set Field into a container (its data lives in the table's library) is
  3. Deleting needs no rules.
- Validation, as FileMaker Server's script engine answered (script oracle
  s17, 13.20; Claris: "In a repeating field, FileMaker Pro validates each
  repetition individually"). Not empty holds when any repetition has a
  value (repetition 2 alone passes; all empty is 509). A validation
  calculation checks each repetition the write changed, also one it
  cleared (before, an empty value was never checked unless `0x2`), and
  with "Validate only if field has been modified" off (`0x2`, before read
  as "check empty values") also repetition 1 and every repetition holding
  a value, changed or not; it runs in the repetition it checks (`Self` and
  the field named in its own formula are repetition n's, `Get (
  CalculationRepetitionNumber )` is n, a non-repeating field reads its
  value however the formula names it, bare, as `F[1]`, through
  `GetRepetition` or `Extend` (13.22; its repetition 2 fails the
  calculation), where a repeating calculation field would read it as
  empty; before, `F[1]`, `GetRepetition` and `Extend` read empty there); a
  calculation that fails (a division by zero) accepts nothing: 507 (before,
  3). A unique value may be in no repetition of another record (rep 2 =
  another record's rep 1, rep 1 or rep 3 = another's rep 2: 504), while
  one record may hold it twice. Maximum characters is Set Field's own
  check (511 from the step, the field keeps its value, the commit has
  nothing to refuse), in every repetition; the commit checks the changed
  repetitions again (auto-entered values). Validating Always only means a
  write's "Skip data entry validation" does not apply: a value the write
  did not change is not checked (before, Always checked every field of the
  record). Overridable rules (strict bit `0x4` clear) and rules for data
  entry only refuse a script's commit too (error capture on). Before, only a
  field's first repetition was validated (review fix); the desktop client's
  writes use the same planner, though its user and script edits reach
  repetition 1 only.
- Strict data types, ranges, value lists and existing values (2026-10-29,
  script oracle s18, 13.21; before, a table with one was read-only to
  scripts, 3), in the shared planner (`om_data::rules`): the rules checked
  as a value is entered (maximum characters 511, numeric only 502, 4-digit
  year 500, time of day 501, in range 503, member of value list 506) are
  `RecordPlan::check_entry`, Set Field's own errors for a field validated
  Always (for a script; a user's entry checks them during data entry
  too), on the value as Set Field gives it (`1/2/26` is not a 4-digit year
  even once stored as 1/2/2026); a value list of a field's values is
  checked by the host (`RecordPlan::value_list_field`; the server looks
  the text's English index key up in the field's value index, else in its
  stored values, as committed). The commit checks a changed value against
  them again (an auto-entered one). Existing values are the commit's 505,
  checked by the host like unique values (`existing_fields_for`; another
  record must hold the value in some repetition, the session's own changes
  counted), for fields validated during data entry too, unless the write
  skips data entry validation. Settled where the runs left it open:
  numeric only accepts the pure number forms of calc-values.md 2.8 (`1.5`,
  `1e3`, `(12)`, `1,000`; not `12abc`, `$5`); the 4-digit year is the
  number in the date order's year place, four digits or more; a time of
  day is 0:00 to before 24:00; a range compares the value converted to
  the field's type (a value that does not convert is out of range; an
  empty bound is open), text by the calculation comparison; a custom
  list's dividers (`-`) are not values; list values compare as English
  index keys for text, by value for other types (unconfirmed beyond the
  runs). Existing values need a text field with a value index in English
  or Unicode raw (as unique values), value lists one in English, and a list of a custom set or of one
  field's values. A related-values list is computed by the desktop for
  the written record and checked at the commit (record-writes.md); another
  file's, a calculated or a two-field list is not checked yet: the table
  plans, and a write whose
  rules check the field's changed value against it is refused (3), while
  one that does not check it (a script's, for a field validated only
  during data entry) is written (#1035). The range bounds and the
  list reference are read from `[T, 3, 5, F, 21]`, `22` and `07`
  (13.21), the list from the calculation schema's value lists or the file.
  `tests`: `om-data` `rules` (every rule), `om-server`
  `scripts::oracle_tests` (all 291 runs of the 19 s18 validation files
  replayed on a server session: Set Field's and the commit's errors and
  whether a value was kept, as FileMaker Server answered), `om-provider`
  `finds_and_transactions` (existing values and a field's value list on
  the desktop's writes).
- Schema changes while a script runs: each session reads the schema
  (record rules, the calculation and SQL schema, summaries, layouts,
  scripts) again before a step when the store's schema generation moved:
  Manage Database replaced catalogs (op13, op86) or op73 did its field
  work (`Store::schema_generation`, `ScriptHost::before_step`), and at the
  end of the script before its last commit. The window stays on its layout
  (by id); when that layout is gone or no longer shows the window's table,
  it moves to the first layout of its table (keeping its found set), else
  to the file's first layout with that table's records (OpenMaker's
  choice; before, it kept an index into the new list); the session's open record and its transaction's records are
  rebased on what the file stores now, keeping the cells the script
  changed, so op73's recomputed stored values and dropped fields are not
  written back. Before, a script paused across a save stored `Doubled =
  8` for `Count = 4` with the old `Count * 2` over the server's
  recomputation. A save cannot change a table under a record the session
  holds: Manage Database's `[T, 5]` lock conflicts with record locks
  (schema-path.md 4), so the rules a commit uses are those of its step.
- Values: stored text is read per field type in the file's formats: the
  locale record `[2, 0d]` (file-metadata.md 2.1) decoded into
  `om_calc::Locale` (separators, date order, leading zeros, 12-hour clock,
  AM/PM, day and month names, language), as FileMaker Server uses it
  (2.2); a file without a record this server reads (or with a month-year-day
  style order om-calc has no form for) is read as en-US with the date order
  its date indexes show, and a warning is logged. The values record rules
  give `Get ( CurrentDate )`, auto-entered dates and the like are still
  en-US text (`om_data::rules` reads them so), a known limit for files in
  other formats. Number text that is not the number's own form is kept
  (`NumberText`). Repetition n of field F (2 to 255) is the record's cell
  `F * 256 + n` (execute-sql.md, observed in the sample files): read by
  calculations and written by Set Field, the rules marking F changed;
  repetitions past 255 are 3 (their storage is not established) and a
  repetition beyond the field's maximum is 111. A field's value index holds
  the keys of every repetition (inferred: a find matches any repetition; an
  all-empty field has the empty key). Global fields hold per-session
  values, starting from the file's stored values `[T, 08, F]` (2026-10-29;
  before, empty), as a FileMaker Server session does; no other session
  sees them. Set Field into a calculation or summary field is 201; into a
  field of another occurrence (a related record) 3.
- Containers (2026-10-29; containers.md "Implementation notes"): a
  calculation reads a container field as the data of the library member
  its stored reference names: the member's file name (`FNAM`) as the text
  `GetAsText` shows, every other stream as the data (the main one first),
  loaded when a function reads data. So `GetHeight`, `GetWidth`,
  `GetThumbnail`, `GetContainerAttribute`, `Base64Encode`, `CryptDigest`,
  `Length` and `GetAsNumber` (0) see the data; `VerifyContainer` is `?`
  for embedded data (script oracle s34). Before, the value was the
  reference text `01` + U+E000 without data. A reference whose member is
  gone stays that text without data. ExecuteSQL reads a container column
  as the file name alone, without the data (execute-sql.md 8.1). Set
  Field into a container stores data in the library with the commit
  (containers.md, "Implementation notes (OpenMaker Server scripts)").
- Reads without a current record (2026-10-29, script oracle s18-find,
  13.21): in Find mode and with an empty found set a step's calculations
  are evaluated on a record no record has (`NO_RECORD`): a global field
  reads its value, a field of the window's occurrence in Find mode the
  current request's criterion as text, otherwise its empty value; nothing
  is related to it. Before, every field reference ended with 1224 (`?`)
  there, as a context without any record does (calc-values.md 13).
- Unstored calculations (calc-values.md 8.8): the host hands the evaluator
  the formula of every unstored or global calculation field from the
  file's calculation schema, so reading one evaluates it on the record it
  is read from, in its formula's table occurrence and the repetition read
  (empty beyond the field's), converted to its result type, through the
  same host: the open record's and the transaction's edits, related
  records, custom functions and nested unstored calculations included.
  Reads outside a step's own evaluation (sort keys, summarized values,
  steps that read a field, relationship match fields) evaluate it with
  `$` and `$$` variables of their own, empty (the runtime's are out of
  reach there; inferred to matter only for calculations that read
  variables). An unstored calculation whose referenced fields all read
  empty reads empty unless set to evaluate always (`CalcHost::
  evaluate_always`, option `0x20`; before, server scripts evaluated it
  anyway, and the runtime's step calculations dropped the host's answer
  on the desktop too, 2026-10-29), so `=` and `#` finds match FileMaker's
  (13.19). Sort Records sorts by them. A match field on the side a
  relationship is travelled from may be one, for every operator; on the
  other side it never matches (FileMaker calls such a relationship
  invalid). Nested reads deeper than 32 give `?` (only a circular, broken
  file gets there). Without the privileges to read records they are 9, as
  stored fields are.
- Summary fields (file-metadata.md 5, shared with the desktop client as
  `om_data::summary`): the 30-byte definition `[T, 3, 5, F, 09]` (every
  operation and flag; the additional field's occurrence key read like the
  others), computed over the window's found set in its order: total,
  average, count, minimum, maximum, standard deviation and list are the
  aggregate functions over the summarized field's non-empty values; the
  weighted average Σ value × weight / Σ weight (an empty weight is 0); a
  running total or count accumulates to the record and, with "restart",
  starts over at each group of the additional field when the found set is
  sorted by it; a fraction is the record's value over the total, or over
  its group's total when subtotaled and sorted by the break field (empty
  otherwise), empty for an empty value except 0 on the first record of a
  sorted found set (5.3); repetitions summarized together read every
  repetition into repetition 1, individually one each (Claris, inferred).
  `GetSummary ( s ; b )`: the whole found set when `b` is `s`; the
  record's group when the window's sort includes `b` (same occurrence, or
  both of the window's table), where a group is a run of records equal in
  every sort field up to `b`'s (inferred for a break field that is not the
  first sort field); empty otherwise; of a running summary its value at
  the end of the group, of a fraction the group's total over the total
  (inferred). The summarized field may be an unstored calculation. A
  summary of another table than the window's (reached through a
  relationship, where FileMaker summarizes the related records, or through
  a self-join occurrence of the window's table, treated as the window's)
  is 1225: the host is not told which record it is read from. Each read
  computes over the whole found set (no cache; a loop that reads a summary
  on every record is quadratic); an export's group rows get every group's
  value in one pass (`Summary::group_values`, also on the desktop). ExecuteSQL still answers summary fields
  with the desktop client's total and count (`FileCalc`).
- Related records (file-metadata.md 4.3, calc-values.md 8.7): `Rel::Field`,
  aggregates, `GetNthRecord`, `List` and `Count` over a related set read
  through the relationship graph `om_data::host` decodes (predicates left
  op right, ANDed, all operators; an empty match value never matches), on
  the records as the session sees them, its open record and transaction
  included. A hop whose predicates are all `=` (or `×`) is answered from
  each right field's value index (built on demand and then stored, as for
  a client's join; a multi-line value matches each line); other operators,
  and a table the session's transaction or open record changed, are
  matched by scanning every record of the right table. The related set is
  in creation order, or in the relationship's sort order when the bit of
  the direction travelled is set (`0x40` from left, `0x80` from right; the
  sort specs follow the occurrence keys in `[3, fb, 5, R, 02]`); over a path
  the last relationship's order counts (Claris, inferred). Custom functions
  come from the file's catalog. Go to Related Record shows the records
  related to the current record (or, with "match all records in the current
  found set", to each record of the found set, in first-seen order) on the
  stored layout, else on the first layout of the related table; another
  table's layout is 106, no related record 101 (the window stays), an
  occurrence without a path 103. "Show only related records" makes them
  the found set; without it the found set is all records with the first
  related one current. "Show in new window" uses the session's one window.
- Finds: Enter Find Mode (with or without stored requests), Set Field into
  requests, New/Duplicate/Delete Record and Omit in Find mode, Perform Find,
  Constrain and Extend with their stored requests (script-steps.md 11.2,
  variables expanded by the runtime) or the window's, Modify Last Find.
  Criteria are read with the desktop client's find operators
  (`om_query::criteria`, listed under "Found sets, finds and sorts" in the
  desktop notes below) and become SimpleFind finders run by `om-query`
  (`om_query::find::plan`); where the finders only select candidates
  (wildcards, phrases, `?`, `~`, partial dates) the candidates' stored
  values are tested with the criterion (2026-10-29); requests
  combine as 11.2.4 executed them (after a find without criteria, 400, the
  window is back in Browse mode with its found set and the record that was
  current before Find mode, and `Get ( RequestCount )` outside Find mode is
  the last find's request count, 13.21): the first request's matches within the
  master list (the found set for Constrain, all records otherwise), or the
  master list without them for an omit request, then each find request
  adds its matches within the master list and each omit request removes
  its matches from what was found so far; Extend adds the result to the
  found set. (Before, an omit request first found nothing.) No criteria:
  400; no match: 401 with an empty found set; a stored criterion on a field
  of a related table: 3; criteria on global fields are left out (11.2.1).
  The value index keys numbers FileMaker's way for whole numbers from 0
  (query-program.md 7.3) and, for the forms 7.3 leaves open, with
  OpenMaker Server's own order-keeping keys (`store.rs` `number_key`: a
  fraction appends its digits, `2.5` = `80 01 36`; a negative number is
  `7f`, the complemented count and digits, then `ff`), so `=`, `*` and
  ranges on number fields see them; FileMaker clients' keys for those
  values may differ (unknown).
- Sorts (script-steps.md 11.1): Sort Records with its stored order (without
  one there is no dialog on the server: 3), Sort Records by Field
  ascending or descending (by value list: 3), Unsort Records (creation
  order). As the script engine was seen to sort (11.1.3): empty values first
  ascending and last descending, last in both with "blanks last"; text by
  the collation of the entry's language override, else of the field's
  index language (`om_calc` collations; a language without a tailoring
  sorts as English); numbers, dates and times by value; a custom value
  list's order (dividers ignored) with values not in the list after them
  and empty values last; ties to the next field, then to the record id. A
  sort field of another table's occurrence sorts by its first related
  record's value (Claris, inferred). Value lists from fields and the
  summary reorder are 3. The found set stays sorted (`Get ( SortState )`
  1) until a step replaces it (Show All, Show Omitted Only, a find, Go to
  List of Records, Go to Related Record).
- Steps with a host method: Go to Layout, Go to Record/Request, Go to
  Field, Go to Related Record, Go to List of Records (same window), Enter
  Browse/Find Mode, New, Duplicate, Delete, Delete All, Open, Revert and
  Commit Records, Truncate Table (the window's table),
  Open/Commit/Revert Transaction, Set Field, Set Field By Name, the Insert
  steps with a target field, Set Next Serial Value, Show All Records, Show
  Omitted Only, Omit Record, Omit Multiple Records, Perform Find,
  Constrain, Extend, Modify Last Find, Sort Records, Sort Records by Field,
  Unsort Records, Pause/Resume Script (waits the duration; 0.1 s when
  indefinite), Refresh Window (nothing to do), and the data file steps
  (188-199) beneath the Documents and temporary folders
  (script-steps.md, Paths on OpenMaker Server). Every other step is 3.
- Get selectors the host answers: ApplicationVersion (`Server ` and the
  OpenMaker version), CurrentDate/Time/Timestamp and CurrentHostTimestamp
  (the server's local time), CurrentTimeUTCMilliseconds/Microseconds (from
  1/1/0001), FileName, FilePath (`file:` and the absolute path), FileSize,
  HostName (the server name), PersistentID (the server id), AccountName and
  UserName (the account), ApplicationLanguage and SystemLanguage
  (`English`), SystemPlatform (1 macOS, -2 Windows, 8 Linux), Device (1, 2,
  6), ApplicationArchitecture, UUID, LayoutCount/Name/Number/TableName,
  WindowMode, FoundCount, TotalRecordCount, RecordNumber, RecordID,
  RecordModificationCount, RecordOpenCount, RecordOpenState, ModifiedFields,
  SortState (1 after Sort Records, else 0; semi-sorted is not tracked),
  RequestCount, DocumentsPath, TemporaryPath and OpenDataFileInfo (the
  session's open data files); the runtime answers the script selectors.
  Every other selector is empty text (WindowName included: no window title
  on the server).
- Step errors go to the server's log (file, script, step, line, code).

Captures that would confirm the inferred wire details: FileMaker Pro 26
against FileMaker Server 26 through the relay, running from one script (a)
Perform Script on Server, Wait for completion on, on a script whose id is
0x80 or more, with a parameter that has a styled word; (b) the same with
Wait off; (c) Perform Script on Server with Callback on a script that
exits with non-ASCII text, then with one that fails with an error that
carries detail. (a) and (b) settle the parameter, key and flag encodings
and the zero callback ids, (c) the ids' order and values, how op105 reaches
FileMaker Pro (call, LocateRequest, request id, header) and what it
answers, and the extended error text. A recording of FileMaker Pro against
OpenMaker Server (`record_dir`) running (c) shows whether FileMaker Pro
accepts OpenMaker's op105.

## Implementation notes (schedules)

OpenMaker Server's FileMaker script schedules (2026-10-29): `om-server`
`schedules` (`calendar.rs` when they run, `file.rs` the schedules file,
`clock.rs` the clock, `mod.rs` the scheduler and its runs), with
`Server::with_schedules` and `with_clock`, `ServerHandle::schedules`,
`run_schedule`, `enable_schedule` and `wait_for_schedules`, and
`om-server --schedules <file>`. Sources: the script oracle (13.23, marked
(run)) and the Claris FileMaker Server Help (marked (doc): "Running
FileMaker scripts", "Specifying schedule details", "Examples of repeating
schedules", "Scheduling administrative tasks", "Event log", the event log
messages of the FileMaker Server 13 to 16 help, and FileMaker Pro's
"About running scripts on FileMaker Server"). Everything else is marked
OpenMaker's choice; U15 lists the runs that would settle it.

Definition
- A schedule has FileMaker's options (doc): its file, its script by name,
  the account it runs as, an optional parameter, a time limit and "End
  the schedule if the timeout is reached" (`end_on_time_limit`), when it
  runs, and whether it is enabled. FileMaker's other kinds (system
  scripts, script sequences, backups, verification, messages) are not
  implemented.
- When (doc): once at a date and time; or daily, every n days (counted
  from the start date) or on days of the week, each day at a time or
  every n minutes or hours from a start time to an end time; from a start
  date to an end date, which runs (the daily example's last run is on the
  end date; the weekly example's "12/30/2021" is not a Friday, so taken
  as a slip). OpenMaker's choices: the end time runs when the interval
  reaches it; the interval is whole minutes, at most 24 hours; every n
  days allows 1 to 366 and needs a start date; once has no end date.
- Times are the server's local ones. Missing local times shift forward
  by the daylight saving gap's duration. Repeated local times run once,
  at their first instant (OpenMaker's choice).
- A new schedule is enabled unless it says otherwise (doc, current help:
  "A new schedule is enabled by default if its next run time is valid";
  the FileMaker Server 16 help said the opposite). "End the schedule if
  the timeout is reached" is off unless set (its default is not
  documented).
- The schedules file is OpenMaker's format: one `[name]` section per
  schedule with `file`, `script`, `account`, `parameter`, `repeat` (`once`,
  `daily`, `every <n> days`, `weekly <days>`), `at` or `every = <n>
  minutes|hours from <time> to <time>`, `start`, `end`, `time limit`,
  `end on time limit`, `enabled`; quoted values keep spaces and read
  `\r`. Names are unique without case. Schedules are checked when the
  server starts; a file or account the server does not know then is
  logged, and its runs fail as below.
- Not implemented: email notifications (FileMaker Server 16 sent one at
  every completion of a schedule that has addresses; the current help
  has only server-wide notifications of errors and warnings; OpenMaker
  Server sends no mail), the server-wide "Allow enabled schedules to run"
  switch, group administrators.

When runs start
- A run starts when its time comes and the schedule is enabled. A start
  the server was down for, or that came while the schedule's last run
  still executed, is skipped, not made up (doc: "If that delay pushes it
  past its next run time, that second run time is skipped"; FileMaker 16
  also suspends schedules during sleep until their next time); a skipped
  start is logged and counted. So one schedule never runs twice at once,
  and "run now" of a schedule that runs is refused
  (`ScheduleError::Running`, OpenMaker's choice).
- Runs of different schedules execute at the same time (doc: "Multiple
  tasks can run at the same time from this queue"), each on a thread of
  its own, under a script slot of the server's script engine: they count
  against `ScriptLimits::concurrent` with Perform Script on Server, and a
  run that finds it full does not start (812; OpenMaker's choice that
  both share the limit, as both are script engine sessions).
- "Run now" (`ServerHandle::run_schedule`) runs a disabled schedule too
  and leaves the next run as it was (OpenMaker's choice; doc: "Run the
  selected schedule immediately").
- The scheduler follows a `Clock`: the server's local clock (it looks
  again at least every minute, so a change of the system time is seen),
  or a `ManualClock` that moves only when a test moves it.

A run
- As fmsased runs one (run 13.23): the file by its hosted name without
  case, else 802 and no session. OpenMaker Server cannot close or pause a
  hosted file, so a file that is not hosted is the closed file's case;
  closing a file while a run executes (`ServerHandle::abort_file_scripts`)
  aborts it as every script of the file (error 1, its transaction and
  open record reverted). What FileMaker does for a paused file is not
  documented, and OpenMaker Server has no pause.
- The login: the account must be one of the file's that a client's Login
  could sign in as (local-files.md 2.2, 2.3: a stored password OpenMaker
  can check, active, a FileMaker-file account, no password change due, a
  set OpenMaker Server enforces); else 212
  and no session (run: wrong password, unknown account, disabled
  account). A schedule names the account without a password (OpenMaker's
  choice: the schedules file comes from the server's administrator, who
  holds the files; FileMaker stores a password in each schedule, so a
  password changed in the file stops its schedules, and here it does
  not).
- The session is a Perform Script on Server session (above): the same
  window, record rules, privileges (the account's in the file),
  limits, abort and shutdown, and the step errors in the event log.
  `Get ( AccountName )` and `Get ( UserName )` are the account as the
  schedule writes it (run: `admin` for `Admin`). The script is found by
  name when the run starts (run: without case, not trimmed, the first of
  two names that differ in case; 104 otherwise); the parameter is its
  text, empty when there is none (run).
- A time limit with "End the schedule if the timeout is reached" ends the
  run when it passes, as an administrator's abort does (error 1, reverts,
  locks released), logged as an error (doc, FileMaker Server 13: "the
  script or script sequence is aborted and FileMaker Server logs an Error
  message"); without it the run goes on and a warning is logged when the
  limit passes (doc: "logs a Warning message and the script scheduled
  task continues to run"). The server's own limits (`--script-steps`,
  `--script-seconds`) end every run, as every script. A shutdown ends a
  run whatever the option (FileMaker ends it only with the option, doc;
  OpenMaker Server cannot run on once it stops).
- Shutdown stops the scheduler first: no run starts from then on, "run
  now" answers `ScheduleError::ShuttingDown`, and a run whose slot is
  refused by a shutdown that began is recorded with 820. The runs that
  execute are aborted and waited for with every script (the existing
  path).

What is kept and logged
- Each schedule keeps its last run: when it started and ended (by the
  scheduler's clock), whether it was run now, whether the script ran, its
  error (the run's final error as Perform Script on Server returns it, or
  802, 212, 812, 820 when it did not run; -1 when the server failed
  running it), its result, why it was aborted, and whether it passed its
  time limit; `ServerHandle::schedules` lists them with the next run, the
  running run's start, and the runs and skipped starts. A run "succeeded"
  when its script ran to its end with error 0 (OpenMaker's reading of the
  Admin Console's Succeeded and Failed).
- Event log (FileMaker's text where the Claris help gives it; names in
  double quotes, levels for FileMaker's Information, Warning and Error;
  message ids are not documented): `Schedule "N" has started FileMaker
  script "S".` when the session starts (doc); `Schedule "N" has exceeded
  its time limit.` (warning, doc); `Schedule "N" aborted; time limit
  exceeded.` (error, doc); every step error as 4.9; then one line of
  OpenMaker's own wording in FileMaker's `Schedule "N" aborted; ...`
  pattern for a run that did not start (802, 212, 812, 820) or was
  aborted otherwise, or `Schedule "N" completed (error E).` (information
  for 0, else warning). A skipped start and each schedule's next run at
  startup are logged too.

Tests: `om-server` `schedules::calendar` (Claris's three repeating
examples, intervals, once, daylight saving time), `schedules::file` (the
schedules file), `tests/schedules.rs` (an in-process server with a
`ManualClock`: runs on time and missed starts, the run's session as
13.23, runs that cannot start, skipped starts, both time limits, a file
closed during a run, disabled schedules, shutdown, checks).

## Implementation notes (desktop client)

The desktop client's script host (2026-10-29): `om-gui` `scripting`
(the worker side: `session`, `host`, `window`, `url`, `files`) and `app/scripts.rs`
(the window side), with found sets in `om-data` (`found`), `om-provider`
(`found.rs`) and the finder builder shared with OpenMaker Server
(`om_query::find`). Stored sort orders and find requests follow
script-steps.md section 11 (commit 08a0b61 of the earlier, private history), decoded by
`om_scripts::specs`. Points marked (unconfirmed) need a FileMaker Pro run.

Execution (2.1, 6)

- The runtime runs on the provider worker thread and calls the provider
  directly; host calls block. `run` returns every 50 ms; between steps
  (`abort_requested`) and while a step waits for the user, the worker
  serves the UI's other requests (layouts, record pages, writes), so the
  window is drawn while a script runs. A file opened or closed during a
  script clears the runtime first. Scripts the UI starts while a step
  waits run when it returns; timers and callbacks the window starts while a
  script runs wait for it.
- The window does not take input while a script runs (not paused), except
  Esc and Cmd-period, which set an abort flag the runtime reads before each
  step: it halts with error 1 when Allow User Abort is On and is ignored
  otherwise.
- Exit Application, Close File and Close Window on the file's only window
  halt the scripts; with Allow User Abort Off the script may go on until
  the end of its 50 ms slice.
- The worker keeps the window the script works on (layout, mode, view,
  found-set position, current record and its unsaved values, Find mode
  requests, title, toolbars, zoom). A script starts from the window as the
  user left it and re-reads the current record; after every step that
  changed something (not while Freeze Window is in force) the window
  follows it. When the last script ends, the file's `$$` variables go to
  the window for layout calculations.
- Go to Layout keeps the current record when the new layout shows the same
  table, and returns to the record a table showed last otherwise.
  Go to Layout [original layout] is the layout when the top-level script
  started, not per frame (unconfirmed for sub-scripts).

Errors and dialogs (4.4, U1)

- Error alerts: the message is `script: step line (step name): error
  text`; a confirm code (4.4's list) has Cancel (only while Allow User
  Abort is On) and Continue, others OK, which continues. Esc cancels when
  Cancel is offered. Go to Record past the last record (101) is silent.
  FileMaker Pro's alert for a step the user cancelled (error 1) is "<step
  name> has been canceled.  Do you wish to continue with this script?"
  with Cancel and Continue (obs, below); its other alert texts are not
  known (U1).
- Show Custom Dialog: inputs start with their field's or variable's value
  (script-steps.md 11.8 storage). A cleared spec, as FileMaker Pro stores
  each unused input, is no input and nothing is written to it [I: Pro shows
  such dialogs with their message and buttons only, #696's and #707's
  readings; #940]. The default button is drawn last, Enter
  chooses button 1, Esc button 2 when there is one; only a committing
  button stores the inputs. `Get ( LastMessageChoice )` numbers the stored
  buttons 1-3.
- Pause/Resume: indefinite or timed. While paused the user works in the
  window (records, edits, buttons; triggers fire); Continue resumes from
  the window as the user left it, Cancel (or Esc) halts with Allow User
  Abort On and resumes otherwise. A script started during the pause runs
  on top of the paused one.
- "With dialog" confirmations: Delete Record ("Permanently delete this
  ENTIRE record?") and Delete Portal Row ("Permanently delete this one
  related record?"), as FileMaker Pro's alerts word them (client-ui.md,
  "Delete confirmations"; shown without a title), Delete All Records, Revert Record, Replace Field
  Contents, Truncate Table; declining is error 1. Sort Records with a
  dialog shows the Sort dialog prefilled with the stored order.
- Saving a changed record asks only on a layout whose Layout Setup option
  "Save record changes automatically" is cleared (layout flag `0x10`,
  layout-model.md 4.2). On a layout that saves automatically, Commit
  Records/Requests commits without a dialog whether "With dialog" is On
  or Off ((doc) `commit-records-requests.html`: "Setting With dialog to
  Off only applies when Save record changes automatically is cleared").
  On a layout with the option cleared (obs, FileMaker Pro 26.0.3,
  2026-11-11: a Contacts file generated with the flag and an
  OnRecordCommit trigger on Contact Details, every answer tried):
  - The alert "Save changes to this record?" offers Save, Don't Save and
    Cancel. It comes for Commit Records/Requests with dialog On (also
    under Set Error Capture On), for a script step that leaves the record
    (Go to Record/Request/Page [ Next ]), and for the user leaving it
    (Records > Go to Record > Next). Commit Records/Requests with dialog
    Off commits without asking.
  - Save: OnRecordCommit runs once the user has chosen Save (in all three
    runs its script ran about 0.8 s after the click was sent, the record
    still open), then the record is committed and the step goes on;
    error 0.
  - Don't Save: the record is reverted, the step goes on (Go to Record
    moves) and OnRecordCommit does not run; error 0.
  - Cancel: the record stays open with its changes, the step does nothing
    (Go to Record stays) and OnRecordCommit does not run; error 1. With
    error capture Off a script then gets FileMaker's alert "Commit
    Records/Requests has been canceled.  Do you wish to continue with this
    script?" (Go to Record/Request/Page names itself the same way) with
    Cancel and Continue; Continue goes on with error 1. Cancelling the
    user's own navigation just stays on the record.

Triggers (7)

- Storage, observed in the shipped templates and test project (every
  record seen is OnLayoutEnter; layout-model.md lists it as unknown): the
  layout's `KScriptTriggers` node is `4/5/<layout>/30`, an object's
  `4/5/<layout>/5/<object>/30`; `04` holds the count (u16) and
  `05/<event id>/01` one 16-byte record: u16 event id, the script key as an
  `HBKey<5>` (6 bytes), u16 enabled modes (1 Browse, 2 Find, 4 Preview),
  and an `HBKey<5>` file reference (`00` = this file). test project layout
  2 "Contact List": `00 65 02 01 03 00 00 00 00 01 00 00 00 00 00 00`,
  OnLayoutEnter running script key 3 in Browse mode. The parameter
  calculation is read from a formula node beside `01` (children 4 and 6)
  when one is stored (inferred, never observed); a 4-byte flags word at
  bytes 6-9 would read the same modes for these records (unconfirmed). The
  script key is read as the raw big-endian key, like `Script::id`.
  **Corrected (file-metadata.md 7.2, from `ScriptTrigger::Read` and 421
  records):** after the u16 event id come two length-prefixed keys (script,
  then file reference), then a u32 mode mask, a u32 option and a u16 count
  of extra bytes; the 16-byte record is `0065 | 020103 | 00 | 00000001 |
  00000000 | 0000`, and longer script keys lengthen it. The script key is a
  serial id. The parameter formula is at `05/<event>/31` (children 04, 05,
  06). Table-view objects have caches at `4/5/<layout>/0d/05/<object>/30`.
- Events: the window fires OnLayoutEnter (also for the first layout when
  the file opens) and OnRecordLoad, OnLayoutExit, OnRecordCommit,
  OnRecordRevert, OnModeExit, OnModeEnter, OnViewChange, OnObjectEnter,
  OnObjectExit, OnObjectModify, OnObjectValidate and OnObjectSave (leaving
  a modified field), OnObjectKeystroke and OnLayoutKeystroke (see the
  keystroke note below); steps fire the
  same layout and object events for what they do (Go to Layout, Go to
  Record, New/Duplicate/Delete Record, Commit/Revert, mode steps, Perform
  Find, Sort, View As, Go to Field/Next Field/Object, Go to Related Record).
  OnTimer comes from Install OnTimer Script (the window keeps the clock),
  OnPSOSCallback from polling the host for callback results. The file's
  window and file triggers come from its document preferences `[2, 18]`
  (file-metadata.md section 3; `app/scripts.rs`): OnFirstWindowOpen and
  OnWindowOpen when the file's window has opened, before its first
  layout's OnLayoutEnter, and OnWindowClose then OnLastWindowClose before
  the user closes the file. OnFileAVPlayerChange is not fired.
- Keystroke triggers (2026-11-02, `app/scripts/keystrokes.rs`; (doc)
  `onobjectkeystroke.html`, `onlayoutkeystroke.html`, `code.html`,
  `get-triggermodifierkeys.html`, `get-activemodifierkeys.html`): in
  Browse mode, typed characters and the Code function's navigational keys
  (Backspace 8, Tab and Shift-Tab 9, Return 13, Escape 27, left 28, up 29,
  right 30, down 31, forward delete 127) fire the active field object's
  OnObjectKeystroke, then the layout's OnLayoutKeystroke (also with no
  active field); False from either stops the key and skips the layout
  trigger. `Get ( TriggerModifierKeys )` is the key's own modifiers (Shift
  1, Control 4, Option/Alt 8, Command 16; Caps Lock is not reported).
  Keyboard shortcuts do not fire them (Command on macOS; Ctrl or Alt on
  Windows, and on Linux), nor do FileMaker's Ctrl-Up/Down. An egui input
  plugin takes the keys out of the window's raw input before egui's focus
  handling. The keys the triggers let through are given back unchanged at
  the start of the window's next pass, so the editor applies them as any
  key. Keys go one at a time per window, and keys typed meanwhile wait in
  order. A key is applied only in the window instance, file, mode, layout,
  record and editor it was typed into. Otherwise it is dropped, and so are
  keys typed before such a change. Queued keys retain their original field
  object and editor before any trigger dispatch. An approved Tab, Return or
  Escape can transfer its own pending typeahead after navigation settles; an
  intervening pointer focus change cancels that transfer. The keypad's
  Enter is 10 and Return 13: egui reports both as Enter, and the app's
  event loop marks the keypad's (`om_gui::keypad`). Limits: Find and Preview mode keys (the
  triggers' other modes), IME commits, Home/End/Page keys (no documented code; they reach the editor
  without a trigger), the no-active-object alert for typed characters, and
  native key timing and type-ahead ordering are not implemented or
  measured.
- Desktop external layout/object triggers (2026-11-02 implementation): the
  stored file reference is resolved through the event owner's ordered data
  source paths after the mode/trigger suppression checks. The parameter is
  evaluated in the originating window and file's global namespace before
  opening the script's file. The script then uses that file's catalog and
  window; its result allows or vetoes the originating held action. Recursion
  suppression is scoped to the event's window. This uses the same
  data-source support and limits as external Perform Script
  (desktop-files.md, Supported sources). Synthetic
  native-provider/GUI checks cover dispatch and caller continuation; native
  FileMaker focus and event ordering acceptance remains unmeasured.
- OnWindowTransaction (2026-10-29, run 13.26; `scripting/host/transaction.rs`)
  fires once for each commit that writes records: a script's Commit
  Records, leaving a record, Delete Record, Delete All Records (one
  firing), Commit Transaction (one firing for the transaction's records,
  each once: a record created and changed is New, created and deleted is
  left out), a record committed with its related records; and the user's
  own save, new record and delete. Not for a commit with nothing written
  (the user's save of only global fields, whose values are the
  session's, writes no record), a revert or a failed commit. The parameter is FileMaker's formatted JSON
  of 13.26. A script's commits queue the trigger's script, which runs when
  every running script has ended, one per commit in commit order. The
  provider used to download the file's locale `[2, 0d]` and document
  preferences `[2, 18]` by their own keys, which answer with an inline
  value the calculation schema never saw: file triggers and the file's
  formats were missing on hosted files; it now reads them with the file's
  node `[2]`.
- Pre-event triggers hold the action in stages, one trigger at a time, in
  FileMaker's order: OnObjectExit, then OnRecordCommit (or OnRecordRevert)
  when the record has unsaved changes, then OnLayoutExit or OnModeExit (the
  window's own mode change keeps unsaved values and has no commit stage).
  A step that leaves the field the user changed (Commit Records, Go to
  Record, Go to Layout, a mode step, Go to Field, Go to Portal Row, Go to
  Object) runs that field object's OnObjectValidate and OnObjectSave
  before its OnObjectExit, as the user's own exit does (#1030). Changed
  means the user's typing in that field: a field of the layout's table, or
  a portal row's field the user typed into its related record. A script's
  Set Field does not count, and neither does another field of the record.
  Revert Record runs OnObjectExit only. A record or related record that
  OnObjectSave's script writes joins the step's own commit. An edit is saved
  once: after its OnObjectSave stage has passed, through a step or the
  user's own exit, leaving the field again without another edit does not
  run it again (#1040). The stage passes when its trigger lets the action
  go on, or when no trigger runs; an OnObjectSave that refuses saved
  nothing, so the next exit runs it again. This is
  inferred from Claris ("Script triggers can be activated by user actions
  or by scripts"; OnObjectSave "Performs a script after object data has
  been validated and saved, but before the field is exited"), not measured
  on FileMaker Pro;
  the action happens when the last one lets it, then the delayed triggers
  queue. For the user's own actions the window holds the action under a
  token, with the id of the window it belongs to, until the worker reports
  the trigger's outcome; the action goes on (or a cancelled field exit
  gives the field the focus back) in that window when the job that ran
  the trigger ends, so the job's final window state cannot undo it, and
  goes with its window when that window closed meanwhile (2026-10-30). A
  navigation's OnRecordCommit approves the save it leads to; the action
  that waited for its save goes on without running its pre-event
  triggers again, after a refused save nothing is remembered (a retry
  runs them all), and the delayed triggers of the user's action and the
  save's OnWindowTransaction wait for a running script instead of being
  dropped; OnWindowTransaction starts after the action's own follow-up
  (OpenMaker's order; FileMaker's is not measured).
- A trigger script that causes its own event (Commit Records inside
  OnRecordCommit) does not fire that trigger again (unconfirmed).
- **Corrected (run 13.17):** a layout switch calls OnRecordLoad before
  OnLayoutEnter, so OnLayoutEnter's script runs first, and calls
  OnRecordLoad also with an empty found set. The runtime now stores a held
  action's outcome (`trigger_action`'s result) as the step's error when the
  trigger lets it happen, after the step's first 3000.
- The desktop host queues the delayed triggers of entering a layout (Go to
  Layout, Go to Related Record) in FileMaker's call order, OnRecordLoad then
  OnLayoutEnter; the runtime pushes each one's frame on top of the last, so
  OnLayoutEnter's script runs first, then OnRecordLoad's, then the caller's
  next step (2026-10-29; `script_engine.rs`
  `on_layout_enter_runs_before_on_record_load`). The window's own layout
  change starts the two scripts one after the other in the same order
  (OnLayoutEnter, then OnRecordLoad), so both paths agree.
- OnObjectEnter, OnObjectExit and the other object events identify the
  object by the field it shows. Go to Object finds the object by its name
  (`4/5/<layout>/5/<object>/10`, now decoded) as "Portals" below says.

Found sets, finds and sorts (script-steps.md 11.1, 11.2; query-program.md 6, 10)

- A provider keeps one found set per base table; `records` pages through
  it; new records join an explicit found set at its end. Perform Find,
  Show All and Show Omitted Only leave it unsorted; Omit keeps the sort.
- Requests combine as script-steps.md 11.2.4 says (in order; a first omit
  request starts from every record, or from the found set in Constrain;
  Extend adds to the found set). No criteria is 400; no match is 401 and
  leaves an empty found set in Browse mode, for a script and for the
  window's own Perform Find (FileMaker Pro's Modify Find choice is not
  offered; unconfirmed for scripts with error capture off).
- Criteria are FileMaker's find operators (`om_query::criteria`, from the
  Claris pages "Finding text and characters", "Finding numbers, dates,
  times, and timestamps", "Finding ranges of information", "Finding empty
  or non-empty fields" and "Finding duplicate values", whose examples are
  its tests): plain words (each starts a word of the field), `=word`,
  `==value`, `=`, `==`, `*`, `!` and `!value`, `?`, `~`, `@`, `#` and `*`
  in words, `\`, quoted phrases (`"..."` from a word's start, `*"..."`
  anywhere, ending at a word boundary unless `*` follows; case ignored,
  accents kept), `<`, `<=`/`≤`, `>`, `>=`/`≥`, `a...b`/`a..b`/`a…b`, `//`,
  dates with `*` or omitted parts and `{a..b}` sub-ranges (`5/12/*`,
  `6/2019`, `*/{10..15}/2019`), day names (`Tuesday`, `=Thu`, `Mon..Fri`),
  times with `AM`/`PM` (`3 PM`, `*:15`, `8 AM...8 PM`) and timestamps
  (`// 3 PM`, `=Mon 2020`). Settled here (unconfirmed): a wildcard pattern
  matches one whole word and never spans words (`#` finds 3 and not 30, as
  the examples need); `=` is an empty value or an empty line, `==` alone
  nothing at all, `*` alone a non-empty line; text ranges compare the
  field's words (Claris: the word index's order; 6.4's value scope was Med,
  and the `Process` tail gives text fields word scope); `?` on text finds
  control characters other than tab and line breaks, U+FFFD and
  noncharacters (which characters "display as blanks" is not known); a
  two-digit year falls in the 100 years from 70 years back; `5/12` and
  `2/*` are in the current year; English day names; `~` folds hiragana and
  katakana, voicing, small kana and the iteration marks, and with the
  English index's keys (primary weights) changes nothing the index does
  not already ignore; numbers compare by value (`.50` = `$.50`). Text a
  field's type cannot read (`someday` in a date field) is error 3
  (FileMaker's error for it is not known).
- On a hosted file each request is one op86 SimpleFind per table of
  ANDed finders (6.5), the instruction carrying its access key (2.4);
  `om_query::find::plan` gives each criterion finders of the established
  kinds (0, 2, 7): exactly its records for words, whole words and values,
  empty, not empty, duplicates, whole numbers, dates, whole days, months,
  years and hours, and ranges of keyable values; a superset where the
  operator needs Match or DateTimeMatch (kinds 8, 9, whose operands are not
  established): a pattern's literal prefix, a phrase's words, a date
  pattern's enclosing year, or every record. The client then reads the
  candidates (op92, 500 at a time) and keeps the ones `Criterion::matches`
  accepts on their stored values, as FileMaker Pro finds locally where the
  host cannot (architecture.md 7). `!ABC` is FindDuplicates ANDed with the
  finders of `ABC`. The requests and Constrain/Extend combine on the
  client. Duplicates compare whole value keys on the host; Claris says text
  compares its first 100 characters (the memory engine does). Number
  criteria are exact on the host only as far as it keys every stored
  number (FileMaker Server does; OpenMaker Server keys fractions and
  negative numbers with its own forms, see the PSOS notes). OpenMaker
  Server's script finds use the same plan and filter.
- Related-field criteria (`Occurrence::Field` in a request: a stored
  request naming another table's occurrence, or Set Field into a related
  field in Find mode): the criteria of one occurrence are solved in its
  table together, so one related record must match them all
  (`SolveOneTableOfPath`, 6.5), then joined back to the layout's table
  relationship by relationship along the calculation schema's path with
  JoinProjection (9.2): the matched records' keys of each predicate's
  field as source rows, computed on the client from their stored values
  (FileMaker Pro sends Projection for them, cap:912; computing them lets
  unindexed join fields work), the relationship's operator as the
  predicate type, empty values never relating. The result is ANDed with
  the request's other criteria. A name no occurrence has is 102.
- Indexes missing when a find runs [implementation]: a finder on a field
  without the index it reads finds nothing while FileMaker Server builds
  that index for later finds (query-program.md 7.1), and a field marked
  indexed can lack its index node (schema-path.md 6). So each SimpleFind
  first GetKeys the counts leaf of every index its finders read (`[T, 01,
  F, 10]` for the value index, `11` for the word index; 7.1, 8.5) in the
  same program, the find last (instructions run in order, 0). When one was
  missing, the client reads the stored values in batches and generates
  each record's index keys (`om_query::keys`: one key per line of every
  repetition, white space kept, `01` for empty English text, no key for
  text a number or date field cannot read). Stored dates and timestamps
  use the file locale's date order (file-metadata.md 2, data-format.md).
  Text uses the field's index-language key form (query-program.md 7.2):
  English, English international, System, Unicode standard and Unicode
  raw are implemented; language 0 generates no keys. Other languages
  fail explicitly with error 3 when fallback needs their keys.
  `om_query::find::ClientFind` decides the same serialized finders record
  by record and keeps only matching record ids, without materializing
  an index or a slot tree. Duplicates keep per-key counts and candidate
  record ids, charged against a 64 MiB state budget before retention;
  exceeding it fails the find with a clear message. This accounting
  budget includes conservative map/vector overhead; it is not an
  allocator limit. Batch data, one record's generated keys and result
  ids are additional memory. A finder that removes records is answered
  in the same pass. The client's filters for criteria the finders
  only narrow then run as on the host's answer. The join back of a
  related-field criterion checks the value index of each non-`×`
  predicate's field in its nearer table (a JoinProjection on an unindexed
  field relates nothing, live:jp_unindexed) and, without one, relates the
  keys that index would hold in batches of that table
  (`om_query::find::join_on_keys`), using its stored-date order and each
  target field's index language. A missing index costs one read of the
  table; later finds use the index the first had the host build. 401
  still means that no record matched. How FileMaker Pro itself finds on
  an unindexed field is open (query-program.md 13). OpenMaker Server still
  builds English-form text indexes regardless of field language; fixing
  server language selection is a separate follow-up. Find criterion
  keys and join-source text keys also retain their existing English
  collation; this fallback does not add localized criterion parsing.
- Imported-table layout finds (2026-11-02, synthetic provider/GUI tests):
  Find mode, Requests menu and Perform Find run stored scalar base-field
  criteria through the hydrated table's owning NativeProvider. The owner
  supplies native table/field metadata, query access keys, indexes, record
  bodies and stored-date order; the root layout/window keeps its imported
  table namespace and found set. Replace, Constrain, Extend and ordered
  include/omit requests reuse the existing finder and found-set algebra,
  including missing-index streaming, budgets and language guards. Base
  criteria may be unqualified or qualified by the layout's own occurrence.
  Stored text/number/date/time/timestamp fields and stored calculations are
  supported. Relationship criteria, evaluated calculations, repetitions and
  containers return 3 before any find query or found-set change; summary and
  global criteria retain the existing ignored-criterion behavior. An
  unsupported Perform Find retains the caller's mode, requests, current
  record and previous found set; 401 returns to Browse with an empty found
  set. Sort remains refused. Creation, deletion, import, explicit
  transactions and other unsupported peer writes remain guarded. Native
  FileMaker acceptance is unmeasured; this is functional synthetic coverage.
- Imported-table layout creation (2026-11-02, synthetic provider/GUI
  tests): New Record and Duplicate Record from the Records menu, toolbar
  and script steps (and a script's New Record with Set Field values) run
  the hydrated table's owning NativeProvider creation engine: fresh owning
  field definitions and plan, auto-entry (account, custom functions, its
  own `Get ( FileName )`), validation, op71 id, op70 commit and verified
  readback, with no fictitious source record. The record capability
  follows the owner's plan, so unsupported active-context `Get` selectors,
  including inside custom functions, keep creation refused before any
  request. Only the layout's own base table is created; the owner's id
  joins the caller window's found set at its end, sorted or not, and the
  owner's version is kept under the caller's imported table id, so
  colliding root ids are never written. Duplicate Record reuses the
  existing pipeline: the menu copies editable scalar values; a script
  duplicate of a record holding a container reference is refused (3)
  before any reference count changes, and non-scalar cells are refused
  before any lock, creation or commit. Explicit transactions, import,
  globals and repetitions remain refused. An unverifiable owning save is
  reported without joining the found set. Native FileMaker acceptance is
  unmeasured; this is functional synthetic coverage.
- Fields without an index (2026-10-29, run 13.19; `om_data::found`
  `findable`, `filter_evaluated`): a criterion on an unstored calculation
  plans no finder; after the request's other criteria (host finders and
  stored-value filters) the client computes the calculation on each
  candidate, as a calculation reads it (`om_data::host::read_field`: in its
  formula's occurrence, with the window's found set, empty when every field
  it references is unless it evaluates always), and keeps the records whose
  value's text the criterion accepts with the operators above; `!` reads
  every record of the table. This is FileMaker's finder option `Unstored`
  / `ClientOnly` solved locally (query-program.md 6.2; architecture.md 7).
  Related-field criteria on unstored calculations are computed in the
  related table the same way before the join back. A failed download fails
  the find. Criteria on summary and global fields are not criteria: the
  provider drops them, and a set of requests left without criteria is 400.
  A script's Set Field into a summary field in Find mode is 201 and stores
  nothing (13.19); into a global field it sets the session's value, in
  Browse and Find mode ("Globals" below; before, 201). A calculation field
  takes a criterion in Find mode.
- OpenMaker Server's script finds do the same (`om-server` `matches`,
  `store_value`, `stored_requests`): Set Field into a calculation field in
  Find mode stores a criterion (it was 201), into a summary field 201, into
  a global field sets the session's global; stored requests drop summary
  and global criteria; an unstored calculation's criterion is tested on
  each candidate's `read_field` value, empty when every field it
  references is (unless it evaluates always), so `=` and `#` find as
  FileMaker does; after a 400 the window is back in Browse mode, its found
  set and current record as they were (13.19, 13.21; both 2026-10-29).
- Tests: `om-query` `criteria` (the Claris pages' examples),
  `om-provider/tests/finds_and_transactions.rs` (about 70 records on an
  in-process OpenMaker Server: every operator above on text, number and
  date fields, each result compared with the client's own evaluation over
  all records, `om_data::found::find`, and with the expected records; and
  the unstored, summary and global criteria of the fixture's calculated
  fields), `om-server/tests/psos_records.rs` (operators in server scripts),
  `om-server/tests/psos_finds.rs` (unstored, summary and global criteria in
  server scripts), `om-gui/tests/client_finds.rs` (the same through the
  window on an in-process server) and the ignored hosted tests over test
  project (related-field criteria).
- Stored requests: `$name`/`$$name` in criteria expand when the request is
  restored (the runtime expands them before the host sees the node); a
  repetition criterion matches the field's value.
- Sorts are computed on the client from each found record's stored values
  with the index key forms (7.2, 7.3; `om_data::found::sort_rows`), not
  with SortList: empty values first ascending and last descending, last in
  both with blanks-last; numbers by value. A value-list order puts the
  list's values first in its order (dividers ignored, values compared as
  index keys) and the others after them by the field's own order;
  descending reverses it (unconfirmed). The provider reads the list:
  custom values from `[21, 5, V, 03]`, a field's values from its value
  index with CreateValueListEx in index order (file-metadata.md 6; a second
  field, "sort by second field" and "include only related values" are not
  applied; another file's or a calculated list is 3). A language override
  sorts text with that `Locale::Type`'s collation (`om_calc`'s `SortValues`
  collations: Swedish puts Ä after Z, German sorts it as AE). A summary
  reorder groups the records by the sort field's value (inside the groups
  of the fields before it) and orders the groups by the Total, Average,
  Count, Minimum or Maximum of the summarized field over each group, in
  the sort's direction, ties by the value (inferred from Claris; other
  summary kinds are 3). The probe orders S01-S11 of script-steps.md 11.1.3
  are the memory engine's tests. Sort Records without a stored order and
  without a dialog uses the last sort. After a sort or find the first
  record is current; after an omit the next record takes the omitted one's
  place.
- op92 record downloads always list their keys in key order: OpenMaker
  Server answers a list out of order (a sorted found set's page) with a
  system exception.

Globals (2026-10-29; Claris "Global fields")

- In a hosted file each client's global field values are its own: they
  start from the values the file was last closed with locally, last for
  the client's session and are gone when it closes the file (Claris:
  "Any change a client makes to these initial values persists only for
  the duration of the client's session"). The native provider keeps the
  values a script set (`DataProvider::set_global`, by table, field and
  repetition, as stored text) and its calculation source reads them over
  the file's `[T, 08, F]` until the file closes; nothing is sent to the
  host, so neither the file nor another client sees them. Set Field into
  a global (any repetition) sets it, in Browse and Find mode (where it is
  no criterion, 13.19); a global container field is still 201. Before,
  Set Field into a global was 201 in both modes.
- A user's typing into a text global field on a layout sets the
  session's value too (the provider takes the global out of the record's
  changes; the layout offers text globals for editing), and Set Field By
  Name writes as Set Field does, globals included (both 2026-10-29;
  before, 201). In Find mode and with an empty found set a field reads
  the current request's criterion or empty, a global its value (13.21).
  The commit checks existing values and a field's value list on the host
  (op86 ExistsInIndex), as it checks unique values.
- OpenMaker offers ordinary Number, Date, Time and Timestamp data globals,
  without validation or prohibited modification, through the session-value
  path. Temporal typing uses `DataProvider::set_typed_global`: ordinary
  entry's file-locale parser completes dates or refuses invalid text (500
  for Date/Timestamp, 501 for Time) before changing the global. Scripts keep
  the permissive `set_global` operation. Complete Date text retains its
  accepted spelling (`DateText`). Time/Timestamp readback uses the file's
  locale and the layout object's data format, as ordinary temporal display
  does; the exact original spelling is not retained in calculation values.
  This is an inferred implementation choice, not a measured global-entry
  result in Pro. Claris documents that
  [field formatting](https://help.claris.com/en/pro-help/content/formatting-fields.html)
  changes display, not storage, and defaults to the system formats at file
  creation; its [time](https://help.claris.com/en/pro-help/content/time-fields.html)
  and [timestamp](https://help.claris.com/en/pro-help/content/timestamp-fields.html)
  help ties display to file system formats and the field's format.
- [Repeating fields](https://help.claris.com/en/pro-help/content/repeating-fields.html)
  can hold separate values in data and global fields (documented). Browse
  entry into later repetitions uses their numeric cells (`F * 256 + n`,
  execute-sql.md), never a fabricated schema name or repetition 1. OpenMaker
  offers separate field objects for repetitions 2 through 255 under the
  same type, account and modification guards as the first repetition.
  Global repetitions remain session values; ordinary repetitions join the
  record's other changes in one write. Native Browse repetition and global
  entry parity remains unmeasured.
- Claris documents that a global's validation runs on field exit, rather
  than record commit
  ([field validation](https://help.claris.com/en/pro-help/content/field-validation.html)).
  Non-text globals requiring those checks stay display-only. Intrinsic
  temporal parsing here runs on save; global field-exit timing is unmeasured.
- Tests: `om-gui/tests/client_globals.rs` (a script's globals through the
  window against an in-process server: the stored value first, Set Field
  in Browse and Find mode, the value across scripts, another client and
  the file unchanged), `client_finds.rs` (Set Field into a global in Find
  mode: 0).
- Fields without a record (2026-10-29): in Find mode and with an empty
  found set a script's calculations run on `om_data::host::NO_RECORD`, as
  server scripts do (13.21): a normal field of the window's table reads
  the current request's criterion as text in Find mode and is empty
  otherwise, a global reads its value, nothing is related to it, all
  without error; `Get ( RecordID )` and `Get ( RecordModificationCount )`
  are 0 there, and in Find mode `FoundCount` and `RecordNumber` count the
  requests (calc-functions.md 6.3 O3). Before, every field reference
  ended with 1224. Test: `om-gui/tests/get_selectors.rs`.
- Focus (2026-10-29): New Record and Enter Find Mode put the focus in the
  first field of the layout's stored tab order, Go to Field [ ] leaving a
  field commits the record, Set Selection keeps the selection and Insert
  Text replaces it, and a field being typed into is not yet a modified
  field (calc-functions.md 9, "Oracle answers (desktop Get selectors and
  s33)"). The same rules hold in server scripts.

Summaries (file-metadata.md 5; calc-values.md implementation notes)

- The desktop calculation host computes summary fields with
  `om_data::summary`, the module OpenMaker Server's scripts use (one
  decoder, one computation): every kind and option over the window's found
  set in its order (the provider's found set for record pages), groups by
  the window's last sort for restart, subtotaled fractions and
  `GetSummary`. Summarized values are read as calculations read them (an
  unstored calculation is computed), a weighted average's weight on the
  record or its first related record. ExecuteSQL's summary columns use the
  same module over the query's found set (execute-sql.md 12, S3). Tests:
  `om-data/tests/metadata.rs` (every kind, break fields, unstored inputs,
  ExecuteSQL) and `om-gui/tests/script_engine.rs`
  `summaries_follow_the_windows_found_set` (a script's summaries before
  and after a find and sort).

Steps

- Records: New Record creates the record at once (provider create, as the
  window's New Record) and Revert Record deletes a new record that was not
  committed; Open Record checks the record exists (records lock when they
  are written, write-path.md); Copy Record/Copy All Records put tab- and
  return-separated values on the clipboard; Paste reads the text the last
  Copy or Cut step put there (the system clipboard cannot be read
  synchronously); Truncate Table empties the window's table only (another
  table is 3).
- Replace Field Contents: every found record in order, the calculation
  evaluated on each record made current, serial numbers counting up the
  last run of digits of the initial value; each record is committed; no
  triggers fire.
- Windows (2026-10-29, run 13.25; `app/windows.rs`,
  `scripting/host/windows.rs`): real windows. Each window keeps its own
  layout, found set, mode, view, current record and unsaved values, Find
  mode requests, toolbars, title and calculation results; the file's first
  window is drawn in the root viewport, every other one in an egui
  viewport of its own (a native window; a backend without native windows,
  as the tests', embeds them in the first one). Requests and replies carry
  their window's id; the provider keeps found sets per window
  (`DataProvider::use_window`, `copy_window`, `drop_window`). On the
  worker each window has its own script `Window`: New Window opens one on
  the current window's layout, found set, record, mode and view, named as
  given or after the current window plus " - 2" (" - 3" when taken;
  OpenMaker's choice beyond the engine's one answer), with a layout going
  there with OnLayoutEnter; Select Window makes a named window current
  (names matched front to back, as given and then without case,
  duplicates allowed, then `Get ( WindowUUID )`s, each in every open file
  before the next, as 9.2 documents; over a dialog only the dialog itself
  may be the window found; Close Window, Set Window Title and Move/Resize Window match
  this file's windows the same way, which Claris does not state for them
  ([#320](https://github.com/OpenMaker-Dev/openmaker/issues/320)), and
  letters beyond ASCII fold as Unicode lowercase [I]); Close Window commits
  the window's record, closes it and makes the window current before it
  current again (closing another window keeps the current one); 112 for a
  name no window has. Closing the file's last window closes the file
  (FileMaker Server: 13, the script stops; FileMaker Pro unconfirmed). Set
  Window Title and Move/Resize Window of a named window leave the current
  window current. A script starts from the UI's windows as the user left
  them, in the one it was started from; when it ends, its current window
  is in front unless a visible Open File requested another file's window
  or the user subsequently selected one. Foreground requests are applied
  once after viewport creation, separately from the script's current file;
  native macOS activation and FileMaker foreground parity remain unmeasured.
  A paused script goes on in the window the user left in
  front. `Get ( WindowName )`, the window selectors the session answers
  and `WindowNames` answer for each window (front to back). Window > New
  Window runs the step; the Window menu lists the windows. A window's own
  close box closes it as the user's action (2026-10-30): OnWindowClose
  (and OnLastWindowClose for the file's last window, which closes the
  file), then its record's commit with OnRecordCommit; the window stays
  until both let it go (the viewport's close is cancelled). File > Close,
  Cmd-W and Quit, and the root window's close box (which quits), close
  every window of the file that way, one at a time from the window acted
  in, each committing what is left on its record (the user's typing and
  what scripts left) before it goes; a veto or a refused commit stops
  there with that window and the ones after it open and their changes
  kept, the windows closed before having committed theirs. FileMaker's
  order of these events for several windows is not measured. The next
  file starts from a fresh window (title, toolbars, zoom, view, `Get (
  WindowUUID )`). Exit Application commits its own window, and once its
  script has ended every other window closes as Quit closes them; while a
  script runs the root close box does nothing (Esc or Cmd-period stops
  the script first). Before, closing the file kept only the acting window's
  record and dropped every other window's changes, and the root close box
  quit without committing anything. The script step Close Window still
  commits without OnWindowClose (as before). Records a window commits, creates or deletes (by
  the user or a script) make the file's other windows read their records
  and calculations again. A trigger names the window its event happened
  in. Adjust Window becomes a viewport command. Resize to Fit sizes the
  window to its layout at its zoom (script-steps.md, Adjust Window; on a
  card it does nothing, OpenMaker's gap). Set Zoom Level zooms the window's
  layout, Scroll Window scrolls it, and Arrange All Windows does nothing.
- Window styles (2026-11-02; `scripting/host/windows.rs`,
  `app/window_styles.rs`; Claris "About window styles" and "New Window",
  not measured against FileMaker Pro). New Window and Go to Related
  Record's new window read the style word (script-steps.md, "New Window
  style word"). A Card belongs to the window current when it opens (its
  parent), which has one card at a time (a second is 3, OpenMaker's
  answer). It is drawn over the parent's layout area in the parent's own
  viewport, with no title bar, frame or status toolbar: centred unless
  Top/Left are given (measured from that area), the area's size unless
  Height/Width are; Move/Resize Window moves it on the parent. Its parent
  takes no input until it closes (dimmed with Dim parent window); the
  viewport's menu bar and keyboard work on the card, the Window menu lists
  no cards, and other windows stay usable. Its close box (when any of the
  Close/Maximize/Resize bits is set) closes it as a window's close box
  does. Closing a card makes its parent current and front; Close Window of
  a parent closes its card first (a card whose record will not commit
  fails the step with both open), and the user's close of a parent closes
  only its card. A Dialog is its own always-on-top viewport without a
  minimize button; while one is open every other window is drawn disabled
  and a click there brings the dialog forward, New Window opens only
  dialogs and cards, Select Window selects only the current window and Go
  to List of Records opens no new window (3, OpenMaker's answer); closing
  it returns to its opener. `Get ( WindowStyle )` answers 0, 2 or 3. A
  card or dialog from Go to Related Record into another file's window, and
  Floating Document or any unmapped style, stop the script before a window
  opens ("not supported here yet"). A window left without its parent is
  closed with it, so a card's controls and replies never reach another
  window or record. A card's parent and a window behind a dialog count as
  covered for web viewers (`document.overlays`), so their native web views
  hide rather than take input through the modal window; a card's own web
  viewers use its parent viewport's native window. Not done: option bits beyond Dim parent window and the
  close box (dialog Close/Maximize/Resize, Menu Bar, Toolbars), OnTimer
  under a dialog, keeping a card from being minimized with its parent's OS
  window.
- Go to Related Record (script-steps.md 11.8; the runtime now reads "show
  only related records" from `kRestore`, step value 1 as the current
  layout and the window name from slot 0): related records come from the
  calculation schema's relationships; none is 101, a layout of another
  table 106. The first related record is current, or the active portal
  row's record (13.24). Without "show only related" the window's found set
  of the related table stays (13.24; before, every record was shown), and
  when it lacks the record every record is shown (OpenMaker's choice).
  With external layouts (`kOption`), the GUI opens/reuses an owned peer
  session through the declared data source's ordered paths (desktop-files.md,
  Supported sources) and
  resolves the layout only in that session. The hydrated table maps back
  to its native table id; record ids are already file-local. A missing
  peer layout is 105 even when Root has the same layout id; another
  table's layout is 106. If path resolution selects a different file from
  the owner that supplied the related ids, the step returns 3 and retains
  the caller window rather than reinterpreting the ids. Files are told
  apart by host, port and name (`FileIdentity`): one name on another
  host, or another port of a host, is another file; an import or a file
  that does not say which it is is matched by name. The existing
  related-data hydrator's source-discovery limits still apply; this change
  does not add fallback reads or Root layouts over imported tables.
  Current-record and found-set matching, show-only-related, the active
  portal record, current/key/calculated destination layouts and a new
  peer-owned window use the same path. New windows apply their optional
  name and bounds; they suppress FirstWindowOpen/WindowOpen (Claris
  [Go to Related Record](https://help.claris.com/en/pro-help/content/go-to-related-record.html),
  Notes), and external destinations are unsorted. FileMaker Pro 26.0.3,
  measured locally (a native differential, 2026-11-17): value 1 (current
  layout), External, show only related, no key, no new window and no Match
  current found set, run from Root (on its first layout, record 1 of 2)
  while the related file was open in a visible window on its second layout
  (record 2 of 2):
  - the script went on in Root: error 0, `Get ( FileName )` Root,
    `Get ( LayoutName )` Root's layout;
  - the related file's window kept its second layout and showed 1 of 2
    records, the matching one current;
  - no window opened.
  
  An earlier observation of the same step without show only related
  (flags `0x102`) also left the script in Root, and did not narrow the
  related file's found set. OpenMaker follows only the first case: the
  destination's window takes the related records in the background, with
  no layout trigger there [I], and the caller's script and window stay.
  Not measured, and OpenMaker still goes to the destination as before:
  - the related file showing no window;
  - no related record;
  - Match current found set, a new window, a layout by key or name;
  - hosted files;
  - whether the related file's window comes to the front.
  
  Runtime frames,
  parameters, local variables and script-file global variables survive the
  window switch; an external/subscript return restores its caller window.
  Evidence: `scripting/databases_tests.rs` and `tests/related_files.rs`,
  generated files served by OpenMaker Server, not native desktop parity.
  Native external-step encoding, native window activation/event ordering,
  animations and filtered-portal subsets remain unconfirmed or
  unsupported; a new window's card or dialog style applies only within the
  caller's file (Window styles above). Dedicated cybersecurity auditing remains deferred.
- Insert from URL: HTTP and HTTPS only (other schemes 1626, a malformed URL
  1630), with Claris's cURL options that apply to HTTP (see `url.rs`).
  Certificates are always verified (FileMaker skips verification when
  "Verify SSL Certificates" is off; OpenMaker does not, deliberately). A 401
  is 1627 without `--show-error`; with it any status of 400 or more is
  1631; other statuses return the body. Transport failures map to 1628,
  1629, 1631 and 1634. A request whose option values could split it fails
  with 3 before anything is sent or any helper starts (OpenMaker's
  choice; cURL sends such values as given, and FileMaker's behavior is
  unmeasured): a method (`-X`) that is not an HTTP token (a space, CR or
  LF), a header (`-H`, `-A`, `-e`) with a byte other than visible ASCII,
  space or tab, a `Content-Length` other than the body's length or given
  twice, a `Transfer-Encoding` other than one `chunked`, or both. The
  step ends with 1629 at `-m` (else an hour), also during a DNS lookup,
  and its connect timeout is cut to that limit; in a server script
  neither outlasts the script's time limit, whatever `-m` asks (the
  script is aborted there). A cancel (Esc or a closing window on the
  desktop; an administrator's abort, a closing file or shutdown on the
  server) ends the step with 1 within 50 ms at any point of the
  transfer. The HTTP client is blocking, so a cancelled transfer goes on,
  on its own thread, until its socket answers or fails, it reads its next
  body chunk, or its time limit passes. At most 128 helper transfers run
  at once, including the cancelled ones: capacity is reserved before a
  helper starts and released when it ends. While all slots are held, a
  new transfer fails at once with 1631 (OpenMaker's choice). A container
  field gets container data, other fields UTF-8 text (2026-10-30,
  containers.md "Implementation notes (Insert from URL into
  containers)"); `Get ( LastErrorDetail )` is not set.
- Perform Script on Server commits the record first; a waiting call
  re-reads the window's records afterwards.
- Insert from Index shows the View Index dialog with the field's
  value-index values (CreateValueListEx, in index order; "Show individual
  words" is not offered); Paste inserts the value at the insertion point
  of the field entered (replacing its selection; at the end of a field not
  entered) or replaces the contents with "Select entire contents"; Cancel
  is 1.
- Set Next Serial Value sends op72 with `kReplace` for the field of the
  step's occurrence's table, any table (write-path.md 3.2); the host stores
  the text, also for a field without an auto-entered serial (unconfirmed).
- Steps the desktop client answers with 3 (and, as before, stops on with a
  message): Relookup, Undo/Redo,
  Export Field Contents, Set Web Viewer, Perform JavaScript in Web Viewer,
  the account steps, Open File and the other database file steps, Show/Hide
  Text Ruler, Enable Touch Keyboard, and every step of script-steps.md 7
  tier 3.

Writes (2026-10-29; 13.20-13.22; `scripting/host/writes.rs`, `om-provider`
`records.rs`)

- A script's writes on the desktop follow a script's rules, as on
  OpenMaker Server: Set Field, Set Field By Name and the insert, paste and
  replace steps check the rules FileMaker checks as a value is entered
  (maximum characters 511, numeric only 502, 4-digit year 500, time of day
  501, range 503, a value list 506, the field keeping its value) at Set
  Field, for a field validated Always only (`DataProvider::check_entry`,
  `RecordPlan::check_entry` with `Entry::script`; Open Transaction's "Skip
  data entry validation" applies); a calculation or summary field is 201
  and a repetition past the field's 111 at Set Field. The record is
  committed with `DataProvider::write_record` (`ScriptWrite`: cells by id,
  repetitions too, any table) and `Entry::script`: any normal field of any
  type, one whose auto-entered value prohibits modification included,
  "only during data entry" rules applying (13.20), Commit Records' "Skip
  data entry validation" passed on. On a new record a script's values are
  never replaced by the creation's auto-entered ones (FileMaker enters
  those at New Record, before the script's Set Field). Inside a
  transaction the record is held and planned so at Commit Transaction.
  Before, a script's writes were a user's: data-entry-only rules applied,
  the entry rules fired at the commit, number fields and prohibit-
  modification fields were 201, and repetitions were error 3.
- A user's typing keeps a user's rules (`Entry::default`), also when a
  script's commit writes it: the typed cells of a `ScriptWrite`
  (`ScriptWrite::typed`, a value no script set) are checked as a user's
  entry, and typing into a global sets the session's value, never the
  record's.
- What scripts leave on a record (2026-10-29): the cells Set Field set
  (repetitions too), the related records changed, created or deleted
  through it, and a record New Record made stay with the record when the
  script ends, as FileMaker keeps the record open. They belong to the
  window's record, not to the worker: the UI keeps them per window with
  the record (`WindowState::uncommitted`, `scripting/window.rs`
  `Uncommitted`; `app/windows.rs`), and the next script starts from them.
  The user's save of that record commits all of it on the worker
  (`Start::Commit`: the record and its related records together, as a
  script's commit, without firing OnRecordCommit again; OnWindowTransaction
  runs), going to another record, another layout, changing mode, or closing
  the file saves it first (a layout button's script starts from it instead), and
  the user's revert drops it and deletes a record New Record made
  (`Start::Revert`). The edit bar shows the record as unsaved.
  A save the user makes while a script is paused runs on top of it, in
  the window it was made in, and the action it held goes on there while
  the script stays paused; a job queued behind another one on top of the
  pause starts at the next pause, not when the paused script ends
  (2026-10-30); a failure is the user's message and leaves the record
  open. The user's own OnRecordLoad and OnWindowTransaction then wait for
  the paused script (a delayed trigger of the user's event waits for
  running scripts, 13.12, and a window event does not fire while one
  runs), and run when it has ended. Before, the
  worker kept these changes for its next script: a user's save or revert
  left them to come back at a later script's commit, and the user's save
  or a move to another record lost a script's repetitions and related
  changes.
- Tests: `om-provider/tests/finds_and_transactions.rs`
  (`script_writes_take_a_scripts_rules_in_and_out_of_transactions`,
  `typing_into_a_global_field_sets_the_sessions_value`),
  `om-gui/tests/client_portals.rs` (`script_writes_follow_a_scripts_rules`;
  `the_users_revert_drops_what_scripts_left_on_the_record`,
  `the_users_save_commits_what_scripts_left_on_the_record`,
  `leaving_the_record_commits_what_scripts_left_on_it`,
  `a_typed_global_stays_the_sessions_when_the_save_commits_what_scripts_left`).

Portals (2026-10-29; 13.24; `scripting/host/portals.rs`)

- A portal shows its occurrence's records related to the current record in
  the relationship's order (file-metadata.md 4.3), the records a script is
  creating through it, and the creation row when the relationship allows
  creating records and the portal scrolls (`PortalLayoutObject.flags` bit
  `0x1`; a portal that does not scroll reaches its `numRows` visible rows).
  A portal of the layout's own occurrence is the found set: its steps are
  Go to Record's and Delete Record's.
- The worker window keeps the active row (`PortalRow`: portal, occurrence,
  row from 1 or 0 for the portal itself, the field in it and its
  repetition: Go to Object's or Go to Field's, else 1; Go to Portal Row
  keeps it with the field). Go to Portal
  Row (First, Last, Previous, Next, by calculation; Select; Exit after
  last), Go to Object (by the object's stored name) and Go to Field set it
  with 13.24's errors (101, 116, 5, 102); Go to Record keeps its number on
  the next record (101 when it lacks the row); Commit Records, a layout or
  mode change and Revert Record clear it. A move to another row or object
  first leaves the field the focus is in, a portal row's field too: its
  OnObjectExit runs before the move and returning False keeps the focus
  and the row. Go to Field [ ] leaves a portal row's field as it leaves
  any field: the record is committed (s33) and no row is active. `Get ( ActivePortalRowNumber )`
  is the row (0 without one), `Get ( ActiveFieldName )` and `Get (
  ActiveFieldTableName )` the portal field's name and occurrence, `Get (
  ActiveRepetitionNumber )` and `Get ( ActiveFieldContents )` its
  repetition and that repetition's value in the row [I: the Claris page
  of Go to Object; 13.24 measured repetition 1 only]. A field of the
  layout's table that takes the focus (Go to Field, Go to Object) is the
  active object instead: no row is active, and the selectors answer that
  field (repetition 1) [I: not measured];
  calculations read a related field of the portal's occurrence from the
  active row's record (empty on the creation row).
- Set Field into a related field writes the active row's record, else the
  first related record, else creates a related record when a direct
  relationship allows creating its records (each `=` predicate's field set
  from the current record), else 101. Delete Portal Row deletes the active
  row's record (3 without a row). Both are held as the current record's
  related changes (`RelatedDraft`) and committed with it: the record first,
  then its related records, all in one provider transaction when no
  script transaction holds them, so one refusal (a related record's
  validation, 507) commits nothing and leaves everything as it was, the
  record still open; Revert Record drops them. Calculations see a changed
  related record's values and not a deleted one before the commit; a
  deleted record is its base table's, whichever occurrence reaches it.
- Go to Related Record from the active row makes that row's record
  current in the related layout.
- With dialog: Delete Portal Row asks "Permanently delete this one
  related record?" (client-ui.md, "Delete confirmations"); Go to Portal
  Row's dialog is not shown. Which portal option allows deleting rows is not known (none of
  the words the engine was given stopped it): the desktop always deletes.
- A portal that sorts its own rows or filters them numbers them for
  scripts as Browse shows them (#192; Claris, Go to Portal Row): its
  `KSortOrder` when portal option `0x8` turns it on (a stored order alone
  does not sort, as in Browse), then its `KFilter`, evaluated on each
  related record's row in the window's record's context (calc-encoding.md
  8, `om_layout::ObjectCalcs`; om-data `host::sort_records` and
  `host::layout::holds`). A row a step moves to keeps its record from then
  on, so that a step's own change of that record does not move the row to
  another record before the commit [I: FileMaker re-sorting a portal only
  when the record is committed is not measured]. Go to Related Record with
  "Show only related records" from a filtering portal's row finds the
  records the portal shows. Only a sort or filter that does not decode
  leaves the order unknown: then the active row reads no record, and Set
  Field, Delete Portal Row and Go to Related Record from it are 3.
- Not done: portals whose relationship path has more than one hop for
  creation.
- A click on a button in a portal row (2026-11-02; docs/scripting.md,
  "Desktop buttons in portal rows") enters that row, without a field,
  before the button's parameter and step: the window sends the row's
  related record, found again among the portal's rows (numbered where it
  is in the relationship's order; Browse's number in a portal sorting or
  filtering its rows). That clicked record is the active row's for reads,
  Set Field, Delete Portal Row and Go to Related Record while it stays
  related. A row no longer related is refused before anything runs [I:
  what FileMaker does for a button click in a row is not measured].
- Tests: `om-gui/tests/client_portals.rs` (rows, creation, deletion, a
  related record's validation, OnWindowTransaction, sorted and filtered
  portals' rows, row buttons) against an in-process OpenMaker Server with a portal layout;
  `scripting/host/portals.rs` unit tests (which portals sort or filter).

Transactions (8; write-path.md; docs/record-writes.md)

- The provider holds the transaction (`DataProvider::begin_transaction`,
  `commit_transaction`, `revert_transaction`; `om-provider`
  `transaction.rs`). Open Transaction first commits the window's record
  outside the transaction (without OnRecordCommit; unconfirmed), then takes
  one write transaction id (write-path.md 1.2) for everything until Commit
  or Revert Transaction.
- Inside it, leaving a record (Go to Record, Commit Records, a mode or
  layout change) commits the window's drafts into the transaction: the
  provider holds them, nothing is uploaded. New Record reserves the id with
  op71 under the transaction's id at once, as FileMaker Pro does (1.1); the
  first change or deletion of an existing record locks it with op60 under
  that id (refused: 301 "Record is in use by another user"; changed by
  another client since the window read it: 306), and the locks stay until
  the transaction ends, so no other user changes the script's records.
  Record pages show the held records (edits applied, new records in the
  table's record list, deleted ones gone). Finds run on the host and see
  its committed records only, and calculations other than on the window's
  current record read committed values (deviation: FileMaker Pro keeps the
  transaction's records in its own cache).
- Triggers: committing a record into the transaction fires OnRecordCommit
  as outside one; Commit Transaction and Revert Transaction fire neither
  OnRecordCommit nor OnRecordRevert (8.2, 8.3); OnWindowTransaction fires
  once for the transaction's records after Commit Transaction (13.26).
- Commit Transaction commits the window's record into the transaction,
  then applies the record rules (`om_data::rules`,
  `RecordPlan::prepare_entry`) to every held record with the transaction's
  options: auto-enter values unless "Skip auto-enter options" (Claris
  "Open Transaction": auto-enter is "performed ... only when the Commit
  Transaction script step is reached"; stored calculations are recomputed
  anyway), validation (with "Skip data entry
  validation" from Open Transaction or from Commit Transaction, fields
  validated only during data entry are skipped), unique values against the
  host's index (op60 on `[T, 01, F]` and ExistsInIndex under the
  transaction's id, 1.3) and among the held records. Nothing is uploaded
  until every record passes. Then each table's records go up with op14 and
  one op70 lists every record of every table, in key order, under the
  transaction's id (3.4, "by the grammar, several tables"; OpenMaker Server
  applies such a list under one hold of its store, and its reader needs the
  keys in order). Every record is read back (op70 carries no status) and
  op65 releases the transaction's locks. A refusal before the upload
  reverts the whole transaction and is Commit Transaction's error (507,
  509, 504, 301, ...). "Override ESS locking conflicts" has no meaning for
  a hosted FileMaker file and is ignored.
- Which step errors mark the transaction is the runtime's rule (13.10:
  only the data layer's; a sub-script's own Open and Commit Transaction
  are 3 and leave the caller's transaction open, its other steps run in
  it; Revert Transaction in a sub-script reverts the caller's transaction;
  a custom revert code is Commit Transaction's error). The host gives the
  record steps FileMaker's codes: a write the record rules refuse carries
  the rule's code (201 for a field the client cannot write, 102 for an
  unknown field, 504, 507, 509, 511 for validation), a lock 301 or 306, a
  missing record 101. Set Field gives FileMaker's own errors at Set Field
  (201, 111, 511, 500-503, 506; "Writes" below) and keeps the value as the
  window's draft; the other rules refuse at the commit.
- Revert Transaction, and the runtime's reverts (an error with Set Revert
  Transaction on Error On, Halt Script, the end of the script), drop the
  held changes and send op65; ids op71 reserved stay used (FileMaker does
  not give back serials either). Closing the file drops the transaction;
  the host releases the connection's locks.
- Where FileMaker Pro differs or is unconfirmed: its own transaction was
  not captured. By write-path.md 1.1 and cap:1022-1026 it keeps the
  records under the window's DB transaction, commits several records with
  one op70 under one txn and then sends op103 with the transaction's files
  (OpenMaker sends no op103: the answer carries nothing, 3.5). Whether it
  uploads each record when the record is left (op14 then, op70 at Commit
  Transaction) or everything at the end, and whether it validates a record
  when it is left or at the commit, was not known; FileMaker Server's
  engine validates at Commit Transaction only (13.27: the step leaving the
  record is 0, Commit Transaction 507 and everything reverted), and
  OpenMaker does the same, validating and uploading at the commit only
  (FileMaker Pro's own timing is still unconfirmed; it shares the
  engine). Cross-record calculation order, including later-held related
  rows and cycles, remains unmeasured; the final-preflight dependency
  scheduling described in record-writes.md is OpenMaker's choice, not an
  observed FileMaker transaction rule. FileMaker Server applies an op70
  list record by record and reports nothing (3.4), so there the commit is atomic only
  in that every record was checked and locked first: a record the host
  skipped shows in the read-back and is reported as a failed commit after
  the others were written. Changes in other windows of the file are inside
  the transaction (one provider per file; FileMaker keeps them out, 8.5).
  Truncate Table inside a transaction deletes the records through the
  transaction, so a revert keeps them (Claris: Truncate Table cannot be
  reverted). Import Records' rows are held with the import's own rules
  (`NativeProvider::hold_import`: applied when the row is held, as
  OpenMaker Server stages an imported row; a new record's id reserved and
  locked with op71, an existing record locked), and Commit Transaction
  writes their cells as those rules left them, applying the transaction's
  rules only to a script's later edits of the record (2026-10-29; before,
  the held rows were planned again as a user's typing). An import never
  creates a table.
- Import Records writes rows through `DataProvider::import_record`
  (2026-10-29, the import review's C6): cells by id, every repetition
  (`F * 256 + n`) and every field type, with `om_data::rules::Entry::import`
  (a script's write, "only during data entry" validation not checked,
  auto-enter only with "Perform auto-enter options" and never over a value
  the row sets), unique, existing and value-list rules checked on the host
  with that entry, each written cell read back. An update of another table
  than the window's works on its records in creation order
  (`CalcNeed::Records`), as FileMaker Server does (oracle x04). Before,
  rows went through the user's typing path: a number, date or time column
  refused every row (201), "only during data entry" validation applied,
  a field whose auto-entered value prohibits modification was refused,
  auto-enter always applied and only first repetitions were written.
- Tests: `om-provider/tests/finds_and_transactions.rs` (both tables of an
  in-process OpenMaker Server's file committed together or not at all,
  the Skip options, revert, locks), `om-gui/tests/script_engine.rs`
  (commit, revert, a failing commit, a sub-script's Open/Commit and
  Revert with a custom code, OnRecordCommit inside a transaction, imported
  rows' rules) and the ignored hosted tests over test project;
  `om-gui/tests/script_transfer.rs` runs Export and Import Records through
  the window against an in-process OpenMaker Server.

Files (the data file steps; Claris script steps reference)

- `scripting/files.rs`. A path list has one path per line, tried in
  order. `$name`/`$$name` anywhere in a line (a name ends at `/`, `\`, `:`
  or the end of the line) is replaced by the variable's value, empty when
  it does not exist, and a value of several lines is several paths (the
  runtime's `paths`; Claris "Creating file paths" examples). Prefixes:
  `file:` (or none), `filemac:`, `filewin:`, `filelinux:`; another
  platform's prefix and other schemes (`fmnet:`, `image:`, URLs) are
  skipped. Full paths: macOS `/volume/...` (the startup disk, found as the
  `/Volumes` entry that links to `/`, or a mounted volume), Windows
  `/C:/...` and `//computer/share/...`, Linux `/...`. Relative paths are
  relative to the user's Documents folder (Claris: for a hosted file).
  On macOS a POSIX path such as `/private/tmp/report.pdf` therefore names
  a volume `private`, which does not exist (826); FileMaker Pro's save
  panels write the volume (`/Macintosh HD/private/tmp/report.pdf`), and
  whether its steps take such a path as one on the startup disk is [U]
  (#570). OpenMaker's in-app file dialogs convert a native absolute path
  to this form before the step reads it.
- Policy: the steps reach what the user's own OS permissions allow, as in
  FileMaker Pro, and nothing a path does not name plainly. `.` and `..`
  are resolved on the text before the path is used; a relative path whose
  `..` leaves the Documents folder, a part with a NUL (or, on Windows, a
  `:` outside the drive) and an unknown volume are 826 "File path
  specified is not a valid file path". Delete File and Rename File act on
  files, not folders (100), and not on a data file a script has open
  (300); Rename File's new name is a name only, without a folder,
  separator or `..` (826), and never replaces an existing file (800).
  Create Data File replaces a file, never a folder (800). At most 25 data
  files are open (2), each once (300); a read or write moves at most 64 MB.
  The open files, these limits, the encodings, positions and their errors
  are `om_scripts::runtime::data_files`, which OpenMaker Server's data file
  steps share; `scripting/files.rs` finds the desktop's files.
- `Get ( DocumentsPath )` and `Get ( DesktopPath )` are the user's
  `Documents` and `Desktop` folders, `Get ( TemporaryPath )` a folder of
  this process in the system's temporary folder (`OpenMaker-S<pid>`),
  created when first asked for, each in FileMaker's form with a trailing
  `/`. `Get ( OpenDataFileInfo )` is one line per open file: the id, a tab
  and the `file:` path (the Claris example shows the separator as spaces;
  unconfirmed).
- Create Data File empties an existing file and creates missing folders
  only with "Create folders" (else 100). Open Data File: missing 100; ids
  count from 1 per database file; a read-only file opens for reading.
  Write to Data File writes at the position (overwriting): UTF-16 little
  endian without a byte-order mark, or UTF-8; container data as its bytes;
  "Append line feed" adds LF. Read from Data File reads from the position
  an amount of bytes (UTF-8, Bytes) or UTF-16 code units, the rest of the
  file without one, and 10 when nothing is left; Bytes give container data
  named after the file. Set Data File Position counts bytes from the start;
  a negative position is 10 (FileMaker's code is not given). A file id that
  is not open is 100 (unconfirmed). Get File Exists is 1 for files and
  folders; Get File Size of a folder is 100. OS errors: missing 100, disk
  full 809, others 800 when creating and 802 when opening.
- Data files close with Close Data File or when the database file closes
  (FileMaker: when the application quits).


Desktop review follow-ups (2026-10-30): single related-field reads and a
Set Field without an active portal row use the related set after pending
portal deletions, as aggregates do. Deleting its first record therefore
moves the read and write to the next remaining record, not a deleted row
or a newly created one. Covered by client_portals' delete-then-edit test.

The user's change from Browse to another mode commits all pending edits,
including script repetitions and related records, before changing the UI.
OnRecordCommit and OnModeExit may veto it, and an approved action's delayed
save does not repeat its triggers. This follows the existing worker mode
path and Claris' [Committing data in records](https://help.claris.com/en/pro-help/content/committing-data.html).
Tests cover entering Find, both vetoes, and the user's save/revert of a
record a script created. These are synthetic in-process-server checks;
they do not replace the outstanding FileMaker Pro portal/window probes.
