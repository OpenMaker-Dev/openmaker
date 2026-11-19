# Script steps: descriptors, parameter storage, schemas

Status: every stored parameter location is specified (sections 2-5), and every
named step id has a schema row in
[catalogs/script-step-params.csv](catalogs/script-step-params.csv). Checked
against all 73 local sample files (66 templates, 6 FMServer_Sample languages,
test project): 2,638 steps of 47 opcodes. Every stored leaf (5,653) and every
set flag bit (2,782) falls under a location the schema names for its step, and
all 393 cached control-flow indexes agree with section 3.1. The check is
`crates/om-scripts/examples/step_params.rs` (section 8). A calculation
parameter is an opaque formula slot here; its encoding belongs to
calc-encoding.md. Runtime behavior is in [script-runtime.md](script-runtime.md);
the catalog root is in [scripts.md](scripts.md). Section 11 specifies the
structures stored inside steps (sort orders, find requests, import and
export options, translation profiles, print records, compound slot maps),
checked against all 746 corpus files, FileMaker's own Save as XML and runs of
FileMaker Server's script engine. Section 12 maps the slots of the steps added
in FileMaker 21-26, which no sample file holds, and the compound elements of
Send Mail, Send Event, Speak, Perform Find/Replace and Create PDF, from the
steps' XML readers and writers and FileMaker Server's XML importer.

## 0. Evidence boundary

| Source | What it gave |
|---|---|
| FileMaker Server 26.0.3.309 `libFMEngine.so` DWARF | `Draco::ScriptStep` layout, `ScriptStep::StepFlags`, `StateFlags`, `DBFieldSpec` |
| `V2_ScriptStepXML::ProcessParameterValues` (0xed2bf0) and `ScriptStepXML::ProcessParameterValues` (0xeafdf0), Ghidra (no-analysis import, on-demand decompile), vtable resolved with `tools/elf_vtable.py` | per step: the XML parameters in order, their names, the `StepFlags` bit of every Boolean, calculation slot numbers |
| `ParameterListFactory::GenerateParameters` (0xe1dea0) | per step: the Script Workspace parameter model (labels, bits, list enumerations, some slot numbers) |
| `ScriptStep` accessors (`Get/SetTextData`, `Get/SetListValue`, `SetNumericData`, `Get/SetFieldSpec`, `GetCalculation`, `GetSortSpec`, `SetBlob`, ...) | the key under which each kind of parameter is stored |
| `_GLOBAL__sub_I_DBGlobals.cpp` in `libDBEngine.so` | values of the `Draco::K*` key constants ([catalogs/hbkey-constants.csv](catalogs/hbkey-constants.csv)) |
| DBEngine string resource 129 | option labels (`label` numbers below are its indexes) |
| corpus (read-only copies) | storage confirmation for 47 opcodes |
| Claris script steps reference (cached under `re/work/script-steps/`) | option names and enumerations where the binary is silent |
| Mac FileMaker Pro 26.0.3 `FMEngine` x86_64 slice: the per-step `V2_ScriptStepXML::Serialize*`/`Deserialize*` pairs and their `kXMLEngine_*` tag strings | section 12: which element each slot, list and flag is written from and read into |
| FileMaker Server 26.0.3 FMUpgradeTool and FMDeveloperTool (`tools/fms-script-harness`) | section 12: the leaves FileMaker's XML importer stores for 115 probe steps, and the XML its writer shows for them |

Tags: **[O]** observed (code or data), **[I]** inference. Confidence High
unless marked Med/Low.

## 1. Where a script lives

Scripts are members of catalog `[17]` (scripts.md): `[17, 5, S]` per script.

| key | name | content |
|---|---|---|
| `04` | step list | the step descriptors, 28 bytes each, concatenated (chunked when large) [O] |
| `05` | step data | one subtree per step, keyed by the step's data key (section 4) [O] |
| `10` | `KName` | script name, masked SCSU text (chunked when long) [O] |
| `13` | (`KCompatibility`/`KPrefixKey` share the number) | u32, 0 in all 361 samples [O]; meaning unknown |
| `d8`, `d9` | `KUniqueID`, `KSourceUUID` | 16-byte UUIDs [O] |
| `f0` | `KMemberFlags` | u64; values 0, 2, 8, 9, 0xa, 0xd, 0x18 seen; bit 0 marks a folder (scripts.md) [O]; bit 3 (`0x8`) leaves the member out of the Scripts menu: FileMaker's XML writes it as the `Options` value with `hidden="True"` (oracle, "Implementation notes (Script Workspace)") [O]; "run with full access" and Siri visibility (XML `runwithfullaccess`, `SiriShortcutVisible`) are not mapped [I] |
| `f1` | `KPrivateMemberFlags` | u64, 0 [O] |
| `fc`, `fc01`-`fc03` | mod count, user, account, timestamp | as for every catalog member (schema-path.md 8) [O] |

Key constant names are the DBEngine `Draco::K*` constants; their values are
written by `_GLOBAL__sub_I_DBGlobals.cpp` through GOT slots. All 229
single-segment and two-byte constants are in
[catalogs/hbkey-constants.csv](catalogs/hbkey-constants.csv) [O].

## 2. The step descriptor (28 bytes)

DWARF `Draco::ScriptStep` (size 0x44 in memory; the first 28 bytes are what
`ScriptStep::Read`/`Write` stream, big-endian on disk) [O]:

| off | size | DWARF member | meaning |
|---|---|---|---|
| 0 | 6 | `mDataKey` (`HBKey<5>`) | the step's data key: names its subtree `[17, 5, S, 5, key]` |
| 6 | 14 | `mReserved` (`unsigned char[14]`) | one inline reference: a `DBFieldSpec` or a catalog member key (section 4.1) |
| 20 | 2 | `mStepID` (`short`) | step id = opcode (catalogs/script-steps.csv); negative = disabled (`IsStepEnabled` tests the sign, `SetStepEnabled` negates) |
| 22 | 2 | `mStepValue` (`ushort`) | step value: list choice or cached index (section 3) |
| 24 | 4 | `mStepFlags` (`StepFlagSet` = int32) | `StepFlags` bits (table below) |

`mStateFlags` (offset 0x1c), `mFullDataKey` and `mPlugInID` are in-memory only
[O].

**Correction to the repository.** `om-scripts` decodes bytes 22-23 as
`flags: u16` and bytes 24-27 as `value: u32`. DWARF names them the other way
round: bytes 22-23 are `mStepValue`, bytes 24-27 are `mStepFlags` [O]. The
layout protobuf agrees with DWARF: `LayoutObject.proto` `ScriptStep` field 2
`value` and field 3 `flags`; the Contacts list arrow (Go to Layout) has
value 5, flags 8 = `kMemberKey`, exactly what a catalog Go to Layout stores in
bytes 22-23 (5) and 24-27 (8). `ScriptRuntimeBase::GetGotoValue` reads offset
0x16 = `mStepValue`. Renaming the two fields (and `start_button`'s mapping) is
recommended; the byte layout used by the crate is right. (Done: the crate
now has `value: u16` and `flags: u32`.)

### 2.1 HBKey fields

An `HBKey<N>` is `[u8 length][N bytes]`: `length` counts the key bytes that
follow; the key itself is a sequence of segments `[u8 seglen][bytes]`. Bytes
past `1 + length` are not part of the key and are not normalized: `be`, `00`
and stale bytes were all seen (data key `02 01 06 00 6f 00` in
FMServer_Sample) [O]. A decoder must read only `1 + length` bytes;
`om_scripts::step_key` also demanded `00 be` padding and therefore rejected
every FMServer_Sample script [O]; the crate now reads only the key.

Examples: data key `02 01 07` = segment `07` (step data key 7); table
occurrence `04 03 d0 00 05` = segment `d0 00 05`; layout `02 01 0b` = layout
11; field `02 01 05` = field 5.

### 2.2 StepFlags

| bit | name | used for [O unless marked] |
|---|---|---|
| `0x1` | `kFieldSpec` | a field (or variable) target is stored in `mReserved` (`SetFieldSpec` slot 0) |
| `0x2` | `kMenuSelection` | `mStepValue` holds a list choice |
| `0x4` | `kTextData` | the step has text data (`SetTextData` sets/clears it) |
| `0x8` | `kMemberKey` | `mReserved` holds a catalog member key (layout, menu set; `SetLayoutKey`, `SetMenuSetKey`) |
| `0x10` | `kFileReference` | file references stored under `20` (`SetFileReferenceKey`) |
| `0x20` | `kFilePath` | a path list is stored under `82` (`SetUniversalPathList`); also selects "file" in Print PDF |
| `0x40` | `kScriptKey` | `mReserved` holds a script key (`SetScriptKey`) |
| `0x80` | `kNoInteract` | "With dialog: Off" (the XML Boolean "With dialog" is its inverse) |
| `0x100` | `kOption` | step-specific option (see each step); Perform Script on Server: set = do not wait (the editor shows "Wait for completion" inverted) |
| `0x200` | `kOption2` | step-specific option |
| `0x400` | `kUNUSED1` | reused by 26.x steps as a third option bit (AI steps) |
| `0x800` | `kStoreReference` | "Store only a reference" |
| `0x1000` | `kSelectAll` | "Select entire contents"; reused as an option bit by newer steps |
| `0x2000` | `kExit` | "Exit after last" (Go to Record, Go to Portal Row) |
| `0x4000` | `kCalculation` | the step has calculation slots; `GetCalculation` returns nothing without it |
| `0x8000` | `kExtendedData` | a spec subtree is stored (sort `0a`, requests `06`, import/export `84`/`85`, print `87`/`88`); `GetSortSpec`, `GetPrintSpec` test it |
| `0x10000` | `kSetOnOff` | on/off steps; written alongside `kTurnOn` by some writers |
| `0x20000` | `kTurnOn` | on/off steps: On; also set by `SetBlob` |
| `0x40000` | `kLinkAvail` | "Replace UUIDs" (Save a Copy as Add-on Package) |
| `0x80000` | `kLock` | "Lock" (Show/Hide Toolbars, Set Zoom Level, Show/Hide Menubar) |
| `0x100000` | `kBreakpoint` | debugger breakpoint (any step) [I] |
| `0x200000`, `0x400000` | `kNotMac_Deprecated`, `kNotWin_Deprecated` | obsolete |
| `0x800000` | `kDisableStepCollapsed` | collapsed display of a disabled step (any step) [I] |
| `0x1000000` | `kPauseScript` | "Pause" (Enter Find/Browse/Preview Mode) |
| `0x2000000` | `kRestore` | "Restore" stored spec; for block steps (If, Else, Else If, Loop, Undo/Redo, Open Transaction) the XML label is "Collapsed" (label 303) |
| `0x4000000` | `kGoToField` | go to target field option [I] |
| `0x8000000` | `kFieldSpecByCalc` | Set Field By Name: target field name calculated |
| `0x10000000` | `kVerifySSLCertificates` | "Verify SSL Certificates" |
| `0x20000000` | `kOpenAutomatically` | "Automatically open file" |
| `0x40000000` | `kCreateEmail` | "Create email with file as attachment" |
| `0x80000000` | `kLimitToCurrentFile` | window steps: current file only |

`StateFlags` (runtime only, never stored): `kDirty` 1, `kMacCompatible` 2,
`kWinCompatible` 4, `kServerCompatible` 8, `kIOSCompatible` 0x10,
`kCustomWebCompatible` 0x20, `kInstantWebCompatible` 0x40,
`kNotButtonCompatible` 0x80, `kRunFromCommand` 0x100, `kDeleteOutputFile`
0x200 [O]. Compatibility semantics: script-runtime.md.

## 3. The step value

### 3.1 Cached control-flow indexes [O, 393/393 in the corpus]

Indexes are 0-based positions in the script's step list.

| step | `mStepValue` | extra |
|---|---|---|
| If (68), Else If (125), Else (69) | the next Else If / Else / End If at the same nesting level | Else If also stores the matching End If index as numeric data `[00 0]/03` (77/77) |
| End If (70) | 0 | |
| Loop (71) | the matching End Loop | |
| Exit Loop If (72) | the End Loop of the innermost enclosing Loop | |
| End Loop (73) | the matching Loop | |
| Open Transaction (205) | the matching Commit Transaction | (script-runtime.md 8.2; not in the corpus) |
| Go to Record (16), Go to Portal Row (99) | list choice (3.2) | numeric data `[00 0]/03`: the enclosing End Loop for "Exit after last", or -1 (`ffffffff` in every corpus step) |

The editor recomputes these when steps move; a writer must recompute them for
every block step after any insert, delete or move [I]. The runtime trusts
them and never searches for the partner step; a stale index silently jumps to
the wrong place or ends the script (script-runtime.md 3.1).

### 3.2 List choices

Steps whose schema says `value` keep a list choice in `mStepValue` with
`kMenuSelection` (0x2) set [O for Go to Record, Go to Portal Row, Show/Hide
Toolbars, Scroll Window, Set Zoom Level, Find Matching Records, Close/Select
Window]. Enumerations come from the `ParameterListFactory::Append*List`
helpers (item label resource id, value) [O]:

| list | values |
|---|---|
| Go to Record/Request/Page, Go to Portal Row (`GetGotoValue`) | 1 First, 2 Last, 3 Previous, 4 Next, 5 by calculation (slot 0), 6 by stored number |
| View As | 1 Form, 2 List, 3 Table, 4 Cycle |
| Adjust Window | 1 Resize to Fit, 2 Maximize, 3 Minimize, 4 Restore, 5 Hide |
| Scroll Window | 1 Home, 2 End, 3 Page Up, 4 Page Down, 5 To Selection |
| Show/Hide Toolbars, Text Ruler, Menubar | 1 Show, 2 Hide, 3 Toggle |
| Undo/Redo | 1 Undo, 2 Redo, 3 Toggle |
| Set Zoom Level | 1 25%, 2 50%, 3 75%, 4 100%, 5 150%, 6 200%, 7 300%, 8 400%, 9 Zoom In, 10 Zoom Out |
| Set Multi-User | 1 On, 2 On (Hidden), 3 Off |
| Enable Touch Keyboard | 1 On, 2 Off, 3 Toggle |
| Arrange All Windows | 1 Tile Horizontally, 2 Tile Vertically, 3 Cascade, 4 Bring All To Front |
| Save a Copy as | 1 copy, 2 compacted copy, 3 clone (no records), 4 self-contained copy |
| Sort Records by Field | 1 ascending, 2 descending, 3 value list [I from Claris] |
| Find Matching Records | 1 replace found set, 2 constrain, 3 extend [I from Claris] |
| Save Records as Snapshot Link | 1 records being browsed, 2 current record |
| AVPlayer Set Playback State | 0 Stopped, 1 Paused, 2 Playing |
| Read/Write Data File encoding | 1 UTF-16, 2 UTF-8, 3 Bytes (read only) |
| Loop Flush (26.x; list value at slot 1, read only with `kOption`) | 1 Always, 2 Minimum, 3 Defer (not stored in the corpus; which of 2 and 3 is Minimum is unconfirmed, script-runtime.md) |
| PSOS with Callback "current script" | 1 Halt, 2 Exit, 3 Resume, 4 Pause |

Adjust Window [Resize to Fit] resizes the window "to the minimum size
possible while keeping all items in the layout visible". On a card it is
the only option available, and on macOS the step has no effect on a
full-screen window and returns an error (Claris, Adjust Window;
documented). Not measured:
- what counts as all items: the layout's width, its parts' height, objects
  beyond the layout's width;
- how the zoom and the status toolbar take part;
- how the screen limits the size;
- where the window's top left ends up.

OpenMaker fits the content area to the layout's width and the bottom of its
lowest part at the window's zoom, with the toolbars and menu bar as the
script left them. It keeps the top left and keeps the frame within the
desktop area: within its size, for a window on a monitor the desktop area
does not describe. Run before the window's content area was first drawn,
or with a bar shown that was never drawn, the fit waits until it is, with
the layout, zoom and bars as the step saw them; a later step that sizes or
maximizes the window replaces it [I].

Set Allowed Orientations (176) uses `mStepValue` as a bit set: 1 Portrait,
2 Portrait Upside Down, 4 Landscape Left, 8 Landscape Right
(`AppendBoolean_SetStepValueVariant`) [O]. The id is in
`ScriptStep::IsStepIDObsolete`'s list in this build (script-runtime.md 2.5).

Go to Layout stores 5 when a layout key is in `mReserved` (all 160 corpus
steps) [O]; the codes for "original layout" and "by name/number" are not in
the corpus [Low].

## 4. Parameter storage

### 4.1 Inline references in `mReserved`

| flag | content | layout |
|---|---|---|
| `kFieldSpec` 0x1 | `DBFieldSpec` (14 bytes) | `mTableKey` `HBKey<5>` (table occurrence, e.g. `04 03 d0 00 05`), `mFieldKey` `HBKey<5>` (`02 01 05`), `mRepetition` i16 BE [O] |
| `kMemberKey` 0x8 | `HBKey<5>` of a layout or menu set | Go to Layout, GTRR, New Window, Install Menu Set (`KStandardMenuSet` = standard menus) [O] |
| `kScriptKey` 0x40 | `HBKey<5>` of a script | Perform Script, PSOS, Install OnTimer Script [O] |
| (privilege) | `HBSerialKey<7>` of a privilege set | Add Account (`SetPrivilegeKey`) [I] |

When the flag is clear the bytes keep stale content (Go to Field without a
target keeps an old field spec) [O]; readers must test the flag first.

**Variable targets.** A target that is a variable is a field spec whose table
key is `HBAbstractKey(0)` and field key `HBAbstractKey(0x20)`
(`VariableStore::kTargetVariableTableKey`/`kTargetVariableFieldKey`, compared by
`ScriptStep::IsVariable` and `TargetHelper`) [O]; the variable name is text
data and its repetition a calculation (`ScriptStep::GetVariableString` reads
text, then calculation slot 2 when `kCalculation` is set) [O shape, Med slots].

### 4.2 The step data subtree

`[17, 5, S, 5, K]` where K is the segment of `mDataKey`.
`ScriptStep::MakeDataKey` allocates K as the next serial key in `[17, 5, S, 5]`
(`DBCatalogItemList::MakeDataKey` -> `HBAbstractKey::GenerateSerialKey`) [O].
Steps without data still get a key and UUIDs.

Slot nodes are two-byte segments `[00 nn]` (`HBInt16Key(slot)`) [O]:

| path | constant | kind | written by |
|---|---|---|---|
| `[00 n]` (value on the slot node) | | list value, u32 BE | `SetListValue(slot, v)` |
| `[00 n, 01]` | `KValue` | text, masked SCSU (chunked when long) | `SetTextData(slot, text)`; numbers via `SetTextData(slot, long)` as decimal text |
| `[00 n, 03]` | `KUnique` | numeric data, u32 BE (bool = 0/1) | `SetNumericData<T>(slot, v)` |
| `[00 n, 05]` | `KFormula` | calculation: children `04` context, `05` formatted stream, `06` unformatted stream (calc-encoding.md) | `SetCalculation(slot, ...)` |
| `[00 n, 10]` | `KName` | `DBFieldSpec`, 14 bytes, for slot n > 0 | `SetFieldSpec(slot, spec)` (slot 0 goes to `mReserved`) |
| `[00 n, 11]` | `KAliases` | table occurrence keys (with `04` `KCount` when several) | `SetTableAliasKey(s)` |
| `[00 n, 82]` | `KPathString` | path list for slot n | `SetUniversalPathList(slot)` |

Step-level keys [O]:

| key | constant | content | flag |
|---|---|---|---|
| `06` | `KRequests` | find requests (`DBRequestSet`, section 11.2) | `kExtendedData` |
| `0a` | `KSortOrder` | `DBSortSpec` stream (section 11.1) | `kExtendedData` |
| `20` | `KFileReferences` | external data source id (section 11.8) | `kFileReference` |
| `82` | `KPathString` | path list text (`file:`, `filemac:`, `filewin:` lines) [I] | `kFilePath` |
| `83` | `KBlob` | binary blob: compiled AppleScript, plug-in step data (section 11.8) | set by `SetBlob` together with 0x20000 |
| `84` | `KImportOrder` | `FMImportOptions` (section 11.3) | `kExtendedData` |
| `85` | `KExportOrder` | `FMExportOptions` (section 11.4) | `kExtendedData` |
| `86` | `KProfile` | `FMTranslationProfile`, the import/export file type (section 11.5) | `kFilePath` tested by `GetProfile` |
| `87` | `KPageFormat` / `KImportProfileEx` | Print Setup, Print: page format record (11.6); Import Records, Convert File: profile extension (11.5) | `kExtendedData` |
| `88` | `KPrintSettings` | print settings record (11.6), one value (chunked; its `00` child is the chunk header, not a kind) | `kExtendedData` |
| `8c` | `KFileFilterSpec` | Insert File filters (section 11.8) | `kExtendedData` |
| `f0` | `KFlags` | Import Records: per-field import flags (section 11.3) | |
| `d8`, `d9` | `KUniqueID`, `KSourceUUID` | 16-byte UUIDs of the step | always |
| `d7`, `da` | `KOwnerID`, `KTags` | owner, tags (`DBCatalogItemList` API) | not seen |

Section 11 specifies every one of these structures; a step editor that does
not change a spec keeps it byte for byte.

### 4.3 Calculation slots

A calculation parameter is `[00 n, 05]`; it exists only with `kCalculation`
set [O]. Slot numbers per step are in the CSV. `DBDataType` in the XML writer
gives the expected result type: 1 text, 2 number, 7 boolean (If, Else If,
Exit Loop If, Revert Transaction condition) [O]. Calculation text in a
parameter is evaluated in the context of the current window's layout table
occurrence [I; script-runtime.md].

### 4.4 Conventions used by many steps

| XML parameter | storage |
|---|---|
| "With dialog" Boolean | inverse of `kNoInteract` 0x80 |
| "Select entire contents" | `kSelectAll` 0x1000 |
| FieldReference (Specify field) | `kFieldSpec` + `mReserved` |
| Target (field or variable) | as FieldReference; variable as in 4.1 |
| File list (`SerializeFileNames(slot 0)`) | `kFilePath` + `82` path list, `20` references |
| Restore | `kRestore` 0x2000000 |
| On/Off steps (Allow User Abort, Set Error Capture, Set Use System Formats, Allow Formatting Bar, Set Layout Object Animation, Set Revert Transaction on Error) | On = `kTurnOn` 0x20000; `kSetOnOff` 0x10000 also read, seen with On (0x30000) in FMServer_Sample [O] |
| Script reference stored "with position" (callback scripts) | the script key as a calculation in a slot whose formula is a quoted text literal of the key segment in hex (`"08"`), with a flag bit marking it present (`SetScriptKeyWithStorePosition`; Perform Script on Server with Callback: slot 3, `0x100`, section 12.2) [O] |

## 5. Schemas

[catalogs/script-step-params.csv](catalogs/script-step-params.csv): one row
per parameter (571 rows, 221 named step ids), columns:

- `step_id`, `step_name`: catalogs/script-steps.csv.
- `order`: XML element order (the order `Deserialize*` reads them); 0 = the
  step has no parameters.
- `parameter`: label (DBEngine resource 129) or XML tag.
- `xml_type`: the V2 XML `<Parameter type=...>` kind: Boolean, Calculation,
  Text, List, FieldReference, Target, ScriptReference, LayoutReference,
  FindRequests, SortSpecification, ... (compound kinds keep the serializer name).
- `storage`: machine-readable, `|`-separated: `flag:0xNN` (bit set = true),
  `flag:!0xNN` (bit clear = true), `value` (`mStepValue`), `reserved:field|layout|script|key`,
  `slot:N:calc|text|num|list|field|aliases|path`, `key:XX`.
- `values`, `default`, `notes` (Low/Med marks and open points).
- `evidence`: `xml+params` (the XML dispatchers and `GenerateParameters`),
  or `xml+params+oracle` where FileMaker Server's XML importer also stored the
  parameter at that location and its Save as XML wrote it back (section 12).

49 named ids have no parameters (absent from both XML dispatchers and from
`GenerateParameters`): 4 5 7 8 19 20 21 23 24 25 27 32 38 44 50 59 70 73 78 79
82 88 90 93 98 101 102 105-109 112 113 114 118 129 133 140 149 151 156 165 169
172 183 206 237 247. Step 186 (unnamed in the string table) is the plug-in
step: `ScriptStep::GetExternalInfo`/`GetPlugInID` give the plug-in's QuadChar
id and step id, parameters are calculations (`ExternalPluginOptions`) [O
shape, Low detail].

Where the CSV says "Low" or "slot map not recovered", the parameter names and
bits are certain (XML dispatcher and `GenerateParameters`) but the slot
number inside a compound element is not; this is 23 step ids, mostly the
mobile (161, 185, 187, 201, 202), AVPlayer (177, 179), window option (119,
122) and account steps (134-138). A writer that only round-trips stored steps
is unaffected (section 6).

### 5.1 Selected schemas in prose

**Perform Script (1).** Script key in `mReserved` with `kScriptKey`; parameter
calculation slot 0 [O]. Calculated script name: `kCalculation` with the name
in another slot [Low; slot 1 assumed]. External scripts: the file reference
to the other file (`20`/`kFileReference`) [I].

**Set Variable (141).** Name text slot 0 (`$x`/`$$x`, with `kTextData`),
value calculation slot 1, repetition calculation slot 2 (absent = 1) [O
215/215 for name and value].

**Set Field (76).** Field in `mReserved`; value calculation slot 0;
repetition calculation slot 1 when the target repetition is calculated [O
name/value]. **Set Field By Name (147):** value slot 0, field-name
calculation slot 1 with `kFieldSpecByCalc` [O].

**Show Custom Dialog (87).** Title calculation slot 0, message slot 1;
button labels text slots 0-2 (Default Button, Button 2, Button 3) with their
"Commit data" flags as one-byte numeric data `[00 3..5, 03]`; input field 1
in `mReserved`, inputs 2-3 at `[00 1, 10]`, `[00 2, 10]`; input labels
calculations slots 2-4; "use password character" numeric data `[00 0..2,
03]`; 26.x window geometry calculations: height 8, width 9, top 11, left 12
[O 28/28 leaves; the commit and password slots against XML, "Show Custom
Dialog (87) inputs" below]. A new step from FileMaker Pro 26.0.3's Script
Workspace stores "OK" committing (`[00 3, 03]` = 1) and "Cancel" not
(`[00 4, 03]` = 0) [O: a Pro-made twin, #221].

**Go to Related Record (74).** Table occurrence at `[00 0, 11]`; layout key in
`mReserved` (`kMemberKey`) or by calculation; option word `[00 0, 03]` (e.g.
`000f0202`, `80010300`) plus `kOption`/`kOption2`/`kRestore` bits; new-window
calculations in slots 1-5 [O storage, Med semantics].

**Sort Records (39)**, **Perform Find (28)**, **Enter Find Mode (22)**,
**Constrain/Extend (126/127)**: `kRestore` + `kExtendedData` + `0a`
(`DBSortSpec`) or `06` (`DBRequestSet`) [O].

**Print (43)** / **Print Setup (42)**: `88`/`87` [O].

**Revert Transaction (207).** Condition Boolean `kOption` enables calculation
slot 0 (boolean); "Error code" Boolean `kOption2` enables slots 1 (number)
and 2 (message) [O].

**Open Transaction (205).** "Skip auto-enter options" `kSelectAll` 0x1000,
"Skip data entry validation" `kOption`, "Override ESS locking conflicts"
`kOption2`, Collapsed `kRestore` [O].

**Commit Records/Requests (75).** "Skip data entry validation" `kOption`,
With dialog, "Override ESS locking conflicts" `kOption2` [O].

**Data file steps (188-199).** File ID calculation slot 0; path list
(`kFilePath`, `82`); target field/variable; encoding in `mStepValue`;
"Append line feed" `kOption2`; Read amount slot 1; Set position slot 1;
Rename new name slot 0 [O dispatcher].

**Insert from URL (160).** Verify SSL, Select, With dialog, target, URL
calculation slot 0, cURL options slot 1; "Do not automatically encode URL"
[Low bit]. **Execute URL (224)** stores the same URL, cURL options, Verify
SSL and encode bit and adds a callback script: its key in `mReserved`
(`kScriptKey`) and its parameter calculation in slot 2 (section 12.2).

## 6. Decode, edit, re-encode

- Keep the 28-byte descriptors and every leaf of `[17, 5, S, 5, K]` exactly;
  unknown keys and stale `mReserved` bytes included (FileMaker preserves them).
- New step: allocate K as the next serial in `[17, 5, S, 5]`, write `d8`/`d9`
  UUIDs, set `mDataKey = [len][01 K]` (pad to 6 bytes; FileMaker writes `be`
  fill when the key is fresh).
- Changing a parameter: set or clear the flag bit together with the data
  (`SetTextData` clears `kTextData` when the text is empty; `SetFileReferenceKey`
  clears `kFileReference` when the key is null; `SetCalculation`/`ClearCalculation`
  maintain `kCalculation`) [O].
- After inserting, deleting, moving or disabling steps, recompute section 3.1
  indexes for every block step. Disabling only negates `mStepID` [O].
- Writing the catalog to a hosted file needs the catalog edit protocol of
  schema-path.md (locks, op86 UploadMultiple, op13 index) [I; not captured for
  scripts].

## 7. Implementation tiers

Which clients run each step is a binary table, not a per-step method:
`sStepsUnstoredFlagsTable` (libFMEngine 0x10ff900, 248 u32 `StateFlags`
values: 2 Mac, 4 Windows, 8 Server, 0x10 Go, 0x20 Data API/CWP, 0x40
WebDirect, 0x80 not a single-step button) [O, re-read for this document:
identical to [catalogs/script-step-compat.csv](catalogs/script-step-compat.csv)
for all ids]. 84 named steps lack the Server bit; on the server they are
skipped with error 3 and the script continues (script-runtime.md 10).
Obsolete ids (error 4): 15, 52, 54, 58, 59, 78, 100, 153, 176, 198, 204;
Execute URL (224) and Validate Scripts (239) are feature-gated off in 26.0.3
and also fail with error 4, so they can wait.

| tier | steps |
|---|---|
| 1: control flow, variables, errors, navigation, records, fields | 1 Perform Script, 68-73 and 125 control, 72, 85, 86, 89, 90, 103, 141, 200, 205-207, 223, 6 Go to Layout, 16, 17, 99, 145, 4, 5, 22 Enter Find Mode, 55, 7-10, 51, 75, 104, 133, 182, 76, 147, 77, 13, 14, 60, 61, 91, 116, 130, 11, 12, 40, 50, 46-49 (clipboard through the host), 45, 80, 79, 167, 180, 208 |
| 2: finds, sorts, found sets, windows, dialogs, files, data files, import/export, URL | 21, 23-28, 39, 126-128, 150, 154, 155, 228, 29-31, 81, 92, 97, 119-124, 166, 169, 87 Show Custom Dialog, 62 Pause/Resume, 83 Change Password, 134-138 accounts, 33, 34, 37, 3, 35, 36, 139, 143, 144, 152, 225, 56, 131, 132, 158, 159, 188-199, 160 Insert from URL, 224 Execute URL, 111 Open URL, 146, 175, 237 web viewer, 117 Execute SQL, 203 Data API, 164, 210 PSOS, 148 OnTimer, 238 persistent data, 142 menu sets, 115 formatting bar, 168, 63 Send Mail, 84, 94, 102, 44, 211 |
| 3: platform-bound | printing and PDF (41 Preview, 42, 43, 242-247), spelling (18-20, 106-109, 209), mobile/iOS (161, 174, 176, 185, 187, 201), AV (59, 177-179), OS integration (57 Send Event, 64 DDE, 67 AppleScript, 65 Dial Phone, 66 Speak, 93 Beep, 181 Get Folder Path, 157 Install Plug-In, 186 plug-in steps), AI (202, 212-222, 226, 227, 239-241), Open Menu Item dialogs (32, 38, 88, 105, 112-114, 118, 129, 140, 149, 151, 156, 165, 172, 183), 82, 95, 96, 139 conversions |

### 7.1 Steps that need a host capability

A `ScriptHost` trait should expose these instead of the runtime touching the
platform:

| capability | steps |
|---|---|
| windows and layouts (open, close, select, move, title, mode, view, zoom, toolbars, refresh, scroll, object focus/refresh) | 6, 22, 29-31, 41, 55, 79-81, 92, 97, 119-124, 145, 166, 167, 169, 174, 176, 180, 228 |
| record/found-set access through the data layer | 7-10, 16, 21, 23-28, 39, 40, 51, 74-77, 91, 99, 104, 116, 126-128, 133, 147, 150, 154, 155, 182, 205-207 |
| field editing and selection, clipboard | 4, 5, 11-14, 17, 45-50, 60, 61, 98, 101, 130 |
| user dialogs (custom dialog, find/replace, sort, import order, print, file pickers) and abort | 62, 83, 87, 128, and every step whose "With dialog" is On |
| local file system (paths, data files, pickers) | 3, 33, 35-37, 56, 131, 132, 143, 144, 152, 158, 159, 181, 188-199, 209, 225, 242-246 |
| network (HTTP, SMTP, Claris Connect, Data API, ODBC) | 63, 111, 117, 160, 203, 211, 224 |
| server calls (PSOS) | 164, 210 |
| accounts and privileges | 83, 134-138 |
| printing/PDF rendering | 41-43, 144, 242-247 |
| web viewer JavaScript | 146, 175, 237 |
| OS integration (Apple events, DDE, speech, phone, beep, plug-ins, menu sets) | 57, 64-67, 93, 142, 157, 186 |
| devices and media (camera, NFC, location, notifications, AV) | 161, 177-179, 185, 187, 201 |
| AI providers | 202, 212-222, 226, 227, 239-241 |
| timers | 148 (Install OnTimer Script) |

## 8. Verification tool

```
cargo run --release -p om-scripts --example step_params -- <file.fmp12>...
```

Decodes every script catalog, classifies each step's flag bits, step value
and data leaves against the CSV (plus section 10's corpus answers), checks
the section 3.1 indexes and that recomputing them changes nothing,
re-encodes every definition and compares it with the stored nodes, and
prints per step id `steps typed disabled leaves_ok/leaves flags_ok/flags`
plus every unclassified item. Result on the 73 local files (copies):

```
definitions 638 {"Folder": 67, "Script": 504, "Separator": 67}; decode errors 0
re-encode: definitions exact 638/638, step lists exact 504
block indexes: step values 593 match, 0 differ; numeric indexes 222 match, 0 differ
total: 47 step ids, 2638 steps, fully typed 2638; leaves 5653/5653, flag bits 2782/2782
```

Opcodes in the corpus: 1 6 7 9 16 17 22 23 26 27 28 29 32 39 41 43 50 68 69
70 71 72 73 74 75 76 79 81 85 86 87 88 89 90 97 99 103 104 121 122 123 125
131 141 145 150 155. The check proves location, not meaning: for the 174
named ids absent from the corpus the CSV rests on the binary alone. Section
10 extends the corpus to 746 files.

## 9. Unknowns

- Slot maps inside compound elements for 23 step ids (CSV notes): AVPlayer,
  window options, Insert File options, web viewer actions, mobile steps,
  Perform AppleScript, the account steps' Booleans. Section 12's method
  (the step's `V2_ScriptStepXML` writer and reader in the Mac `FMEngine`, a
  probe through FileMaker Server's XML importer) applies to them unchanged.
- Section 12's own open points: the meaning of Perform Find by Natural
  Language's `Target` calculation (slot 6) and of Configure Prompt
  Template's step value; the slots of the web script parameters of
  Generate Response from Model and Perform SQL Query by Natural Language
  (`[00 19, 03]` is their count); which `mStepValue` hides Configure
  Regression Model's training fields; the names of Create PDF's Edit 2,
  Layout 2 and the other magnifications.
- Go to Layout and GTRR `mStepValue` codes other than 5; GTRR option word.
- Loop Flush codes 2 and 3 (Minimum vs Defer).
- `f0` script flag bits; `[17, 5, S, 13]`.
- The stored specs (`06`, `0a`, `84`-`88`, `8c`, `f0`) are section 11; its
  own unknowns are 11.10.
- `kSetOnOff` semantics (written with On by FMServer_Sample's writer).
- Insert File `kOption2` (set on all 6 samples).
- Perform Script calculated-name slot; external-file script references.
- Whether FileMaker Pro's own editors (Script Workspace, clipboard
  `fmxmlsnippet` paste) store what FileMaker Server's importer stores for the
  steps of section 12. The readers and writers are the shared `FMEngine`
  code, so the same slots are expected [I]; a file FileMaker Pro wrote with
  one instance of each step, checked with `step_params`, would settle it.

## 10. Corpus answers (implementation)

Observed in sample bytes while building `om-scripts`' typed model (no
binary work). Corpus: 746 files (66 templates, 6 FMServer_Sample, test
project, 673 third-party example files), 3,639 definitions (3,404 scripts,
74 folders, 161 separators), 55,646 steps of 200 step ids. Every definition
re-encodes byte for byte; 55,645 steps are fully typed (one Read from Data
File carries an unexplained `0x200`). The locations below are in
`om-scripts` (`schema.rs`, `CORPUS`) on top of the CSV; "meaning not
mapped" marks a location whose purpose the bytes do not show.

Of the 54 step ids whose CSV rows were Low or unmapped, 30 occur in the
corpus and every stored leaf of theirs has a location; the other 24 (PSOS
with Callback 210, the AI steps 212-222 and 226-227, Execute URL 224, Save
Records as JSONL 225, Go to List of Records 228, Configure Persistent Data
238, 239-241, the PDF build steps 243-246) do not occur, and section 12 maps
them from their XML readers and writers and an importer probe.

**Descriptors and subtrees.**

- A layout button's single-step protobuf descriptor does not contain its
  parameters. They live in the owning object's attribute 17 subtree,
  including for grouped buttons. Keep that subtree with the descriptor
  and decode it with the same `StepData` slots as catalog scripts; for
  example, calculated Go to Layout uses calculation slot 5. Dropping it
  leaves a valid step with a missing target (error 105 for Go to Layout).
- Data keys have length 2 (one-byte segment, 41,187), 3 (two-byte serial
  segment, 7,832) or 0 (no data subtree, 6,627, mostly End If, Else, Loop
  and comments of converted files). The filler varies in all three; `00 be
  be` follows 938 one-byte keys. Inline member and script keys vary the
  same way (314 Go to Layout steps and 813 Perform Script layout buttons
  have `00` filler).
- 6,152 step data subtrees are named by no step's data key; 167 steps name
  a subtree that does not exist. Both are kept as found.
- Empty slot pushes occur (`[00 0]` in Insert Text, `[00 0a]` in Set Field).
- The step list is plain up to 1,000 bytes and chunked above (4-byte
  header, 1,000-byte pieces). Calculation streams follow the same rule
  (48,505 of 48,505 rebuild from their streams). Text is plain when its
  masked SCSU has one byte per UTF-16 unit and fits 1,000 bytes; otherwise
  the header holds the byte length and the UTF-16 length, plus a third word
  `4` above 1,000 bytes. Five short ASCII texts carry a bare 4-byte header
  (another writer).

**Cached indexes** (section 3.1; 10,285 step values and 1,082 numeric
indexes, none differ).

- Only enabled steps take part. A disabled If, Else or End If is not a
  block boundary, and disabled block steps keep a stale value (0, their own
  index + 1, or an old index): the 24 mismatches the earlier check reported
  were all such steps.
- Commit Transaction stores its own index, the same value its Open
  Transaction stores (7/7).
- Go to Record/Request/Page and Go to Portal Row store at `[00 0, 03]` the
  index of the End Loop of the innermost enclosing enabled Loop whether or
  not "Exit after last" is set and whatever the record choice, and -1
  outside a loop (all 474 enabled steps of 475).
- No enabled step structure in the corpus is unbalanced.

**Parameter locations.**

| step | observed |
|---|---|
| any "File paths" | the path list is `[00 0, 82]` (192 steps); a step-level `82` never occurs |
| Target parameters (13, 14, 60, 61, 77, 131, 160, 188, 189, 191-194, 203, 186), Show Custom Dialog inputs | a variable target's name is text in slot `0x20 + n` for target n, with the variable field spec (140 leaves) |
| Perform Script (1) | by name: `[00 1]` = 1 (0 = from list, 18 steps), the name calculation in slot 2, with `0x800` and `0x2000000` (10/10); script in another file: file reference under step-level `20` with `kFileReference` (7); value 4 with `kMenuSelection` on 4 steps, meaning not mapped |
| Go to Layout (6) | `mStepValue` 1 original layout (49), 3 name by calculation (30), 4 number by calculation (4), 5 stored key (497); the calculation is slot 5 |
| Go to Field (17), Set Field (76) | a calculated repetition of the inline target (spec repetition 0) is the calculation in slot 10, not slot 1 |
| Open File, Close File (33, 34) | the data source is a file reference under `20` with `kFileReference` |
| Import Records (35) | `mStepValue` 1, 2 or 4 with `kMenuSelection` (likely the three import actions, Low); `87` (`KImportProfileEx`) and `f0`; calculations in slots 0 and 1 |
| Export Records, Save a Copy as, Save Records as PDF, Snapshot Link (36, 37, 144, 152) | `kOpenAutomatically` and `kCreateEmail` |
| Show Custom Dialog (87) | input labels are the calculations in slots 2-4; slots 5-7 hold calculated button labels beside the text labels; numeric slots 3-5 are always stored (slot 3 = 1 in 1,224 of 1,391, OK commits) and 0-2 only with input fields (831/831), so 3-5 are the commit flags and 0-2 the password flags, the CSV's assignment swapped (Med) |
| Replace Field Contents (91) | `[00 1, 03]` 1 current contents, 2 serial numbers, 3 calculated result (111 calculated = 111 with a calculation); `[00 2, 03]` increment, `[00 3, 01]` initial value, `[00 0, 11]` = `01 00`, `[00 4-6, 03]` options (meanings in 11.8) |
| Insert File (131) | `8c` (`KFileFilterSpec`, with `kExtendedData`), a calculation in slot 0, `kStoreReference` |
| Read from Data File, Set Data File Position (193, 195) | `kExtendedData` accompanies the slot 1 calculation |
| Get Folder Path (181) | a calculation in slot 2 beside the target variable (its repetition, as Set Variable) |
| Perform JavaScript in Web Viewer (175) | `[00 0, 03]` = the parameter count; parameter i is the calculation in slot 10 + i (17 parameters reach slot 26) |
| Go to Related Record (74) | animation in slot 6, as Go to Layout |
| New Window (122) | a selected layout uses step value 5 without `kMenuSelection` (35/41 corpus steps with a layout; native XML); the layout key alone at value 0 does not select it |
| New Window style word (`[00 0, 03]`, also Go to Related Record's new window) | the low byte is the style: `02` Document, `04` Dialog (native XML), `10` Card (the only other word in Claris's 66 starter-solution files: `C0100010`, Tasks' "Add Related Record" opening the 668 x 584 Assignee Chooser, 11 languages). Options sit above it: FileMaker's default Document `00370602` has seven option bits (Close, Minimize, Maximize, Resize, Menu Bar, Toolbars, Dim parent window), a harness Document with only the four title-bar options `000F0202`, the Dialog `80050204` lacks Minimize (`0002_0000`), and the card has only `0010_0000` (Dim parent window: the one bit set in the card and the default and clear without Dim). Close, Maximize and Resize (`0200`, `0001_0000`, `0004_0000`) and Menu Bar and Toolbars (`0400`, `0020_0000`) are not told apart; the top bits (`80` dialog, `C0` card) are not mapped. Floating Document's word is not measured (Med) |
| Perform Script on Server (164) | canonical by-name selection is `[00 1]` = 1 and a name calculation in slot 2 with `0x800` and `0x2000000`, as Perform Script (native XML); the reader retains its older slot-1 compatibility form |
| Truncate Table (182) | the base table key (two-byte id from `0x8000`) in `mReserved` with `kMemberKey`, step value 1 and `kMenuSelection` (42/42); value 0 denotes the current table |
| plug-in step (186) | `mStepValue` = function id, `83` = the plug-in's QuadChar id (`MBSP`) and bytes, one calculation per parameter in slots 0-9, a field or variable target, `0x100`, `0x200` and `0x1000` on all 5 steps |
| Send Event, Send Mail, Speak, Perform AppleScript, New Window, Perform Find/Replace, Save Records as Excel, Set Web Viewer, Insert from Device, AVPlayer Play/Set Options, Configure Region Monitor Script, Local Notification, NFC Reading, Machine Learning Model | further numeric, list and calculation slots and flag bits of the compound element (listed in `CORPUS`); Send Event's numbers are four-character codes (`aevt`, `odoc`, target signatures), Speak's voice too; AppleScript stores a compiled blob under `83`; Send Event, Send Mail, Speak and Perform Find/Replace are mapped in section 12.3, the others' meanings are not mapped |
| Paste, Dial Phone, Add/Reset/Enable Account, Convert File | a `kMenuSelection` step value (0, 1 or 4), meaning not mapped |

## 11. Stored specs inside steps

Sections 4-5 locate every stored parameter; this section specifies the
structures that the step codec keeps as opaque nodes: sort orders, find
requests, import and export options, translation profiles, print records,
Insert File filters, external file references, and the compound slot maps
of Go to Related Record, Replace Field Contents, Insert from URL, Show
Custom Dialog and Save Records as PDF. Everything here is enough to decode,
edit, re-encode byte for byte and execute these specs.

Evidence, in addition to section 0:

| source | what it gave |
|---|---|
| `libDBEngine.so` `DBSortSpec::Read`/`Write` (0x102fa10, 0x102fbf0), `DBSortSpec::Entry` DWARF, `DBRequestSet::SaveSet` (0xf65370), `DBRequestCursor::GetOmitState`/`PutData` (0xf68440, 0xf66120), `DBFieldSpec::GetFieldSpecKey` (0xee0890), `DBFind::Find`/`Constrain`/`Extend` (0xf79480, 0xf79a60, 0xf79d80), `DBFindRow::Find` (0xf78170), `DBRequestSet::ExpandVariables` (0xf65550), `DBCalcScan::ExpandVariables` (0xe35e30), `DBSortSpec::CanSortOnHost` (0x1030ff0) | layouts and semantics (addresses are `nm` values) |
| `libFMEngine.so` `FMImportOptions::Write`/`WriteImportFlags` (0xe0b070, 0xe0b2d0), `FMExportOptions::Write`/`Read` (0xe0a690, 0xe0a0c0), `FMTranslationProfile::Write` and the subclass writers (0xe08e10, 0xe08ee0, 0xe0dd40, 0xe10940, 0xe10fb0, 0xe12280, 0xe162f0, 0xe18260), DWARF `TImportFlags`, `TImportAction`, `FMImportFieldMapEntry`, `TExpCharacterSet`, `FMTranslationProfile` | import/export layouts and enumerations |
| `libSupport.so` `SwapOStream::InternalSCSUCompress` (0x1f3e60), `UniversalPathListToStream` (0x260640), DWARF `Locale::Type` | stream strings, path lists, language ids |
| FileMaker Server's FMDeveloperTool `--saveAsXML` of 143 corpus files (FMSaveAsXML 2.3) | the meaning of every stored value, compared mechanically (11.9) |
| FileMaker Server's script engine (`tools/fms-script-harness` of the script oracle workstream, FMUpgradeTool patches) | 31 probe specs written by FileMaker's own XML importer and executed (11.1.3, 11.2.4) |

Tags as before: **[O]** observed, **[I]** inference; High unless marked.

### 11.0 Common encodings

- **Key** (`HBKey<N>` in a stream): `[u8 n][n bytes]`, the bytes being
  segments `[u8 len][bytes]`. A table occurrence is one 3-byte serial
  segment (`04 03 d0 00 19` = occurrence `d0 00 19`), a field, value list or
  data source one serial segment (`02 01 08`). Ids in FileMaker's XML are
  the serial values (`d0 00 19` = 1065113, file-format.md / om-hbam
  `serial`). "No key" is written two ways: `00` (zero length) and `02 01 00`
  (the segment `00`); FileMaker's XML prints both as id 0 [O]. A writer
  keeps the form it read; FileMaker's XML importer writes `02 01 00` [O].
- **Field spec** in a stream (`DBFieldSpec::Write`): table key, field key,
  u16 BE repetition (1-based) [O].
- **String** (`SwapOStream::InternalSCSUCompress`): a byte count, then that
  many bytes of SCSU text XOR-masked with `0x5a` like all catalog text.
  Count `< 0x80`: one byte; `< 0x4000`: u16 BE with `0x8000` set; `<
  0x10000`: `c2` then u16; else `c4` then u32 [O code; corpus strings are
  all one-byte counts].
- **Path list** (`UniversalPathListToStream`): u32 BE count, then three
  strings per path: its kind (`file`, `filemac`, ...), a volume and the
  path [O]. FileMaker's XML importer stores a line `kind:path` with an
  empty volume [O, oracle]; the Script Workspace writes a list so
  (`om_scripts::paths`).
- **Values** longer than 1000 bytes are chunked as every catalog value
  (section 10, file-format.md 4.3): the stream is the reassembled value. The
  print records (11.6) are chunked; the other specs are short in the corpus
  [O for 87/88, I for the rest].

All integers are big-endian.

### 11.1 Sort order (`0a`, `KSortOrder`)

Steps: Sort Records (39) with `kRestore` and `kExtendedData`. A Sort Records
step without Restore may still carry a stale `0a`. Sort Records by Field
(154) stores no spec: its field is in `mReserved`, the direction in
`mStepValue` (1 ascending, 2 descending, 3 by value list) [O].

#### 11.1.1 Layout [O, 110/110 corpus specs and 11/11 probes re-encode]

`DBSortSpec::Write(SwapOStream&)`, the value of `0a`:

```
u16  flags             DBSortSpec::Flags: 1 kDontMaintainOrder, 2 kSortBlanksLast
u16  n                 number of sort fields, in priority order
n x  entry (DBSortSpec::Entry::Write):
  field spec           the sort field: table occurrence key, field key, u16 repetition
  field spec           mSummarySpec: "reorder based on summary field" (none: keys absent)
  key                  mValueListKey: "custom order based on value list" (none: key `00`)
  u8   order           mSortOrderFlags: bit 0 descending (kSortDirectionMask)
  u32  locale          mLocaleOverride, a Locale::Type; 0 = the field's own language
```

`mAppFlags`, `mSemanticFind` and `mCustomOrder` are in-memory only [O].
Example (Contacts template "Sort by Name", Initial ascending):
`0000 0001 | 0403d00002 020108 0001 | 020100 020100 0001 | 00 | 00 |
00000000`. An absent summary field is two keys holding the segment `00`;
an absent value list is the empty key `00`. Written as `02 01 00`, the
value list reads as id -1 and the entry shows as a custom order
(FileMaker's XML: `ValueListReference id="-1"`) [O: the Contacts and Tasks
templates' stored specs; FileMaker Server's Save as XML]. Tasks' "Sort by
Status" names value list 2: `… | 020102 | 00 | 00000000`.

#### 11.1.2 Meaning and FileMaker's XML [O, 72/72 corpus specs equal]

| stored | XML `SortSpecification` / `Sort` |
|---|---|
| flags bit 0 clear / set | `maintain="True"` / `"False"` ("Keep records in sorted order") |
| flags bit 1 | `blanksLast` |
| value list key present | `Sort type="Custom"` + `ValueListReference id` (order byte 0 in every corpus and probe spec) |
| order bit 0 clear / set, no value list | `type="Ascending"` / `"Descending"` |
| summary spec present | `SummaryField/FieldReference` |
| locale non-zero | `LanguageReference id` = the `Locale::Type` value (25 = German) |

The XML carries no repetition: the corpus and FileMaker's importer always
store 1 [O]. `DBSortSpec::Entry::SortOrder` names 2 `kByValueList` and 4
`kCustomOrder`; neither is stored by any observed writer (the value-list
kind comes from the key) [O]. Order 2 sent to the host sorted ascending
(query-program.md 10.2) [O]. An empty list (n = 0) occurs (1 corpus step);
FileMaker's XML then writes no `SortList` [O].

`Locale::Type` (libSupport `Facets.h`): 0 none, 1 system, 2 Unicode raw
("Unicode"), 3 Unicode standard, 4 catalog, 16 Catalan, 17 Croatian,
18 Czech, 19 Danish, 20 Dutch, 21 English, 22 Finnish, 23 Finnish (FileMaker),
24 French, 25 German, 26 German dictionary, 27 Greek, 28 Hungarian,
29 Icelandic, 30 Italian, 31 Japanese, 32 Norwegian, 33 Polish,
34 Portuguese, 35 Romanian, 36 Russian, 37 Slovak, 38 Slovenian,
39 Spanish, 40 Spanish traditional, 41 Swedish, 42 Swedish (FileMaker),
43 Turkish, 44 Ukrainian, 45 Chinese, 46 Chinese stroke, 47 Hebrew,
48 Hindi, 49 Arabic, 50 Estonian, 51 Lithuanian, 52 Latvian, 53 Serbian,
54 Persian, 55 Bulgarian, 56 Vietnamese, 57 Thai, 58 Greek mixed,
59 Bengali, 60 Telugu, 61 Marathi, 62 Tamil, 63 Gujarati, 64 Kannada,
65 Malayalam, 66 Oriya, 67 Punjabi, 68 Sinhala, 69 Urdu, 70 Divehi,
71 Burmese, 72 Sanskrit, 73 Lao, 74 Khmer, 75 Tibetan, 76 Korean,
77 English international, 65535 invalid [O names; XML spells 2 "Unicode",
20 "Dutch", 21 "English", 24 "French", 25 "German", 30 "Italian",
31 "Japanese", 34 "Portuguese", 39 "Spanish", 41 "Swedish", 45 "Chinese",
76 "Korean"].

#### 11.1.3 Semantics, executed [O, FileMaker Server 26.0.3 script engine]

Eight Tasks records (Task, Status, Priority, Category): alpha/Pending/High/
Home, beta/In Progress/Low/Business, gamma/Completed/Medium/(empty),
"Alpha beta"/In Progress/High/Other, (empty)/Canceled/Low/Home,
delta/Pending/Medium/Business, Äpfel/Postponed/Low/Other,
zeta/Completed/High/Business. Value lists: Status = In Progress, Pending,
Postponed, Canceled, -, Completed, -; Priority = High, Medium, Low; Category
= Business, Home, Other, -. Order of Task after Sort Records [Restore]:

| probe | spec | result |
|---|---|---|
| S01 | Task ascending | (empty), alpha, Alpha beta, Äpfel, beta, delta, gamma, zeta |
| S02 | Task descending | zeta, gamma, delta, beta, Äpfel, Alpha beta, alpha, (empty) |
| S03 | blanksLast, Task ascending | alpha, Alpha beta, Äpfel, beta, delta, gamma, zeta, (empty) |
| S10 | blanksLast, Task descending | as S02 |
| S04 | Status by value list, then Task ascending | Alpha beta, beta, alpha, delta, Äpfel, (empty), gamma, zeta |
| S05 | Priority by value list, then Task descending | zeta, Alpha beta, alpha, gamma, delta, beta, Äpfel, (empty) |
| S11 | Category by value list, then Task ascending | beta, delta, zeta, (empty), alpha, Alpha beta, Äpfel, gamma |
| S06 | Task ascending, Swedish (41) | (empty), alpha, Alpha beta, beta, delta, gamma, zeta, Äpfel |
| S07 | Task ascending, German (25) | (empty), Äpfel, alpha, Alpha beta, beta, delta, gamma, zeta |
| S08 | not maintained; Category ascending, Task descending | gamma, zeta, delta, beta, alpha, (empty), Äpfel, Alpha beta |
| S09 | blanksLast; Category descending, Task ascending | Alpha beta, Äpfel, alpha, (empty), beta, delta, zeta, gamma |

So: empty values sort first ascending and last descending; `blanksLast`
puts them last in both directions and at every level; a value-list order
follows the list's values (dividers ignored) and puts values not in the
list after them; ties fall to the next sort field; the language override
replaces the field's collation (Swedish puts Ä after Z; German, kType_DEU,
sorts Ä as AE). "Keep records in sorted order" only affects later edits.
The summary reorder (`DBSortTree::SummarySort`) orders the groups of a sort
field by the summary field's value per group (Claris documentation; not run:
no summary field in the probe file) [I].

#### 11.1.4 Onto the query program

The stored stream is exactly the `0a` operand of SortList (query-program.md
10.2): a client sends it unchanged (the probe vectors there wrote empty
keys as `00`, scripts store `02 01 00`; both are the same entry)
[O layout, I that the host accepts both forms]. Table keys are table
occurrences; the host accepts an occurrence key in a sort spec
(query-program.md 6.2). `DBSortSpec::CanSortOnHost` is false only for an
empty spec or an entry whose order is `kCustomOrder` (4, an in-memory
explicit value order); value-list, summary and language entries can be
sorted by the host [O code]. Sort fields in a related occurrence sort by
the first related value (Claris) [I].

### 11.2 Find requests (`06`, `KRequests`)

Steps: Enter Find Mode (22, `kPauseScript` = Pause), Perform Find (28),
Constrain Found Set (126, `kOption` 0x100 = "Find without indexes"),
Extend Found Set (127), each with `kRestore` and `kExtendedData`. FileMaker's
XML lists the requests only when Restore is set; steps without it may keep
stale requests (3 corpus steps) [O].

#### 11.2.1 Layout [O, 32/32 corpus and 20/20 probe request sets re-encode]

`DBRequestSet::SaveSet` copies the request row set's two subtrees under the
step's `06`:

```
06
  05          KItems: RID list (query-program.md 5.1) of the request ids, in request order
  06          KRequests: the request rows
    00 = [u8 len][serial]     the highest request id (01 01, 01 02, ...)
    <id>                      one node per request, keyed by its serial id
      00 = (empty)            present = Omit request (DBRequestCursor omit state at KBegin)
      <packed field key> = text   one criterion per field (masked SCSU text, chunked like any text)
      fc = [u8 len][serial]   KModCount, the row's modification count (optional)
```

Observed in all 52 sets: ids are 1..n with no gaps, `00` is n, and the order
list is the singleton `01` for one request and the bitmap run `00 =
0000000e`-style for several. FileMaker's XML importer writes no `fc`; rows
edited in FileMaker Pro carry `fc` 0101-0103 [O]. Requests run in id order
(the RID list is sorted) [O].

**Criterion key** (`DBFieldSpec::GetFieldSpecKey`, one key segment):

```
00                     packed-key marker
[u8 len][bytes]...     the table occurrence key's segments (one 3-byte serial)
[u8 len][field][rep]   the field id as a serial key, followed by the
                       repetition as a serial key when it is not 1
```

`00 03 d0 00 02 01 04` = occurrence `d0 00 02`, field 4, repetition 1;
`00 03 d0 00 02 02 01 02` = field 1 repetition 2; repetition 300 is
`00 03 d0 00 02 03 01 80 ac` [O probes F12, F17]. A field spec whose table
is `KUnknown` gets the unpacked key: the field key's segments with the
repetition appended to the last one, no `00` [O code, not in the corpus].
The occurrence may differ from the layout's (related-field criteria,
probe F16) [O].

Criteria are text exactly as typed, with find operators (`==`, `=`, `<`,
`...`, `!`, `//`, `?`, `@`, `*`, `#`, `~`, `"`) and returns (`1¶2` is
`31 0d 32` masked) left in the text; parsing happens when the find runs
(query-program.md 6.4). `DBRequestCursor::PutData` writes a field whose
definition has flag bit 0 of byte 0x0a set through a record cursor instead
of into the request row [O]; that flag is presumably global storage, so
globals typed in find mode keep their value and are not criteria [I].

#### 11.2.2 FileMaker's XML [O, 11/11 corpus and 20/20 probe sets equal]

`FindRequestSet/FindRequest action="find|omit" index="n"` per request, each
criterion `find criteria="text"` + `FieldReference id repetition` +
`TableOccurrenceReference id`. The element is `find` for omit requests too.

#### 11.2.3 Variables in criteria [O]

When a stored request set is restored into the window
(`DataEntryModel::SetupRequestSet`), `DBRequestSet::ExpandVariables` runs
every criterion through `DBCalcScan::ExpandVariables`: each `$name` /
`$$name` token (the calculation scanner's variable rule,
`DBCalcScan::IsVariable`) is replaced by the variable's value as text,
`$$` from the global map and `$` from the current script's; a variable that
does not exist is left as written; replaced text is not rescanned. The
expanded text is what the find parses (probes F07 `==$s`, F08 `$$g`, F14
`$nosuch` finds nothing: error 401). The stored text keeps the variable
names (`< $TODAY` in FMServer_Sample, `==$ID` in the DynaPDF Form example).

#### 11.2.4 Evaluation [O code `DBFind::Find`, `DBFindRow::Find`; executed]

With M the master list (all records of the layout's table for Perform Find
and Extend; the current found set for Constrain) and requests r1..rn in
order:

```
R = r1 is omit ? M - matches(r1) : matches(r1) within M
for each later request r:
    r is omit ? R = R - matches(r)            (omit from what is found so far)
              : R = R + (matches(r) within M)
Perform Find, Enter Find Mode + Perform Find: found set = R
Constrain:  found set = R (computed with M = found set)
Extend:     found set = found set + R (computed with M = all records)
```

Within a request every criterion must match (AND). Record access privileges
filter the result. No requests is error 400; an empty result is error 401.
Executed on FileMaker Server (found set in record order, `Get(LastError)`):

| probe | requests | found |
|---|---|---|
| F01 | find Status "In Progress" | beta, Alpha beta |
| F02 | find Status "Completed"; find Priority "Low" | beta, gamma, (empty), Äpfel, zeta |
| F03 | find Status "In Progress"; omit Priority "High" | beta |
| F04 | omit Status "In Progress" | the six others |
| F05 | omit Priority "High"; find Status "Completed" | beta, gamma, (empty), delta, Äpfel, zeta |
| F06 | find Status "Pending"; omit Task "alpha"; find Task "alpha" | alpha, Alpha beta, delta |
| F07 | `$s` = "Completed"; find Status "==$s" | gamma, zeta |
| F08 | `$$g` = "Low"; find Priority "$$g" | beta, (empty), Äpfel |
| F09 | find Priority "High", then Constrain [find Status "In Progress"] | Alpha beta |
| F10 | find Priority "High", then Extend [find Status "Completed"] | alpha, gamma, Alpha beta, zeta |
| F11 | find Priority "High", then Constrain [omit Task "alpha"] | zeta |
| F12 | find Task[2] "alpha" (field not repeating) | alpha, Alpha beta |
| F13 | find Task "alpha" and Status "Pending" | alpha |
| F14 | find Status "$nosuch" | none, error 401 |
| F15 | find Task "=" | (empty) |
| F16 | find Assignees::First Name "x" and Task "alpha" (no related records) | none, error 401 |
| F17 | find Priority "High" and Task[300] "b"; omit Category "Home" | Alpha beta |

F06 shows that the order matters (an omit only removes what earlier requests
found); F12 and F17 that a repetition criterion on a field with one
repetition matches its only value.

#### 11.2.5 Onto the query program

Each request becomes one DBFindRow: its criteria are parsed into finders
(query-program.md 6.4) and solved as one SimpleFind of ANDed finders over
the table (6.5), related-field criteria through the join path of 6.5. The
combination above maps onto the RID-list operations of query-program.md 8.1:
`R + matches` is op 6 (OR), `R - matches` op 7 (AND NOT), "within M" is the
SimpleFind master list `0d` (Constrain: M = found set) or an AND (op 5).
Which instruction sequence FileMaker Pro sends for several requests is
still not captured (query-program.md 13.3); any sequence that computes R in
this order is equivalent.

#### 11.2.6 Writing a request set

Rows: ids 1..n in request order, `00` = n as `[01][n]` (serial), order list
singleton for one request and one bitmap run otherwise (FileMaker's writer;
any valid RID list reads back), omit marker `00` = empty, criteria keyed by
the packed key, text with `text_node`'s rule, no `fc` for new rows. Set
`kExtendedData` and, for a restored find, `kRestore` [O].

### 11.3 Import Records (35) and Convert File (139)

| location | content |
|---|---|
| `mStepValue` (with `kMenuSelection`) | data source kind = XML `DataSourceReference value`: 1 file, 2 folder, 4 XML source [O 35 steps; other values not seen] |
| `kFilePath` + `[00 0, 82]`, `20` | the source file path list / data source reference |
| `84` (`KImportOrder`) | `FMImportOptions` (below), `kExtendedData` |
| `f0` (`KFlags`) | per-field import flags (below), optional |
| `86` (`KProfile`) | the source file type and its options (11.5) |
| `87` (`KImportProfileEx`) | profile extension: u16 field names row (i16, -1 none), u32 predefined profile id (i32, -1 none), string field delimiter [O 2 steps: `0000 ffffffff 01 '\t'`] |
| `kRestore` | "Import order" stored (restore); FileMaker's XML omits `ImportField` without it [O] |
| slots 0, 1 | calculations of the XML source / XSL (section 10) |

**`FMImportOptions::Write`** [O, 34/34 re-encode, 25/25 equal to XML]:

```
key   target table occurrence (mTableAliasKey)
u8    action      TImportAction: 0 add (XML "Add"), 1 update existing (XML "Replace"), 2 update matching (XML "Update")
u8    flags       TImportFlags: 0x01 split repetitions, 0x02 perform auto-enter, 0x04 match field names,
                  0x08 add remaining data as new records (XML addasnew), 0x10 copy containers as is,
                  0x20 keep original data
u8    charset     0 Windows ANSI, 1 DOS OEM, 2 Mac Roman, 3 Unicode, 4 Unicode (big endian), 5 UTF-8,
                  6 Shift JIS, 7 Simplified Chinese (GB), 8 Traditional Chinese (Big-5), 9 Korean (EUC-KR)
u16   n           one entry per source column, in column order
n x   key field   the target field (empty key: no target)
      u8 mapping  FMImportFieldMapEntry::MappingOption: 0 import, 1 don't import, 2 match (3 unimportable)
```

XML: `ImportField addasnew`, `Options` booleans, `Target/TableOccurrenceReference`,
`action value`, `CharacterSet value`, `Field/Map index id kind`: the first n
`Map` elements are the stored entries (`kind` = mapping); FileMaker appends
every other field of the target table as `kind="1"` [O]. Flag bits 0x01 and
0x20 come from DWARF only (never set in the corpus).

**`WriteImportFlags`** (`f0`) [O 2 steps]: string `mTargetTableName` (the
name of a new table to import into; empty otherwise), u16 n, n x (key
field, u8 per-field `TImportFlags`). XML `Map Options` is the per-field
flag (2 = perform auto-enter for that field); without `f0`, FileMaker shows
the auto-enter flag on fields that have auto-enter options when the global
flag is set [I from 8 steps].

### 11.4 Export Records (36)

| location | content |
|---|---|
| `kFilePath` + path list, `kOpenAutomatically`, `kCreateEmail`, `kOption2` create folders | output file (section 10) |
| `85` (`KExportOrder`) | `FMExportOptions` (below); `kRestore` = "Specify export order" stored |
| `86` | output file type (11.5) |

**`FMExportOptions::Write`** [O, 16/16 re-encode and equal to XML]:

```
u16   n                export fields, in order
n x   field spec       the field (mFieldSpec)
      field spec       mSumByFieldSpec: for a summary field, the break field it is grouped by (none: empty keys)
u16   g                group-by fields
g x   field spec       mGroupByFieldSpec
      u8               mSelected (1)
u8    ExpFormat        1 = apply current layout's data formatting (XML Options Formatting)
u8    ExpCharset       TExpCharacterSet = XML Options value: 0 Windows (ANSI), 1 ASCII (DOS), 2 Macintosh,
                       3 Unicode (UTF-16), 4 Unicode (UTF-8), 6 Japanese (Shift-JIS),
                       8 Simplified Chinese (GB), 9 Traditional Chinese (Big-5), 10 Korean (EUC-KR)
u8    ExpUseNameAlias  26.x writers only: every corpus spec ends after ExpCharset, and Read accepts that
```

XML: `Export/Options name value Formatting`, `Order type="Group"/Group`,
`Order type="Field"/Field value=position` with the field reference [O].
Save Records as Excel (143) uses `85` the same way [O CSV, 0 corpus specs].

### 11.5 Translation profiles (`86`) [O, 77/77 re-encode]

`FMTranslationProfile::Write(HBPosition, key)`: the 4-character file type
(`mFileType`), then the class's body, which always starts with the base u16
= 1 when the profile has a field names row (`mFieldNamesRow != 0`), else 0.
Types and bodies after that u16:

| type | class | body |
|---|---|---|
| `TABS` tab-separated, `COMS` comma-separated, `MRGE` merge, `DBF ` dBase, `HTML` HTML table, `XLXE` Excel (Save Records as Excel) | `FMTranslationProfile` | none |
| `FMPR` FileMaker file | `FMDatabaseProfile` | key: the source table (`m_TableKey`); older writers append a source field list (kept as bytes, 4 corpus steps) |
| `XLS `, `XLSX` Excel import | `FMExcelProfile` | u32 selected sheet, string worksheet name, string file name |
| `BTCH` folder | `FMBatchFileProfile` | string folder name, u8 include enclosed folders, u8 picture and movie import, u8 import by reference (XML `Folder Include`, `Text`, `FileReference`) |
| `XML ` XML import | `FMXMLImportProfile` | u8 XML source kind (1: string = file name or calculation text, 2: string = HTTP URL, 3: field), u8 XSL kind (1: path list, 2: string = URL, 3: none) |
| `XMLE` XML export | `FMXMLExportProfile` | u16 grammar, u16 XSL kind (1: path list, 2: string) |
| `ODBC` | `FMODBCConnProfile` | strings DSN, user, password, u16 flags, then (26.x) u8 query kind and string query; corpus specs have the query string right after the flags [Med] |

Data source kinds in the XML: `DataSourceReference value` 1 file
(`fileType` = the profile type), 2 folder, 4 XML [O]. File type constants
are `FMTranslationData::FT*`, set at start-up and stored as little-endian
u32 (so `0x534d4f43` = `COMS`): `FTCommaSep` `COMS`, `FTTabSep` `TABS`,
`FTHTML` `HTML`, `FTMerge` `MRGE`, `FTDBase` `DBF `, `FTExcel2003` `XLS `,
`FTExcel2007` `XLSX`, `FTExcel2007Export` `XLXE`, `FTXML` `XML `,
`FTXMLExport` `XMLE`, `FTODBC` `ODBC`, plus obsolete SYLK, DIF, WKS, Basic,
ImageCapture and Bento [O]. `FMPR` and `BTCH` are not `FT*` constants.

### 11.6 Print Setup (42), Print (43): `87`, `88` [O 46/46 re-encode]

Both are one chunked value: a sequence of tagged chunks `[4-byte tag][u32
length][payload]` (`PageFormatCommon` / `PrintSettingsCommon`, which look
chunks up by tag with `ScanForTag`). The `Comm` chunk is platform-neutral;
the others belong to the platform that saved them and are kept as stored.

| tag | content |
|---|---|
| `Comm` in `87` (29 bytes) | u8 orientation (0 portrait, 2 landscape), f32 scale (1.0 = 100%), f32 x 4 paper rectangle in points relative to the printable origin (left, top, right, bottom: A4 = -8.5, -8.5, 586.77, 833.36), f32 x 2 printable width and height [O 7 steps against XML `PageSetup`: `Orientation`, `scale`, `size` = paper height and width in 1/100 inch] |
| `Comm` in `88` (11 bytes) | u16 page numbering offset (0; 0x13 for "number pages from 20"), u16 from page, u16 to page (`ffff` or 9999 = all), u16 copies, u8 1 = all pages, u16 1 [O 11 steps against XML `Print/Pages All from to offset`, `Copies value`; collate not located] |
| `PrNm` | printer name, UTF-32 text with a header [Low] |
| `M_PM`, `M_PD`, `MMod` | macOS print records (PMPageFormat / PMPrintSettings XML property lists) [O tags] |

Print's stored records-to-print choice is `mStepValue` (XML `Print type`,
14/14): 1 records being browsed, 2 current record, 3 blank record as
formatted, 4 with boxes, 5 with underlines, 6 with placeholder text [O].
FileMaker Server does not print (script-runtime.md); a server keeps these
records.

[O, 2026-11-14, FileMaker Pro 26.0.3, local disposable Tasks copies, #843]
With "Specify print options" checked, choosing Current record stored
`mStepValue = 2` and `printStyleSetting = 2` in the macOS `M_PM` chunk of
`88`; Records being browsed stored 1 in both places. The `87` PageFormat
record was byte-identical between the two; `88` also differed in the file's
job name. Unchecking "Specify print options" on the Current record step
cleared only `kRestore` in `mStepFlags` (`0x02008080` to `0x00008080`):
`mStepValue = 2`, `87` and `88` stayed byte-identical. In a three-record
found set, with "With dialog" on, running the Restore-on step opened Pro's
Print dialog with Current record selected; the Restore-off step opened it
with Records being browsed selected. Both dialogs were canceled.

[O, 2026-11-16, FileMaker Pro 26.0.3 Tools > Save a Copy as XML, #932]
Pro's XML writes the records as `<Print name="Current record" type="2"
toFile="False">` for its Current record twin and `<Print name="Records
being browsed" type="1" toFile="False">` for its Records being browsed
twin. A copy of the latter that OpenMaker's Script Workspace changed to
Current record (step value and `printStyleSetting`, one byte each) wrote
the same `<Print name="Current record" type="2" …>`, and its whole step was
Pro's twin's apart from the step's hash and UUID. All three showed With
dialog False, Restore True, all pages and one copy. Not measured: what Pro
prints at run time.

### 11.7 Save Records as PDF (144), as Excel (143), as Snapshot Link (152)

PDF options are slots, not a blob (no `83` in any of 23 steps) [O]; with
Restore off FileMaker's XML shows no options. From 3 restored steps
compared with the XML [Med]: `mStepValue` records (1 being browsed, 2
current record), slot 0 u16 (2 in all), slot 1 u8 1 = all pages, slots 2,
3 u8 security "require password to open" / "to control", slot 4 u16 print
permission (2 high resolution), slot 5 u16 edit permission (4 any except
extracting pages), slot 6 u8 enable copying, slot 7 u8 screen reader,
slot 8 u16 initial view (2 pages panel and page), slot 9 u16 page layout
(1 single page), slot 10 u16 magnification (7 = 100%). Calculations: the
XML `position` of each calculation is its slot (page from = 4, and the
document metadata and passwords at the positions the XML gives). So slot 4
can hold both the print permission number (`03`) and the page-from
calculation (`05`). A production step with Restore and All pages stores
exactly that: the number 2 and the calculation `1`, with slots 8-10 =
2 / 0 / 0 (pages panel, default layout, default magnification) [O, its
stored step, relayed privately]. [I] The calculation is From's default,
kept but unused while All pages is chosen. Numeric
data here is 1 or 2 bytes wide (`SetNumericData<T>`), not always 4.
Excel: `mStepValue` is 0 in all 10 corpus steps (one example file);
slot 0 u8 (0 in all) and slot 1 u16. Slot 1 is the records choice, 1
records being browsed, 2 current record [O]: FileMaker Server's XML writer
shows `Save type="Records being browsed" value="1"` and `Save
type="Current record" value="2"` (in `Options` and as the `Excel` element
under `UniversalPathList`) for two
steps FileMaker Pro wrote in one corpus script with Restore on, which differ
only in slot 1. With Restore off the writer shows no options. The meaning of
slot 0 is not mapped (the same XML shows `useFieldNames="True"` with slot 0
= 0). FileMaker Pro 26.0.3 applies the choice only with Restore [O: four
runs with dialog off on a disposable file, three records found, record 2
current]: Restore on with Records being browsed wrote 3 rows, Restore on
with Current record only record 2, and Restore off wrote all 3 whichever
choice the step kept; no run prompted or failed. Its options dialog kept
Current record selected across the Restore-off run. Not observed: with
dialog on. The current record's summary values are measured for a total,
running total and count (with and without restart), a List, a weighted
average and a subtotaled fraction in local FileMaker Pro 26.0.3; see
"Implementation notes (import/export)", Exports. Other summary options
in this scope remain unmeasured. The server engine cannot run the step
(error 3).
Snapshot Link: records in `mStepValue` (section 3.2).

### 11.8 Other compound parameters

**Calculation slots from the XML.** Every `Calculation` in FileMaker's XML
has `position` = the step slot holding it (Set Field value 0, repetition
10; target variable repetition `0x20 + n`) [O]. A writer can take slot
numbers for any step from an XML export.

**Go to Related Record (74)** [O 117 steps, 63 compared with XML]:
related occurrence at `[00 0, 11]`; `mStepValue` 1 original layout, 5
layout key in `mReserved` (`kMemberKey`); `kRestore` = show only related
records; `kOption2` = match all records in the current found set; `kTurnOn`
(0x20000) = show in a new window, then slot 0 = window name, slots 1-4 =
height, width, top, left (calculations), `[00 0, 03]` = the window style
word (XML `WindowReference/Style value`: `00370602` Document, `80050204`
Dialog; the same word as New Window); slot 6 list value = animation
(0 none). Steps without a new window may keep a stale style word in
`[00 0, 03]` [O].

Desktop external-layout routing uses `kOption` (0x100, the remaining
Go to Related Record option in the parameter schema) to select the
related table's owning file's layouts. Layout calculations use slot 5,
as Go to Layout; new-window calculations retain their slot positions even
when some are absent. This encoding is exercised with generated two-file
fixtures; a native FileMaker Pro external-layout step/GUI differential is
still needed to confirm the external flag and calculated layout variants.

**Replace Field Contents (91)** [O 126 steps]: `[00 1, 03]` = choice (XML
`List value`: 1 current contents, 2 serial numbers, 3 calculation; absent =
0); calculation slot 0; `[00 2, 03]` increment, `[00 3, 01]` initial value
text; `[00 4, 03]` u8 "Update entry options"; `[00 5, 03]` u8 "Entry option
values" (use the field's serial settings); `[00 0, 11]` `01 00`; target
repetition calculation slot 10. "Skip auto-enter options" is `kRestore`
together with `[00 6, 03]` inverted: FileMaker's XML reader sets `kRestore`
on every step it reads and stores `[00 6, 03]` = 0 for True and 1 for False,
and its writer shows True only when `kRestore` is set and `[00 6, 03]` is 0
(`V2_ScriptStepXML::SerializeReplaceFieldContents` 0x4b56e4 and
`DeserializeReplaceFieldContents` 0x4b517a in the Mac `FMEngine`; observed
on three probe steps, section 12). So `[00 6, 03]` = 1 means auto-enter
options apply; the reader writes no `[00 0, 11]` (the corpus's `01 00` comes
from FileMaker Pro's own editor) [O].

**Insert from URL (160)** [O 47 steps, 38 compared with XML]: URL slot 0,
cURL options slot 1, `kOption` 0x100 = "Do not automatically encode URL"
(XML `autoEncode` inverted, 38/38), `kSelectAll` = Select, `kVerifySSLCertificates`; a
variable target's name and repetition are slot `0x20`.

**Show Custom Dialog (87) inputs** [O 21 inputs against XML]: input 1-3
target is a field (input 1 in `mReserved` with `kFieldSpec`, inputs 2-3 at
`[00 1, 10]`, `[00 2, 10]`) or a variable (variable spec there, name text
and repetition calculation in slot `0x20 + n - 1`); label calculation in
slot `n + 1` (2, 3, 4); "use password character" `[00 n - 1, 03]` = 1;
button commit flags are `[00 3..5, 03]` (section 10 confirmed).

**Insert File (131) filters, `8c`** [O 24/24]: one node per filter keyed by
its index as two bytes (`00 00`, `00 01`), holding `05` the filter name
calculation (a `KFormula` node, section 4.2) and `07` the extension list
text (`*.pdf`, `png; jpg; gif`); title, storage, display and compress
options are other slots and flags of the step (XML `Options type="..."`).

**External file references, `20`** [O 16/16]: `[u8 len][serial]`, the
external data source id (Perform Script in another file, Open/Close File,
Import source): `01 02` = data source 2. `01 00` (source 0) occurs on 2
steps, presumably a cleared reference [I].

**`83` (`KBlob`)** [O]: compiled AppleScript (`FasdUAS` data, Perform
AppleScript 67) and plug-in steps (`MBSP` + function bytes, 186); kept as
bytes.

### 11.9 Verification

```
cargo run --release -p om-scripts --example stored_specs -- [--emit] <file.fmp12>... | @list
python3 -I tools/script_specs_xml.py <emit.tsv> <saveAsXML dir>
```

`stored_specs` decodes every stored spec with the layouts above, re-encodes
it and compares the node byte for byte. On the 746 corpus files: sort 110,
find 32, import 34, import flags 2, export 16, profile 77, profile
extension 2, page format 20, print settings 26, filters 24, file
references 16, all decoded and all re-encoded exactly (plus 1,858 slot
summaries of the steps in 11.7-11.8). `script_specs_xml.py` renders the
same canonical text from FileMaker Server's `FMDeveloperTool --saveAsXML`
export of 143 files (the other 9 need passwords) and compares: sort 72/72,
find 11/11, import 25/25, export 16/16 equal (the remaining specs are in the
9 files without an export, or in steps without Restore, which the XML
omits). The probe file's 31 specs, written by FMUpgradeTool from XML, read
back equal to that XML (11/11 sorts, 20/20 finds).

Reproduction of the probes (research side, scratch
`re/work/scripting/specs/probe/`): `probes.py` extends the script oracle's
`make_patch.py` with Sort Records, Perform Find, Constrain, Extend and Enter
Find Mode builders; `run.sh p1` applies the patch to a copy of the Tasks
template and runs the scripts on FileMaker Server's engine in the harness
container. The step builders could join `tools/fms-script-harness`.

### 11.10 Unknowns

- Sort: the meaning of an order byte other than 0/1 when stored (never
  written); the summary reorder was not executed; whether the host's
  SortList accepts `02 01 00` empty keys as well as `00` (not sent yet).
- Find: the exact instruction sequence FileMaker Pro sends for several
  requests (11.2.5); the unpacked criterion key for a `KUnknown` table never
  observed.
- Import: the per-field flag default when `f0` is absent (Med); `TImportFlags`
  0x01 and 0x20 and charset 9 positions confirmed by DWARF/XML only; the
  `FMPR` profile tail of older writers; ODBC profile layout (Med); Import's
  `mStepValue` for an ODBC source (3 expected).
- Export: `ExpUseNameAlias` meaning (26.x, not in the corpus).
- Print: `Comm` last u16 of `88` and the collate setting; the `PrNm`
  layout; the macOS chunks are opaque by design. The #843 dialogs were
  canceled, so which records would be printed after confirmation is
  unmeasured. Whether Restore-off's Records being browsed selection is a
  fixed default or Pro's last choice in that session is also unmeasured.
- Save as PDF: slot 0 and the full calculation slot list (document title,
  subject, author, keywords, passwords) need a restored step with them set.
  Its options writer (`SerializeSaveRecordsAsPDFOptions`, Mac `FMEngine`
  0x4b875a) reads calculations 0-8 and the same numeric slots 2-10 as Create
  PDF, whose title, subject, author and keywords are calculations 0-3 and
  passwords 7 and 8 (section 12.2); the same calculation slots are likely
  [I]. Save as Excel options.

## 12. Slot maps of the steps added in FileMaker 21-26, and of compound elements

No sample file holds the steps added in FileMaker 21-26, so their slots come
from two other sources. Both are recorded in
[catalogs/script-step-params.csv](catalogs/script-step-params.csv), whose
`evidence` column says `xml+params+oracle` for every row below.

- **From bin.** Each step's XML writer and reader in the Mac FileMaker Pro
  26.0.3 `FMEngine` (x86_64 slice, `nm` = Ghidra addresses):
  `V2_ScriptStepXML::ProcessParameterValues` (0x496300) dispatches on the
  step id to a `Serialize*`/`Deserialize*` pair, whose writer/reader
  addresses are given per step below.
  Element names are the `kXMLEngine_*` tag constants (UTF-16 literals the
  module initializer assigns, e.g. `kXMLEngine_SetLLMAccountName_Tag` =
  `LLMAccountName`). Slots are the immediate arguments of the
  `ScriptStep` accessors the pair calls (`Get/SetListValue`,
  `Get/SetNumericData<T>`, `Get/SetFieldSpec`, `Get/SetTableAliasKey(s)`,
  `Set/GetScriptKeyWithStorePosition`) and the slot argument of
  `SerializeCalculation`/`DeserializeCalculation` (vtable 0x1f8/0x1f0).
  Send Mail, Execute URL's URL and Configure Persistent Data delegate to
  `V1_ScriptStepXML` (the object at offset 0x70). REA evidence:
  `ProcessParameterValues`
  `ev_f8216c2c2ed36b16643a756b839b90812b71c9f2e040ea69d34008ff52ec2949`,
  `DeserializeGoToRecordList`
  `ev_6ed3dbb20434f674a5803edaad3b57fe8aee4244c069bfa2f30ca4f7ec65e5e0`,
  `DeserializeCallbackScript`
  `ev_b1c4810a94019e764944b208dfb4df2e4dc4909ffcbf55276e8f1a0384df12d6`.
- **Observation.** 115 probe steps, one per step and choice, written as
  Save as XML (FMSaveAsXML 2.3) step XML with distinct values (every
  calculation a text literal naming its element), added to a copy of the
  Tasks template by FileMaker Server 26.0.3's FMUpgradeTool
  (`tools/fms-script-harness` `run.sh -p`), its stored leaves dumped
  (`om-hbam` example `dump_keys`), and the file saved as XML again
  (FMDeveloperTool, `run.sh -x`). Every element below was stored where the
  writer and reader put it and written back unchanged, except where noted.
  The same 61 steps imported with `<Options>0</Options>` stored the same
  flags: for these steps the reader derives every flag from the elements,
  and `Options` adds nothing. With the CSV rows, `step_params` explains
  2,125 of the probe's 2,129 leaves and all 597 set flag bits (the other 4
  are the counts in 12.1); its result on 739 corpus files (the templates
  and third-party example files of section 10) is unchanged.

FileMaker Pro's own editors were not observed (section 9). Tags below:
**[O]** stored by the importer and written back by Save as XML, **[bin]**
from the code only.

### 12.1 Conventions

- A calculation the V2 writer emits is wrapped in a parameter named by its
  tag: `<Parameter type="LLMAccountName"><Calculation datatype="1"
  position="1">` [O]. The V1 writers (Send Mail, Execute URL's URL,
  Configure Persistent Data) and Send Event's source write the bare
  `<Calculation>` under their own element [O]. The reader stores a
  calculation only in the slot its code names, and finds a wrapped one by
  its `position`: one whose `position` is not that slot is not stored
  [bin].
- A list inside a compound element is the value of a slot node `[00 n]`
  (u32); a `List` directly under the step's `Parameter type="List"` is
  `mStepValue` with `kMenuSelection` [O].
- A response target (`Parameter type="Target"`, written with
  `SerializeSpecifyTarget(.., 0x20)`) is a field in `mReserved`
  (`kFieldSpec`) or a variable: the variable spec there and its name as text
  in slot 0x20 (`kTextData`). FMUpgradeTool stores a variable target's
  repetition `1` as a calculation in slot 10, while the writer shows it with
  `position="32"` [O]. A nested target (`<Parameter type="X"><Parameter
  type="Target">`) keeps the field spec at `[00 a, 10]` and the variable
  name at `[00 b, 01]`; (a, b) is given per step.
- Several keys in one slot (Perform SQL Query by Natural Language's table
  occurrences, Validate Scripts' scripts): `[00 n, 11]` holds 6 bytes per
  key (the key and its filler) and `[00 n, 04]` the count, `02 00 00 00`
  for two [O]. The CSV grammar has no location for the count.

### 12.2 Steps added in FileMaker 21-26

**Perform Script on Server with Callback (210)** [O; `SerializeCallbackScript`
0x49f134, `DeserializeCallbackScript` 0x49f540,
`DeserializeCallbackScriptParameter` 0x49f7dc,
`De/SerializeSpecifyScript_withStorePosition` 0x4c0a4c/0x4c136e]. The script
to run is stored as Perform Script on Server's (164): from list, its key in
`mReserved` (`kScriptKey`); by name, `[00 1]` = 1 with the name calculation
in slot 2 and `0x800 | 0x2000000`. `Parameter`: calculation slot 0.
`CallbackScriptState`: `mStepValue` (1 Halt, 2 Exit, 3 Resume, 4 Pause).
The callback script: `[00 6]` = 0 from list (`CallbackScriptReference`),
its key stored by `ScriptStep::SetScriptKeyWithStorePosition` (0x50259a) as
a calculation in slot 3 whose formula is a quoted text literal of the key
segment in hex (`"08"` for script 8, `"8750"` for the two-byte key `87 50`),
with `0x100`; `[00 6]` = 1 by name (`CallbackScriptByName`), the name
calculation in slot 7 with `0x1000 | 0x2000`. Its parameter
(`CallbackScriptParameter`, or a `repetition` inside
`CallbackScriptReference`): calculation slot 4 with `0x200`. A callback
script in another file adds `0x400` and a calculation in slot 5 [bin].
In a local Pro 26.0.3 file, choosing a callback from data source member
`[32, 5, 1]` put the quoted text `"01"` in slot 5; its callback key in
slot 3 and parameter in slot 4 stayed unchanged [O: #511]. A callback
selected from the same file's list as script id 2 stored the text
calculation `"02"` in slot 3 [O: #511]. Changing the choice to By name
left that slot in the file, set `[00 6]` to 1 and added the callback name
in slot 7; slot 4 kept its parameter [O: #511]. Execution of a callback
in another file is unmeasured. The step always calls the server
asynchronously (script-runtime.md 11.4), so `0x100` is not PSOS's "do not
wait" bit here.
OpenMaker's runtime and step text read the callback from slot 2 and
its parameter from slot 3, which is wrong (#511).

**Configure AI Account (212)** [O; `SetLLMAccount` 0x4a8b34/0x4a872a]:
Verify SSL `0x10000000`; provider `mStepValue` (0 OpenAI, 1 Anthropic, 2
Cohere, 3 AFM, 4 Google, 5 Open Source, 20 Custom); `LLMAccountName` slot
1, `LLMEndpoint` slot 2, `LLMAPIKey` slot 3. The writer shows the endpoint
only for provider 20; the reader stores it for any.

**Fine-Tune Model (213)** [O; `FineTuneLLM` 0x4a94e8/0x4a8d38]:
`FineTuneAccountName` slot 1, `FineTuneBaseModel` slot 2,
`FineTuneDataSource` `[00 6]` (1 Table, 2 File; 2 sets `0x800`). Table:
`FineTuneTable` occurrence key `[00 0, 11]` with `0x100`,
`FineTuneDataCompleteField` field spec `[00 3, 10]` with `0x200`. File:
`UniversalPathList` `[00 0, 82]` with `kFilePath`. `FineTuneParameters`
slot 4 with `0x400`; response target (12.1).

**Perform SQL Query by Natural Language (214)** [O; `LLMRequest`
0x4af11c/0x4ae624]: `LLMAccountName` 1, `LLMModel` 2, `LLMParameters` 3
(`0x100`), `LLMMessage` (the prompt) 4, `LLMPromptTemplateName` 18
(`0x2000`); Stream `0x1000`; response target. `LLMOptionsSelection`
`[00 15]`: 0 From list, 1 By JSON data, which sets `0x800` and takes
`LLMOptionsName` (slot 16); with 0 the writer also shows `LLMAction`
`[00 6]` (1 Query, 2 Query for Data Only, 3 Email, 4 Get SQL, 5 Get First
SQL Only, 6 Get DDL; 30 hides the tables) and `LLMTablesSelection`
`[00 8]`: 0 From list, the occurrences of `TableOccurrences` at `[00 9, 11]`
(12.1) with `0x200`; 1 By name, `LLMTablesName` slot 11; 2 By DDL,
`LLMByDDL` slot 13. `LLMWebScript` (`0x2000000`): `Name` slot 20,
`FunctionRef` slot 21 and the web script parameters, whose count is
`[00 19, 03]` (0 in the probe; their slots were not measured).

**Insert Embedding (215)** and **Insert Image Caption (241)** [O; one
writer, `LLMEmbedding` 0x4b1786/0x4b1492]: `LLMEmbeddingAccountName` 1,
`LLMEmbeddingModel` 2, `LLMEmbeddingInputText` 3 (the text, or the image
for 241), `LLMParameters` 5 (`0x100`); response target.

**Insert Embedding in Found Set (216)** and **Insert Image Captions in Found
Set (240)** [O; `LLMBulkEmbedding` 0x4b1de6/0x4b192a]:
`LLMEmbeddingAccountName` 1, `LLMEmbeddingModel` 2, source field
`LLMBulkEmbeddingField` `[00 3, 10]` (`0x200`), `LLMParameters` 4
(`0x800`), target field in `mReserved`; Replace target contents `0x400`,
Continue on error `0x1000`, Show summary `0x2000` (216 only).

**Set AI Call Logging (217)** [O; `LLMDebugLog` 0x4a8c8e/0x4a8992]: enabled
`0x20000`, Verbose `0x200`, Truncate Messages `0x400`, `LLMFileName` slot 1
with `0x100` (the writer shows it only with `0x100`).

**Perform Semantic Find (218)** [O; `LLMSearch` 0x4b2226/0x4b2b62]: the
embedding field in `mReserved`; `Query` `[00 8]` with `0x10`: 1 Text
(`Text` slot 2), 2 Vector data (`File` slot 9) and 3 Image (`Image` slot
11), both written with `datatype="6"`; `AccountName` 1 and `LLMEmbeddingModel` 4 for Text and
Image; `Records` `[00 7]` with `0x8` (1 All records, 2 Current found set);
Return count `0x100` with `Count` slot 3; `Condition` `[00 6]` with `0x400`
(1 greater than, 2 less than, 3 equal to, 4 greater than or equal to, 5 less
than or equal to) with `Threshold` slot 5; `SaveResult` (`0x200`) nested
target (10, 10); `LLMParameters` 12 (`0x800`).

**Perform RAG Action (219)** [O; `RAGSpace` 0x4ab85e/0x4ab0da]:
`RAGAccountName` 1, `RAGSpaceID` 2, `RAGAction` `[00 5]` (1 Add Data, 2
Remove Data, 3 Send Prompt). Add Data: `RAGDataSource` `[00 3]`: 1 From
Text (`RAGInputText` slot 4; `RAGAddDataResponse` nested target (12, 13)
with `0x200`), 2 From File and 3 From File (Async) (`UniversalPathList`),
4 From Container and 5 From Container (Async) (field in `mReserved`);
Detect vertical text `0x8000`; `RAGTokensPerTextChunk` 14 (`0x40000`).
Remove Data: `RAGPromptParameters` 7 (`0x400`). Send Prompt: `LLMMessage`
6, `RAGModel` 8, `RAGAIAccountName` 9, `TemplateName` 11 (`0x2000`),
`RAGPromptParameters` 7 (`0x400`), Stream `0x1000`, response target.

**Generate Response from Model (220)** [O; `LLMRequestWithTools`
0x4acb20/0x4ac2a0]: `LLMAccountName` 1, `LLMModel` 2, `LLMUserPrompt` 3,
`LLMInstruction` 5 (`0x200`); `LLMMessages` (`0x400`) nested target (6, 7);
`LLMSlidingWindow` (`0x40000`): `LLMSlidingWindowVariable value` = variable
name `[00 8, 01]`, its `IncludeToolCalls` = `[00 22, 03]`,
`LLMSlidingWindowCount` slot 9, `LLMSlidingWindowVariableRepetition` slot
11; `LLMTemperature` 12 (`0x800`), `LLMToolDefinitions` 13 (`0x1000`),
`LLMToolCallsFromModel` (`0x2000`) nested target (14, 15); `LLMAction`
`[00 18]` (1 Tool Call, 2 Dump Tools); `LLMParameters` 16 (`0x100`);
`LLMWebScript` as 214; Stream `0x8000`, Agentic mode `0x20000`, response
target.

**Perform Find by Natural Language (221)** [O; `LLMCreateFind`
0x4b1006/0x4b0b76]: `LLMAccountName` 1, `LLMModel` 2, `LLMMessage` 3,
`LLMAction` `[00 4]` (1 Found Set, 2 Found Set as JSON, 30 Find Request as
JSON), `LLMParameters` 5 (`0x100`), a calculation tagged `Target` in slot 6
(meaning not mapped), `TemplateName` 7 (`0x200`); response target.

**Configure Regression Model (222)** [O; `LLMTrain` 0x4adbdc/0x4ad4a2]:
`LLMTrainActions` `[00 7]` (0 Train Model, 1 Save Model, 2 Load Model, 3
Unload Model), `LLMTrainAlgorithm` `[00 9]` (0 Random Forest, not stored),
`LLMTrainModelName` 3; `LLMTrainVectorsField` `[00 2, 10]` (`0x100`),
`LLMTrainLabelsField` `[00 8, 10]` (`0x400`), `LLMTrainModelParameters` 4
(`0x800`), which the writer shows only while `mStepValue` is 0 [bin]; Skip
empty or invalid records `0x1000`; response target. Neither the reader
nor the writer has an element for a model file.

**Execute URL (224)** [O; dispatcher case 0xe0, V1 `SerializeURL`
0x4ed624, `SpecifyScript` 0x4c0da2/0x4c03f0]: `URL` slot 0
(`autoEncode="False"` = `0x100`), cURL options slot 1, Verify SSL
`0x10000000`; the callback `ScriptReference`'s key in `mReserved`
(`kScriptKey`) and its `repetition` (the parameter) in slot 2.
`GetScriptKeyWithStorePosition` (0x50273e) also treats step 224 as having
a key with store position [bin]; the importer does not write one.

**Save Records as JSONL (225)** [O; `SaveAsJSONL` 0x4aa550/0x4a9ecc]: paths
`[00 0, 82]`, Create folders `0x200`, Format for fine-tuning `0x1000`;
`SaveAsJSONLTable` occurrence key `[00 0, 11]` (`0x100`); with
fine-tuning, `SaveAsJSONLSystemPromptField` 2, `SaveAsJSONLUserPromptField`
3, `SaveAsJSONLAssistantPromptField` 4; otherwise
`SaveAsJSONLDataCompleteField` is a field in `mReserved`.

**Configure Prompt Template (226)** [O; `ConfigureLLMTemplate`
0x4b3760/0x4b328e]: `TemplateName` 1, `ModelProvider` `[00 2]` (0 OpenAI, 1
Anthropic, 2 Cohere, 4 Google, 20 Custom). Pro 26.0.3 saves all five menu
choices at this slot in local SQL Query steps [O: #149]; the XML writer
does not name Google.
`TemplateType` `[00 5]` (1 SQL Query: `SQLPrompt` 3 with `0x200`,
`NaturalLanguagePrompt` 4 with `0x100`; 2 Find Request:
`FindRequestPrompt` 6 with `0x400`; 3 RAG Prompt: `RAGPrompt` 7 with
`0x800`). Pro saved each of the three template types in `[00 5]` and left
`mStepValue` at 0; all five provider choices in SQL Query also left it at
0 [O: #149, seven local files, scripts not run]. The dispatcher writes
`mStepValue` as an unnamed `List`; what a nonzero value means remains
unmeasured.

**Configure RAG Account (227)** [O; `CfgRAGAccount` 0x4aaf8c/0x4aad24]:
`RAGAccountName` 1, `RAGEndpoint` 2, `RAGAPIKey` 3, Verify SSL
`0x10000000`.

**Go to List of Records (228)** [O; `GoToRecordList` 0x4ce10c/0x4cded8]: the
record ids are the calculation in slot 7; `LayoutReferenceContainer` is
`mStepValue` (5 with the layout key in `mReserved`, `kMemberKey`);
`Animation` `[00 6]`; a new window sets `kTurnOn` (`0x20000`) and stores the
window as Go to Related Record's (11.8): name slot 0, height, width, top,
left slots 1-4, style word `[00 0, 03]`. Turning New Window off in a local
Pro 26.0.3 file cleared `0x20000` but left the old slot-0 name calculation
in place; slot 7 still held the record ids [O: #512]. OpenMaker reads the
ids from slot 0 and the new window from `0x100`, which is wrong (#512); its
layout and animation slots are right.

**Configure Persistent Data (238)** [O; V1 `SetPersistentData`
0x4f4c0a/0x4f4a1c]: `PersistentStore` with `Name value` = the key as text
in slot 0 (`kTextData`), `value` the calculation in slot 1, `instanceID`
the calculation in slot 2, and an empty `Delete` element = `0x100` (no
value then). `mStepValue` is not used.

**Validate Scripts (239)** [O; `ValidateScript` 0x4b04fa/0x4affa8]:
`LLMAccountName` 1, `LLMModel` 2, `LLMInstruction` 4; `Scripts` (one
`Script` > `ScriptReference` each) at `[00 3, 11]` (12.1) with `0x100`;
response target.

**Create PDF (243)** [O; `CreatePDFFile` 0x4cf378 and `CreatePDFFileOptions`
0x4b9fd2/0x4b93f6]: Restore `0x2000000`; `Options`: `Document` `Title`,
`Subject`, `Author`, `Keywords` slots 0-3; `Security` `Open` `[00 2, 03]`
with its `Password` slot 7, `Control` `[00 3, 03]` with its `Password`
slot 8, `Print` `[00 4, 03]` (0 Not Permitted, 1 Low Resolution, 2 High
Resolution), `Edit` `[00 5, 03]` (0 Not Permitted, 1 Inserting, deleting
and rotating pages, 3 Commenting, filling in form fields and signing, 4 Any
except extracting pages), `EnableCopying` `[00 6, 03]`,
`AllowScreenReader` `[00 7, 03]`; `View` `show` `[00 8, 03]` (0 Page Only,
1 Bookmarks Panel and Page, 2 Pages Panel and Page), `Layout` `[00 9, 03]`
(0 Default, 1 Single Page, 3 Facing, 4 Continuous and Facing),
`Magnification` `[00 10, 03]` (0 Default, 1 Fit Page, 4 25%, 12 800%).
Booleans are u8, choices u16. Slots 2-10 are Save Records as PDF's (11.7).

**Append PDF (244)**, **Open PDF (246)** and **Close PDF (245)** [O;
`AppendPDFFile` 0x4cf772/0x4cf4e0, `OpenPDFFile` 0x4cfce8/0x4cfa56,
`ClosePDFFile` 0x4d024a/0x4cffcc]: `From` (`SaveTo` for 245) `[00 11]`: 0
File, `UniversalPathList` `[00 0, 82]` and Create folders; 1 Target, a
response target, with `PDFName` slot 12 for 245. `PDFPassword` slot 7 with
`0x100` (244, 246). Create folders is written from `0x200` as `Boolean
id="512"`, but the reader looks for `id="256"` and sets `0x100`: an
exported step loses Create folders on import, and `id="256"` sets the
password flag instead [O 5 steps, bin].

### 12.3 Compound elements of older steps

**Send Mail (63)** [O 8 steps; V1 `SendMail` 0x4eb702/0x4ea5a2]. Under
`Parameter type="Email"`: `Send SMTP` = `[00 4, 03]`, `OAuthAuthentication`
attribute = `[00 7, 03]`; `Multiple` (one email per record) `[00 3, 03]`;
`To`, `CC`, `BCC` calculations 0-2, each `CollectAddresses` `[00 0..2, 03]`
(written and read only without `Multiple`); `Subject` 3, `Message` 4;
attachments `UniversalPathList` `[00 0, 82]` with `kFilePath`. Beside
`Send`, `SMTP`: `Name` 5, `Email` 6, `ReplyTo` 7, `Server` 8, `Port` 9,
`Encryption` `[00 5, 03]` (u8: 0 None, 1 SSL, 2 TLS), `Authentication`
`[00 6, 03]` (u16: 1 None, 2 Plain Password, 3 Cram-MD5), `UserName` 10 and
`Password` 11 (not read with 1). `OAuthAuthentication`: `OAuthProvider`
`[00 8, 03]` (u16: 1 Google, 2 Microsoft), `Name` 12, `ReplyTo` 14; Google
`Email` 13, `UserID` 15, `ServiceAccount` 16, `PrivateKey` 17; Microsoft
`PrincipalName` 18, `TenantID` 19, `ClientID` 20, `ClientSecret` 21. With
dialog (`0x80` inverted) is written only for client mail; with SMTP or OAuth
the writer shows `Boolean type="No dialog" value="True"`, which stores
nothing.

Step text [O, FileMaker Pro 26.0.3 Script Workspace, #221 comment
6874156929 on #837's file]: client mail with To (Collect addresses on),
Subject, Message and the attachment `file:report.pdf` reads `Send Mail [
Send via E-mail Client ; With dialog: Off ; To: "pat@example.com" ;
Subject: "Weekly report" ; Message: "See attached" ; “report.pdf” ]`:
Collect addresses is not shown, and the attachment shows as its file name
in typographic quotes, without the `file:` kind. Unmeasured: the text with
CC, BCC, one email per record, SMTP or OAuth, several attachments, or an
attachment of another kind.

**Send Event (57)** [O 6 steps; `SendEvent` 0x4bb8c0/0x4bb32c]: `Target
value` = the application name as text `[00 0, 01]` (`kTextData`) and
`Target id` its signature `[00 6, 03]` (the writer also shows it as
`type`); `Send class` `[00 0, 03]` (`aevt`) and `Send id` `[00 1, 03]`
(`odoc`, `pdoc`), four-character codes as u32 (the writer adds
`classString`, `idString` and, for known events, `name`); `Source type`
`[00 3, 03]` (u16): 0 Document > `UniversalPathList` `[00 0, 82]`, 1
Calculation > `Calculation` > the calculation (slot 0), 2 Text, its
character data as text `[00 7, 01]`; `Options` `Wait` `[00 2, 03]`, `Copy`
`[00 4, 03]`, `Foreground` `[00 5, 03]`.

**Speak (66)** [O; `SpeakOptions` 0x4a14bc/0x4a11bc]: the text is the
calculation in slot 0; `Voice value` `[00 1, 03]` and `Voice id`
`[00 2, 03]` (u32); `Wait` `[00 0, 03]` (u8). The 26.0.3 writer does not
read the voice name text the corpus has at `[00 0, 01]` [bin].

**Perform Find/Replace (128)** [O 5 steps; `PerformFindReplace`
0x4b4932/0x4b427c]: `perform` `[00 0, 03]` (1 Replace & Find, 2 Replace, 3
Replace All, other values Find Next), `find` calculation 0, `replace`
calculation 1, `direction` `[00 1, 03]` (0 Forward, 1 Backward, 2 All),
`across` `[00 2, 03]` (0 All records/requests, other values the current
record), `within` `[00 3, 03]` (0 All fields, other values the current
field), Match case `[00 4, 03]`, Match whole words only `[00 5, 03]`, With
dialog `0x80` inverted. The reader and writer use none of the `0x100`,
`0x200` and `0x1000` bits `GenerateParameters` lists.

**Replace Field Contents (91)**: 11.8.

## Implementation notes (import/export)

`om-transfer` implements Import Records (35), Export Records (36) and Save
Records as Excel (143) from section 11.3-11.5 and runs of FileMaker Server
26.0.3's own script engine; OpenMaker Server (`om-server`,
`scripts/host/transfer.rs`) and the desktop client run them. Everything
below is [O] from those runs unless marked.

### Oracle

The script harness has libFMEngine's shared runtime, where 35, 36, 143 and
132 return 3 (their handlers are fmsased's, script-runtime.md 10.2). The
runs were therefore made on the lab FileMaker Server with
`tools/fms-transfer-oracle` (README there): a separate probe database
generated from the Contacts starter template plus probe fields, run as a
schedule, its `omprobe_*` files read from the server's Documents folder.
Batteries `x01-export`, `x02-import`, `x03-export`, `x04-import-matching`
(the import review's P2 and P4, 2026-10-29); results in
`crates/om-transfer/tests/data/`.

Battery `x05-export-summaries` (#372) ran offline in the script harness
instead. With `DOCUMENTS` set, the harness runs Export Records as
fmsased's `FMS::SASEScriptRuntime::DispatchStep` does: an
`FMExportManager`'s `PerformExport` on the script's window
(`GetWindow(true)`), its error the step's [from bin: fmsased 0x79bdab].
It accepts every path, where fmsased's file user limits writes to its
Documents and session folders (`SessionTempAndDocDirectoryFileFilter`;
libDBEngine's default filter fails every path, so 800). Run that way, x01
gave the lab server's files: 28 of its 34 exported files byte for byte,
the other six differing only in the file name (FMPXMLRESULT `DATABASE
NAME`), the Excel creation time and the dBase header date. Its errors
matched too, except the path outside both folders (800 on the server, 0
in the harness). x05's results are in
`tools/fms-transfer-oracle/results/x05/`.

The probe steps come from FileMaker's XML
importer (`make_patch.py` builders); their XML shapes were settled by
writing a step from 11.3-11.5 into a scratch file and reading FileMaker's
Save as XML of it:

- Export: `Parameter type="UniversalPathList"` (`UniversalPathList
  fileType membercount` / `ObjectList` / `Location`), for XML a
  `DataSourceReference fileType="XMLE"` with `XSL index="0" grammar`, then
  `Parameter type="Export"`: `Options name value Formatting`, `Order
  type="Group"` with `Group value=n` + `FieldReference`, `Order
  type="Field"` with `Field value=n` + `FieldReference` and, for a summary
  field grouped by a break field, `SummaryField/FieldReference`.
- Import: `DataSourceReference fileType isPredefined delimiter value`
  (value = the data source kind; for XML `XML index="1"` around the path
  list and `XSL index`), `ImportField addasnew isPredefined delimiter
  fieldNameRow` with `Options`, `Target`, `Field/Map New name index id kind
  Options`, `CharacterSet`, `action`.
- Save Records as Excel: `Restore`, `UniversalPathList fileType="XLXE"`
  with `Excel` (`Boolean "Use field names as column names"`, `Worksheet`,
  `Title`, `Subject`, `Author` parameters), `Options/Save type value
  useFieldNames`. Its `85` is not shown in the XML.

### Stored specs

- The path list of a step (`[00 0, 82]`) is the 11.0 path stream: u32
  count, then per path its kind (`file`, `filemac`, ...), a volume and the
  path; FileMaker's XML shows `kind:path`. `om-scripts` read it as text
  before and is fixed.
- `87` field names row: the 0-based row holding field names, -1 for none;
  the rows up to and including it are not imported. FileMaker's XML
  importer stores `fieldNameRow` as given (so `0` makes the first row a
  names row) and the `delimiter` attribute as the delimiter's text (`"0"`
  splits records at zeros), while the XML writer shows a tab as `0` and a
  comma as `1`. `isPredefined="False"` stores 0, not -1. The `86` base u16
  follows `fieldNameRow`.
- FileMaker's XML importer writes `ExpUseNameAlias` = 1, `f0` for every
  import, and the `86` base u16 = 1 for exports.
- `XMLE` grammar 1 is FMPXMLRESULT; XSL kind 0 is none.

### Exports

Rows and values (all formats):

- A stored field exports its stored text: a number field what was entered
  (`007`, `2.50`, `abc`, `1e3`), a date, time or timestamp as typed
  (`2/29/2024`, `1:02`, `1/2/2020 15:04`); values set from calculations
  store `MM/DD/YYYY`, `HH:MM:SS` (24-hour, `26:01:00`, `-01:00:00`) and
  `MM/DD/YYYY hh:mm:ss AM` (12-hour, `11:59:59.5 PM`). Unstored
  calculations and summaries export in the same forms, numbers as FileMaker
  writes them (`-.002`).
- Container fields are left out of every format (and of FMPXMLRESULT's
  metadata).
- A repeating field holds all its repetitions; an empty found set writes no
  file and the step succeeds; no fields is 410. A repeating field in Excel
  or dBase is 720 ("format does not support repeating fields"), and the
  file is still written, every record with each repeating field's first
  repetition (x01 `x_xlsx`, `x_dbf`; x05 `x5_rep_xlsx`, a repeating
  summary).
- A related field holds one value per related record: FMPXMLRESULT puts
  them in one `COL` (one `DATA` each; no `DATA` without a related record);
  the other formats write the first on the record's row and one more row
  per further related record with only the related values.
- Groups: a group-by field makes a level only when the found set is
  sorted by it. The sort fields up to the last such group-by field are the
  group levels; each row is the first record of a group of the finest
  level; a level field shows its value only on the row starting its group,
  and no row follows the last group. Merge headers name a summary by break
  `S by Company`. Summaries by a group-by field that is not a sort field
  are left out of the file (x05 `gu`, `gk`, below: 20 of 37 columns).
  The rest is then an export without groups when the found set is sorted
  (`gk`); when it is unsorted (`gu`) every record is a row, but summaries
  without a break field show the grand summary on the first row only, as
  in a grouped export.

Summary fields [O: x05 on FileMaker Server's engine unless marked; every
value written equals the script engine's value of the field, or of
`GetSummary`, in the same found set; the summary rules are file-metadata.md
5.3]. x05's probes are named here by their scripts without `x5_` and the
format: `u`, `s`, `k` without groups, unsorted, sorted by the break field B
and by K only; `ub`, `sb`, `kb` the same with summaries by B; `g` grouped by
B, `g2` by B and K, `gu`, `gk` grouped by B but unsorted or sorted by K:

- Without groups a summary without a break field holds its value on the
  row's record. A running total or count is the value so far, restarting
  at each group of its break field only when the found set is sorted by
  that field. A fraction of total is the record's share, subtotaled only
  when sorted by the break field (empty otherwise). Every other operation
  is the found set's value on every row: Total and Count also in x01 and
  x03. FileMaker Pro wrote the same [O, FileMaker Pro 26.0.3.307, local:
  values 10, 20, 30 unsorted, current record 2. A tab-separated UTF-8
  Export Records without formatting and a Save Records as Excel of the
  records being browsed both wrote Total 60 on each row, running total 10,
  30, 60 and fraction .1666666666666667, .3333333333333333, .5].

  A second local FileMaker Pro 26.0.3 check on the same three records
  added Count of Price with Running count on and restart off. Save Records
  as Excel of the unsorted records being browsed wrote 1, 2, 3 in the
  running-count column beside running totals 10, 30, 60. With B current,
  Save: Current record wrote one row, B/Price 20/running count 2/running
  total 30 [O: [#372](https://github.com/OpenMaker-Dev/openmaker/issues/139)].
  On another copy, the three records were A/Price 10, A/Price 20 and
  B/Price 30. Running Count restarted for each group sorted by Name, and
  the found set was sorted by Name ascending. Browse showed 1 on the B
  record. Save Records as Excel of the records being browsed wrote running
  counts 1, 2, 1 beside running totals 10, 30, 60; Save: Current record
  with B current wrote one row with running count 1. The Running Total
  field did not restart in that copy [O: [#372](https://github.com/OpenMaker-Dev/openmaker/issues/139),
  local FileMaker Pro 26.0.3.307, workbook OOXML]. Enabling restart for
  Running Total in the same sorted found set wrote 10, 30, 30 and Current
  record B wrote 30 [O: [#372](https://github.com/OpenMaker-Dev/openmaker/issues/139)].

  On an unsorted copy with A/Price 10, B/Price 20 and C/Price 30, a List
  of Price wrote `10\n20\n30` on all three Excel rows and on Current
  record B (the separators are line feeds). After omitting B, it wrote
  `10\n30` on both remaining rows [O: [#372](https://github.com/OpenMaker-Dev/openmaker/issues/139),
  local FileMaker Pro 26.0.3.307, workbook OOXML]. An Average of Price
  weighted by Price wrote `23.3333333333333333` on all three rows and
  Current record B; after omitting B it wrote 25 on both remaining rows
  [O: [#372](https://github.com/OpenMaker-Dev/openmaker/issues/139)].
  On the same three Price values, Standard Deviation of Price wrote 10
  on each Excel row; with "by population" on, it wrote
  8.1649658092772603. With only the Price 10 record found, the sample
  standard-deviation cell was absent from the worksheet XML and the
  population cell was numeric 0. After Show All, Save: Current record
  with Price 10 current wrote one row with 10 and 8.1649658092772603
  [O: [#372](https://github.com/OpenMaker-Dev/openmaker/issues/139),
  local FileMaker Pro 26.0.3.307, workbook OOXML]. These current-record
  rows keep the found set's summary value.

  A Fraction of Total of Price subtotaled by Name, sorted by Name, wrote
  0.3333333333333333, 0.6666666666666667, 1, and Current record B wrote
  1. With that break field configured but the found set unsorted, all
  fraction cells were empty, including Current record B; a Running Count
  restarting by Name wrote 1, 2, 3 in that unsorted set [O: [sorted](https://github.com/OpenMaker-Dev/openmaker/issues/139),
  [unsorted](https://github.com/OpenMaker-Dev/openmaker/issues/139),
  local FileMaker Pro 26.0.3.307, workbook OOXML]. Grouped local exports
  of these options remain unmeasured.
- Without groups a summary by a break field is empty when the found set is
  unsorted or sorted by the break field (x01 `x_sorted`; x05 `ub`, `sb`).
  When it is sorted by other fields only (`kb`), every row holds the grand
  summary.
- In a grouped export a summary without a break field shows the grand
  summary, `GetSummary ( s ; s )`, on the first row only:
  - a running total or count: the found set's total or count, with restart
    too;
  - a fraction of total: 1, subtotaled too (and in the unsorted `gu`,
    where `GetSummary ( SFS ; SFS )` is empty);
  - any other operation: the found set's value.

  FileMaker Pro wrote the same [O, FileMaker Pro 26.0.3.307, local: values
  10, 20, 30 in groups A, A, B, sorted by the group field only, a running
  total with restart off and a fraction with subtotal off. A tab-separated
  UTF-8 Export Records grouped by it without formatting wrote
  `A\t60\t60\t1\rB\t\t\t\r` for Group, Total, Running Total, Fraction].
- A summary by a break field that is a level shows `GetSummary ( s ;
  break )` on the row starting the break field's group, empty on the other
  rows:
  - a running total or count: its value at the end of the group (with
    restart, the group's own);
  - a fraction of total: the group's share of the found set's total;
  - a subtotaled fraction: 1;
  - any other operation: over the group.

  With two levels each summary is by its own level (`g2`, B and K).
- A summary of a repeating field that summarizes repetitions individually,
  with repetitions of its own, holds per repetition the summary over that
  repetition. Exports write every repetition: without groups the record's
  own values; on group rows FMPXMLRESULT writes every repetition of the
  group's (or grand) value. Tab, comma and HTML write repetition 1 of a
  group row's or a break column's value and, from repetition 2 on, the
  row's record's own value, also where repetition 1 is empty (`|15|18`: a
  group row without the summary, or a summary by a break field without
  groups). FMPXMLRESULT writes empty repetitions on group rows without
  the summary. Excel refuses it as a repeating field (720, with the file).
- In the FileMaker Server engine's x05 grouped export, an invalid summary
  (`?`: the standard deviation of a group with one value) is written as
  an empty field in tab, an unquoted empty field in comma (`,,`), a `COL`
  without `DATA` in FMPXMLRESULT, `?` in HTML and a text cell `?` in Excel.
  In the local ungrouped Excel export above, the undefined sample
  standard deviation of a one-record found set instead has no worksheet
  cell [O]. The two export contexts have not been compared on the same
  file.
- A list holds the values in found-set order, empty ones left out,
  separated by returns: VT in tab and comma, CR in FMPXMLRESULT and Excel,
  `<BR>` in HTML; Excel writes it as text.

x05's records in creation order (K, B, N, W, NR): 1 b 5 1 `1|2|3`; 2 a 1 2
`4||6`; 3 b 4 3 `|5|`; 4 a 2 1 `7|8|9`; 5 c - 1 `||`; 6 c 3 2 `10||`.
Summaries of N:

- S total;
- SRT running total, restarting by B; SRT0 running total;
- SRC running count; SRCR running count, restarting by B;
- SF fraction of total; SFS fraction subtotaled by B;
- SA average; SWA average weighted by W;
- SMIN, SMAX;
- SSD standard deviation; SSDP by population;
- SL list.

Summaries of NR: SNR total (together); SNRI total and SNRRI running total,
both individually with 3 repetitions. In tab, sorted by B then K, without
groups (`-` empty, `|` the repetition separator GS):

```
K  SRT SRT0 SRC SRCR SF                SFS               SNRRI
2  1   1    1   1    .0666666666666667 .3333333333333333 4||6
4  3   3    2   2    .1333333333333333 .6666666666666667 11|8|15
1  5   8    3   1    .3333333333333333 .5555555555555556 12|10|18
3  9   12   4   2    .2666666666666667 .4444444444444444 12|15|18
5  -   12   4   0    -                 0                 12|15|18
6  3   15   5   1    .2                1                 22|15|18
```

Every row also holds:

- S 15, SA 3, SWA 3, SMIN 1, SMAX 5;
- SSD 1.5811388300841897, SSDP 1.414213562373095;
- SL `1¶2¶5¶4¶3`;
- SNR 55, SNRI `22|15|18`.

Unsorted, SRT and SRCR equal SRT0 and SRC (5, 6, 10, 12, 12, 15 and 1, 2,
3, 4, 4, 5) and SFS is empty. Sorted by B descending then K, record 5 comes
first: SRT and SRT0 are empty there and SRC, SRCR, SF and SFS are 0.

Grouped by B (`x5_g_tab`, sorted by B then K), the rows start at records
2, 1 and 5. The first row's grand values are S 15, SRT 15, SRT0 15, SRC 5,
SRCR 5, SF 1 and SFS 1, and the other summaries their found-set values
listed above. The summaries by B on each group's row (`?` written as
above):

```
   S SRT SRT0 SRC SRCR SF SFS SA  SWA                SMIN SMAX SSD               SSDP SL  SNR
a  3 3   3    2   2    .2 1   1.5 1.3333333333333333 1    2    .7071067811865475 .5   1¶2 34
b  9 9   12   4   2    .6 1   4.5 4.25               4    5    .7071067811865475 .5   5¶4 11
c  3 3   15   5   1    .2 1   3   3                  3    3    ?                 0    3   10
```

SNRI by B is `11 8 15`, `1 7 3` and `10` (repetitions 2 and 3 empty) in
FMPXMLRESULT, and `11|15|18`, `1|15|18`, `10|15|18` in tab.

OpenMaker Server, running the same scripts over the file FileMaker
generated for x05, differs in:

- repeating summaries (#513);
- `GetSummary ( s ; s )` and the grand row of restarting running summaries
  and subtotaled fractions (#514);
- a subtotaled fraction of an empty value that starts its group (#515);
- invalid summaries, error 15 and no file (#516);
- group-by fields the found set is not sorted by (#517);
- summaries by a break field without groups, sorted by other fields
  (#518);
- Excel and dBase with repeating fields, no file (#519).

Every other exported value of x05 is equal; FMPXMLRESULT's product
version, file name and record ids differ with the host.

Formats:

- Tab: rows end with CR (the last too), fields are tab-separated,
  repetitions GS (0x1D)-separated, a tab in a value is a space, a return a
  VT (0x0B).
- Comma: every value quoted, quotes doubled, tabs kept, returns VT; a
  number field writes its number (`007` is `7`, `abc` `0`); a related
  field without a record is empty and unquoted; extra related rows quote
  the other (empty) fields.
- Merge: a header row of bare field names (`City`, not `Addresses::City`),
  comma-separated and unquoted, then comma-separated rows with number
  fields as stored.
- HTML table: one tag per line, `<TH>` names, `<BR>` for an empty value and
  each return, `&amp; &lt; &gt; &quot;`, repetitions as a nested one-row
  table; extra related rows hold `<TD></TD>`, wrapped in a one-cell table
  before the first related value when the previous cell was a repetition
  table. Only the names and values are in the character set; with UTF-16
  the markup stays ASCII and the content type loses its `CHARSET`
  (`CONTENT="text/html;>`).
- FMPXMLRESULT: one line, `<?xml version="1.0" encoding="UTF-8" ?>` even in
  UTF-16 (little-endian, no BOM); `PRODUCT BUILD="08-26-2026"
  NAME="FileMaker" VERSION="Server 26.0.3"`; `DATABASE DATEFORMAT="M/d/yyyy"
  LAYOUT="" NAME="<file>.fmp12" RECORDS="<table count>"
  TIMEFORMAT="h:mm:ss a"`; `FIELD EMPTYOK MAXREPEAT NAME TYPE` with related
  fields as `Occurrence::Field`; `RESULTSET FOUND="<found count>"`; `ROW
  MODID RECORDID`; entities for `& < > " '`, `&#N;` beyond U+FFFF, control
  characters other than tab, CR and LF dropped.
- Excel (`XLXE`, also for Export Records): one sheet `Sheet1`, no header
  row from Export Records; numbers, dates from 1900 (day serials from
  1899-12-30) and times that are not negative (day fractions) are numbers,
  everything else shared strings (deduplicated, `xml:space="preserve"` for
  edge spaces), empty values no cell; per column a cell style
  (`s` = 6 + column) and, for text, date, time and timestamp columns, a
  number format numbered 168 + column (`@`, `mm/dd/yyyy`, `[hh]:mm:ss`,
  `mm/dd/yyyy hh:mm:ss`); `numFmts count` = 4 + columns, `cellXfs count` =
  6 + columns; fixed parts and their zip order as `om_transfer::export`
  writes them. FileMaker's time fractions agree with seconds × 1/86400 to
  about 15 significant digits; its further digits are not reproduced
  [O, Med]. The creation time is the export's (UTC).
- dBase III: fields `C` as wide as the longest value in characters (a
  related field: the longest last related value) with longer values written
  whole, `N` 19 wide (rounded to a whole number, left-aligned, 18
  significant digits past 18; a summary's text cut at 19), `D` `YYYYMMDD`
  (the year unpadded and the field then shorter); names upper-cased with
  `_` for spaces; language driver byte 0x02; after the end mark the found
  count as u32 little-endian; the header date's year is two digits.
- Character sets: Windows 1252, DOS code page 850, FileMaker's Mac OS
  Roman table from before the euro (0xDB is ¤, 0xBD the ohm sign), UTF-16
  little-endian with a BOM, UTF-8 without, GB 18030; a character the set
  lacks is dropped, except that € is `EUR` in Macintosh and Big-5 and a
  letter with a diacritic is the mark (`"`, `'`, ...) then the letter in
  Big-5 and EUC-KR (`"i`, `'e`; `´e` in EUC-KR, – is `-`). Shift-JIS
  (FileMaker maps Latin letters into row 0x85), Big-5 (Greek, kana) and
  EUC-KR (accented Greek) differ in characters these tables do not cover
  [O, gap].

### Imports

- Text sources: records end at CR, LF or CRLF; a blank line is a record of
  empty fields; tab files split at the stored delimiter with quotes as
  data; comma and merge files toggle quoting at every quote (`"q"x"y"` is
  `qxy`, ` "7" ` is ` 7 `), two quotes inside quotes are one, and a line
  break inside quotes is one CR; VT is a return and GS separates
  repetitions everywhere; a merge file's first row is always its names.
- A UTF-16 byte order mark decides the byte order whatever the character
  set; UTF-16 without one is little-endian; a UTF-8 BOM is skipped but the
  character set stays (UTF-8 read as Windows gives mojibake).
- Mapping by position through `84`, or by name with "match field names"
  (columns without a field of that name skipped); columns past the order,
  "don't import" and "-" columns, calculation and summary fields are
  skipped silently; a repeating field takes the column's repetitions up to
  its count, another field the first.
- Values are stored as written (`abc` in a number field, `13/45/2020` in a
  date field), except dates with a year of fewer than four digits, stored
  completed (`1/1/1` is `01/01/2001`, `1/1/24` `01/01/2024`).
- Add: one record per row. Update matching: match fields compared with the
  found set's records only; every match is updated; unmatched rows are
  added with "add remaining data", else dropped. Update found set: row n
  into the found set's record n; extra rows are added with "add remaining",
  else dropped, and no error either way (not 726/727). Updates write every
  mapped field, empty values too.
- Match values compare as the field's index keys them (x04): text without
  regard to case (`alpha` and `BETA` update `Alpha` and `Beta`), numbers
  by value (`007`, `2.50`, `-3.0` and `1e1` match 7, 2.5, -3 and 10). With
  "add remaining" a matched record also takes the row's match values (its
  `Alpha` became `alpha`); without it the match fields are not written.
  OpenMaker compares text by its English index key and numbers, dates,
  times and timestamps by value; dates were probed only with equal text.
- An update into another table than the window's (x04: the window on
  Contacts, the target Addresses): update found set works on that table's
  records in creation order (rows 1 and 2 into its records 1 and 2, a
  fourth row added with "add remaining"), update matching matches all of
  its records, and the window's found set stays as it was.
- Auto-enter on: unmapped fields get their auto-entered values; mapped
  fields keep the imported value, empty or not, also with "do not replace"
  off and with the per-field flag. Auto-enter off: nothing auto-entered.
- Validation "always" is checked: a failing row is not imported, the
  others are, and the step ends with 729. "Only during data entry" is not
  checked on import.
- Found set afterwards: the added and updated records (all of the found
  set for an update of the found set). An import into another table
  occurrence than the layout's (the stored target is honored) leaves the
  window's found set.
- Errors: missing source 100, an empty source 409; FileMaker Server does
  not write an `Import.log` in the Documents folder.
- Not reproduced: "split repetitions into separate records" from a text
  source (FileMaker Server kept only each field's first repetition and left
  the other fields empty); the XML source of a stored XML import did not
  resolve on the server (717; the file name is stored in the profile,
  variables not expanded), so XML imports are checked by round trip only.

### Rules, aborts and costs (OpenMaker's choices)

From the review of 2026-10-29 (C1-C5, P5-P8), with a test each:

- An imported row gets the file's rules once, with the import's options
  (`om_data::rules::Entry::import`: a script's write, "only during data
  entry" validation not checked, auto-enter only with "Perform
  auto-enter options" and then only for the fields the row does not set;
  a field's value list of a field's values checked, as Set Field would).
  The staged row keeps what they left (`OpenRecord::ruled` on OpenMaker
  Server, the provider's held import on the desktop): the import's own
  commit and Commit Transaction write it as it is, so stored calculations
  and indexes see the auto-entered values and a transaction's own Skip
  options do not apply to it. A step that changes the record later in the
  transaction gets the transaction's rules for its own cells, over what
  the import left. Before, the commit planned the rows again with its own
  options: a field validated not empty and filled by auto-enter refused
  every row (509), stored calculations lost the auto-entered values,
  Commit Transaction auto-entered what the import left out and refused
  values the import had accepted (507).
- A table whose rules the server cannot enforce (serials, ...) refuses the
  step with 3 before any row, as other script writes to it (before: 729
  after refusing every row).
- An aborted script, or one past its time limit, stops an import or
  export before its next row or record (`ImportTarget::interrupted`,
  `ExportSource::interrupted`) with error 1; the import's rows are dropped
  (its own transaction, or taken out of the script's) and the export
  writes no file.
- Costs: a commit of n records costs about n (the lock table is indexed by
  path and connection; each table's record list and each changed index's
  counts are written once per commit), a grouped export reads the
  found set once per summary and break field
  (`om_data::summary::Summary::group_values`) and one without groups once
  per summary (`Summary::values`). Release build on the
  development machine: a 20,000-row import 0.73 s, another client's lock
  request (op60) waiting
  at most 0.22 s meanwhile (before, it waited past the client's read
  timeout); a grouped export of 4,000 records 42 ms (before: 44.7 s).
  `om-server` `tests/transfer_timings.rs` measures both.
- What an export writes in the session's temporary folder is private
  (files 0600, folders 0700) whatever the server's umask, and the folder
  goes with the session also when its thread panics.
- Unique and existing values are checked against the rows staged so far
  through an index of the transaction's values (by table and field, kept
  current by every change), not one row at a time: an import of 8,000
  rows into a unique field took 12.0 s and takes 1.5 s (release).
- Open: an import still reads its whole source and stages every row in
  one transaction, so its memory grows with the file (up to the import
  limits below).

### FileMaker Server's folders

`Get ( DocumentsPath )` is `/opt/FileMaker/FileMaker Server/Data/Documents/`
and `Get ( TemporaryPath )` `/tmp/S<n>/`, a folder per session removed when
the session ends. A relative path (`file:name`) is in the temporary folder,
so an export to one is gone after the script. Full paths outside the two
folders are error 800 for an export; a missing folder is 800 unless
"Create folders" is on; the first usable path of a list is used and an
existing file is replaced. Save Records as Excel is error 3 on the server.
OpenMaker Server puts its Documents folder beside the `Databases` folder
holding the file (else beside the file) and its temporary folder under the
system's [I: placement].

### Paths on OpenMaker Server (OpenMaker's choices)

FileMaker Server's handling of links in its folders is not observed. A
security review (2026-10-29) showed OpenMaker Server's first boundary
check, which resolved `..` on the text and then opened the original path,
let `Documents/link/../x` and a dangling link in Documents write outside
it, and let an export write through a hard link. `om-server`'s
`scripts/host/files.rs` now resolves every path of a script step, the
data file steps' included, as follows:

- A full path must start with a folder as `Get ( DocumentsPath )` or
  `Get ( TemporaryPath )` shows it, or as the system resolves it; the most
  specific match wins. `..` before the folder is not resolved (such a path
  is refused).
- The rest is walked beneath the folder's open handle a name at a time
  with `openat` and `O_NOFOLLOW`. A symbolic link anywhere below the
  folder is refused, dangling or not, so the walk never leaves the
  folder; `..` returns to a folder already walked and is refused at the
  folder itself. What is opened is what was checked: no path text is
  looked up again. `openat2`'s `RESOLVE_BENEATH` gives the same on Linux
  only; one walk serves Linux and macOS (through `rustix`).
- An import opens a regular file with a single link (`O_NONBLOCK`, so a
  pipe never blocks the session). A file with another link is refused
  like one outside (error 100): whoever can write in Documents could link
  any file the server can read there (Linux restricts this with
  `protected_hardlinks`, macOS does not).
- An export's destination must be free or a regular file. The export is
  written to a new file beside it (`O_CREAT | O_EXCL | O_NOFOLLOW`) and
  renamed over its name, so a hard link's other names keep their
  contents and the destination is never half written. It is not synced
  (as the earlier `std::fs::write` was not): a sync took about a second
  per file on the development machine.
- The temporary folder (`OpenMaker-S<session>` in the system's temporary
  directory, one per script session with a random id) is made 0700 and
  used only when it is a directory of the server's user, never through a
  link; at the session's end it is removed only on the same condition.
  Files and folders a step writes in it are made 0600 and 0700.
- The data file steps (188-199) find their files the same way, the first
  usable line winning. FileMaker Server runs them (catalogs
  `script-step-compat.csv`); what it answers for a path outside its
  folders, a link or a file with another link is unmeasured, so these
  errors are OpenMaker's: Get File Exists is 0, Get File Size, Open Data
  File, Delete File and Rename File are 100 (as an import's), Create Data
  File is 800 (as an export's). Get File Exists counts a file or folder;
  the others take a regular file with a single link. Create Data File
  empties a file as an export writes (a new file renamed over the name,
  so a hard link's other names keep their contents). Open Data File opens
  for reading and writing, or for reading when the file may not be
  written. Rename File never replaces a name (`RENAME_NOREPLACE`, 800).
  The handles, limits, encodings, positions and their errors are the
  desktop's (script-runtime.md, Files), from
  `om_scripts::runtime::data_files`; a session's data files close when
  its script ends, is aborted, or its thread panics.

### Import limits (OpenMaker's choices)

FileMaker's limits for hostile sources are not observed. The review also
showed a worksheet row number aborting the process (a vector sized by it)
and a dBase header and a cell outside a row panicking. Every reader in
`om_transfer::import::read` now keeps to `om_transfer::import::limit`,
and a source past a limit is refused with its format's error, as a
malformed one: 708 for text and dBase, 718 for FMPXMLRESULT, 730 for an
Excel workbook.

- A source is at most 256 MiB (hosts read one byte more, no further), and
  its parsed rows take at most 1 GiB, counted as values are read (each
  value its slot, each repetition its string and text; a shared string
  each time a cell uses it). With the decoded text an import peaks at
  about 2 GiB.
- A row has at most 16,384 values in every format; a worksheet row and
  column reference must lie within `A1:XFD1048576`, with checked
  arithmetic. A row or cell without a reference follows the one before
  (ECMA-376; FileMaker's own workbooks always have them). Rows are no
  longer padded to the widest row: a missing value is empty to the
  import.
- A cell outside a row is 730. Rich text runs add up; phonetic guides
  (`rPh`) are left out.
- Workbook packages: the end of central directory record must end the
  file, follow its central directory (which starts with an entry) and
  list at most 10,000 entries (no zip64); the file may hold at most 16
  such record signatures. The `zip` crate 2.6.1 tried each earlier
  signature in turn when a directory failed to read, each try a pass over
  the file, so a file of fake records stalled it for minutes; zip 9 looks
  only in the last 64 KiB, and the limit stays so the cost does not
  depend on the reader. A part inflates to at most 100 times its
  compressed size (Apache POI's ratio; any part may reach 1 MiB) and the
  parts read to 512 MiB together. zip 9 checks a part's local header only
  when the part is read, and refuses a part that inflates past the size
  its directory entry declares.
- XML: elements nest at most 256 deep (libxml2's default), entity
  references are at most 32 characters and only the predefined ones and
  character references are known, so a DOCTYPE's entities are never
  expanded. Events are read one at a time, not collected first.
- dBase: the header must lie within the file and end its field
  descriptors with 0x0D; each field is 1 to 255 bytes and the fields fit
  the record length (which is not 0). Records are read up to the
  header's count or the end of the data. A date field is reformatted only
  when its text is ASCII (8 bytes of other text panicked).
- Matching imports compare match values through a hash table of their
  index keys (x04), so an update costs rows plus records, not their
  product.

`crates/om-transfer/tests/hostile.rs` has a test per hostile file and
`tests/fuzz.rs` feeds truncated and mutated copies of every fixture (and
each workbook part, repacked) through every reader and the import engine;
on the code before these limits it found 492 panics in 5,946 variants.

### Open

- Save Records as Excel's option slots (worksheet, title, subject,
  author, "use field names") are not mapped to slot numbers; the desktop
  client writes a header row of field names and `Sheet1`.
- "Apply current layout's data formatting" was probed only with fields not
  on the layout (no change); layout number and date formats are not
  applied.
- File locales other than English (date order in exports, FMPXMLRESULT
  formats) were not probed.
- Summaries in exports were measured on FileMaker Server's engine (x05)
  and for running totals and counts, fractions, List, weighted average,
  and sample and population standard deviations on local FileMaker Pro
  copies (Exports above). Not measured locally:
  - the other operations, repeating summaries and invalid summaries
    beyond the ungrouped one-record standard-deviation case;
  - grouped exports of the options above, and Current record Excel rows
    for operations other than the ones named above;
  - summaries in merge and dBase files;
  - summaries of a related table exported from the parent;
  - found sets sorted by a value list or by a related field.
- Exports past a format's limits are not probed. A dBase header holds its
  own length and the record length in 16 bits, so a file has at most
  2,046 fields and records of at most 65,535 bytes; past either,
  `om_transfer::export` refuses with error 3 and writes no file
  (OpenMaker's choice). An Excel export of more than 16,384 columns
  writes references past `XFD`. FileMaker's answer (error 736, a cut, a
  file) is unknown.
- The desktop client reads a whole import source before the 256 MiB
  check; its exports write through symbolic links, as FileMaker Pro may.

## Implementation notes (step text)

`om_scripts::display` renders each step as the Script Workspace shows it
(`step_line`, `script_lines`, `script_text`); `om_data` supplies a file's
names through `CalcSchema` (`StepNames`). Sources: the Claris script step
reference (each step's Options section and the examples, which show the
display form), FileMaker Pro 26's Script Workspace capture
(`re/captures/fm-ui/35`, `35b`: `Go to Object [ Object Name: Get (
ScriptParameter ) ]`) and the schema of section 5. No FileMaker binary
was read for this.

Rules:

- Options in brackets after the step name: `Name [ a ; b ]`, one space
  inside the brackets and ` ; ` between options, as 35 shows. Steps whose
  schema has no parameters show no brackets; steps with parameters but
  nothing to show keep `[ ]` (Claris: `Perform Find [ ]`, `Refresh
  Window [ ]`).
- Labels and order follow the Claris examples: `With dialog: On|Off`
  (inverse of `kNoInteract`), `Select` for "Select entire contents",
  `Restore`, `Pause: On|Off`, `Create folders: On|Off`, `Exit after last:
  On|Off`, `Lock: On|Off`; on/off steps show `On` or `Off` alone; a
  labelled calculation with nothing stored keeps its label (`Exit Script [
  Text Result: ]`, `Perform Script [ ... ; Parameter: ]`).
- Calculations are their formatted stream printed by `om-formula` with
  the file's names and decimal separator. A formula whose text was
  stripped (calc-encoding.md 10, item 7) is rebuilt from its Polish tree in
  OpenMaker's form: FileMaker's spacing, parentheses only where precedence
  needs them, Let variables as `_v0`, `_v1`.
- Fields show as `Table::Field`, `[n]` past repetition 1 and `[calc]` for a
  calculated repetition (slot 10); a variable target shows its name and
  repetition. A cleared spec (occurrence 0 or field 0, kept by Show Custom
  Dialog for unused inputs) is no field.
- Layouts show as `“Name” (Occurrence)`, scripts, tables and data sources
  as `“Name”`, file path lists as `“path¶path”` with their prefix
  (`file:`, `image:`; a path entered without one shows bare). A reference
  the file does not have shows `<unknown>`.
- Disabled steps start with `// `, comments with `# ` (an empty comment
  is a blank line); the enabled steps of If, Loop and Open Transaction
  blocks are indented one level per block (Else and Else If at their If's
  level). A disabled block step opens no block.
- Each option's byte range and schema parameter are kept (`OptionSpan`),
  so the editor knows which option a click lands on.

Checked on the 746 corpus files (`crates/om-data/tests/step_text_corpus.rs`,
`OM_STEP_TEXT_REPORT` writes each step id's texts): 3,273 scripts, 55,646
steps of 200 ids render with no panic, each line starts with its step's
name (after `// ` when disabled), every option range lies on its text; 74
steps name a reference their file lacks (MBS examples calling scripts and
layouts of other files, menu sets).

Formats not settled (to check in FileMaker Pro; one Script Workspace
screenshot or a Copy of these steps answers each):

1. Quotes: typographic `“ ”` around layout, script, table, data source,
   path and button label names (the Claris pages print straight quotes).
2. File paths: shown with their prefix (`“file:report.xlsx”`) or without;
   how FileMaker separates several paths (OpenMaker writes `¶`, so the
   list reads back).
3. Show Custom Dialog: `Title: … ; Message: … ; Default Button: “OK”,
   Commit: “Yes” ; Button 2: … ; Input #1: target, label, Password`.
   Which label shows when both a text label and a label calculation are
   stored (OpenMaker shows the text).
4. Go to Layout and Go to Related Record animations other than `None`
   (OpenMaker's order: slide in from left, right, bottom; slide out to
   left, right, bottom; flip from left, right; zoom in, out).
5. New Window styles other than Document, Dialog and Card (`C0100010`,
   the Tasks starter solution's word, is shown as `Style: Card`), and its
   window options; Go to Related Record's new window options.
6. Import Records and Export Records: the table, import action, worksheet
   and character set (`Add ; Mac Roman`, `Unicode (UTF-16)`) are not
   shown; Convert File's options.
7. Loop's flush values 2 and 3 (Minimum, Defer; section 9).
8. Steps shown by their schema names only (no Claris example of the full
   form): Send Event, Send Mail, Speak, Insert from Device, the AVPlayer
   steps, Configure Region Monitor Script, Configure Local Notification,
   Configure NFC Reading, Configure Machine Learning Model, the AI steps
   (212-222, 226, 227, 239-241), the PDF steps (242-247), Save Records as
   JSONL, Configure Persistent Data, Set Allowed Orientations, Set
   Dictionary and the plug-in step 186 (its function's name).
9. A comment with returns, and how a disabled comment shows.
10. A formula whose text was stripped (item 7 of calc-encoding.md 10).

### Text form of steps (copy and paste)

The Script Workspace copies steps to the system clipboard as text and
reads pasted text back into typed steps (`om_scripts::display`:
`steps_text`, `parse_steps`; FileMaker Pro has no such feature).

**What was not checked against FileMaker Pro.** FileMaker Pro is not
available on this machine. The text form is OpenMaker's own, built on the
display text above; its comment form (`# `, the `¶` and `\¶` escapes) and
Insert Text's quoted text were checked only against the corpus and
FileMaker Server's Save as XML (the oracle below), never against FileMaker
Pro's Script Workspace or clipboard. Nor was what Pro puts on the
clipboard when it copies steps (see "FileMaker XML on paste"). The
escapes are the text form's only: the workspace still shows a comment's
returns and Insert Text's typographic quotes as stored (item 9 above).

**Grammar.** One step per line:

```
line     = indent [ "// " ] ( step | comment | "" )
indent   = { TAB }                  ; one per enclosing If / Loop / Open Transaction
step     = name [ " [" options " ]" ]
options  = option { " ; " option }
comment  = "# " escaped-text        ; or "# (comment) [ options ]"
```

- `name` is the step's English name (longest match, any case), or
  `Unsupported step (n)` for an id FileMaker does not name.
- The options are the display options above, in the same order and with
  the same labels.
- Options split at `;` outside parentheses, brackets, `"…"` literals,
  `“…”` names and `/* */` and `//` comments.
- The text form writes exactly one space around each separator and inside
  the brackets. The reader removes that one space, so a calculation keeps
  any other whitespace it stores. Hand-typed text may use any spacing.
- A step continues over further lines while its brackets, literals or
  comments are open, so a calculation keeps its returns. A step still
  open at the end of the text (a stray quote or bracket) is its first
  line alone, which fails, and reading goes on with the next line.
- After an option that ends in a `//` comment, the text form starts a new
  line before the next separator, and the reader drops that line break.
- A blank line is an empty comment, and the final line break of the text
  is ignored.
- In a comment, a return is `¶`, a `¶` is `\¶`, and a backslash before
  either (or before another backslash) is doubled.
- In Insert Text's quoted text, a typographic quote (`“ ” „ ‟ ＂`) is
  escaped with `\` as `"` and `\` are, since every quote character closes
  a calculation's literal when it is read.

Copying always writes tabs. Reading takes any mix of tabs and spaces,
because indentation only shows nesting: the editor recomputes block indexes
when the steps are inserted.

**Names.** Fields (`Table::Field`, `[n]` or `[calculation]` for a
repetition), table occurrences, layouts (`“Name” (Occurrence)`), scripts
(`“Name”`) and tables resolve by name against the open file; a layout's
occurrence, when the text gives it, chooses among layouts of that name. A
name that several objects share (FileMaker lets layouts and scripts share
names) is never guessed: like a name the file does not have, it fails the
step. A step that names something the file does not have is not inserted
as written. A step
that does not parse is not inserted as written either. Both go in as
disabled comments holding the original text, marked in the editor, and the
workspace says how many lines failed. Calculations compile against the
file's names with no context table occurrence.

**Copying scripts.** Copy as Text in the scripts pane (or ⌘C there)
copies the selected scripts' steps one script after another, in the order
the pane shows them, an open script's draft in place of its saved steps.

**The workspace's own copies.** Step ids are file-local, so a copy also
keeps the steps themselves in the process, scoped to the workspace that
copied them. A paste in that workspace whose system clipboard text is still
the copy's inserts those steps whole; any other paste reads the text. Cut
copies the steps Delete removes with the selection (its blocks' other
steps).

**Limits.** A paste reads at most 8 MB of text and inserts at most 20,000
steps (the workspace's own copy too, though it is not read), and FileMaker
XML may nest elements at most 128 deep (its step XML nests about 20; an
empty element is a level too) and give an element at most 256 attributes
(it writes a handful); past any of them nothing is pasted and the editor
says why. A calculation past the formula parser's budget (151 active
expression or prefix-operator calls, counting the outer expression, or a
tree with more than 151 nodes on a path) fails its step like any
calculation that does not compile. A file path longer than its 14-bit
length prefix holds (16,383 encoded bytes) fails its step rather than be
stored with a cut prefix. A `Stored step` must be the step its line names,
and a pasted step carries no item keys (UUID, source UUID, owner, tags):
the Script Workspace gives a pasted step its own UUID, as it does a copy.

**Where text comes from.** `Stored …` options hold the ids of the file the
text was written from, and text carries no proof of which file that was.
The Script Workspace pastes its own copy whole only within the workspace
that copied it, while the system clipboard still holds its text; any other
text, edited copies and text from documents included, is read as of
unknown origin (`Origin::Unknown`). Then a `Stored …` option is taken only
where it leaves every file-local id the step holds (its field, layout,
table or script reference; the fields, occurrences and custom functions of
its calculations; slot fields and occurrences; sort orders, find
requests, file references, import and export orders) as the open file's
names give them. A plug-in function a calculation calls counts too: its
id is the plug-in's, not the file's, but a call the text cannot name
(`<Function Missing>`) is code the text does not show, so it fails the
step. Where the text names a part the options hold under other
ids, the bytes can only be another file's, so the part is taken as the
names give it. A calculation that names ids is compared as code: it is
taken as its text compiles here only when its stored code is that code
with other ids in the same places, one for one (as sets, First Name and
Last Name could not be told from the same two fields the other way
round). Stored code that differs from what its text compiles to, or whose
ids do not stand one for one for the named ones, fails the step: its text
and code disagree. Ids the text names nothing for, and a reference whose
name the file lacks, fail the step; reference bytes no flag uses are
dropped. Only the text form's own round trip reads with
`Origin::ThisFile`, taking every option as written.

**Lossless options.** When a step's display line alone does not give the
step back, the text adds `Stored …` options after the displayed ones:

| option | carries |
|---|---|
| `Stored flags: 0x…` | the step flags |
| `Stored value: n` | the step value |
| `Stored reference: <28 hex>` | the 14-byte inline reference (stale or unusual filler) |
| `Stored slot n: <base64>` / `-` | slot `[00 n]` as stored, or none |
| `Stored key xx: <base64>` / `-` | step-level key `xx` (sort order `0a`, find requests `06`, specs `84`-`88`) |
| `Stored without text: n` | slot n's calculation has no formatted stream (stripped) |
| `Stored data: <base64>` | the whole data subtree, when its root differs |
| `Stored step: <base64>` | the whole step (descriptor and data); the display options are ignored |

- Base64 is the URL-safe alphabet (`-`, `_`): it holds no `/`, so it never
  reads as a comment.
- A node is its wire form as the only child of a root (file-format.md).
- These options override what the display options set, in this order:
  data, slots, keys, value, reference, flags.
- The workspace shows FileMaker's display line. Only the copied text
  carries `Stored …` options, and only when they are needed.

**What is compared.** A pasted step is a new step, so it has no data key
and no item UUIDs (`d7`-`da`). The editor recomputes the indexes that
depend on the step's place (block steps' step value, Else If's End If
index, Go to Record and Go to Portal Row's End Loop index). Everything
else must match byte for byte (`display::canonical`).

**Defaults the reader writes.** These are the values most corpus steps
store:
- references with zero filler (1,488 of about 4,000 field specs, 1,090 of
  1,300 member and script keys);
- Set Variable's repetition `1` when none is shown;
- Show Custom Dialog's three commit flags as one byte each, with a label
  calculation beside each text label;
- Pause/Resume Script's choice 1 for a duration;
- On/Off steps' `kTurnOn` alone, without `kSetOnOff`.

**Forms FileMaker shows.** FileMaker's Save as XML of steps pasted this
way (the XML oracle below) asks for these forms, and the reader writes
them [O]:
- Insert Text's text in slot 0;
- a variable target's repetition as a field's: a number is the spec's
  repetition, anything else a calculation at `0x20 + n` with spec
  repetition 0 (the formatter shows a variable's repetition the same way);
- "Specified: By name" of Perform Script and Perform Script on Server in
  their canonical form (section 10: `[00 1]` = 1, the name calculation in
  slot 2, `0x800 | 0x2000000`);
- the step value that shows the reference (FileMaker's XML shows nothing
  at 0): New Window's layout and Truncate Table's table as section 10
  records them, 1 with `kMenuSelection` for a named window (Move/Resize,
  Close, Select Window, Set Window Title: all 36 corpus steps store 1; the
  XML shows 1 and 2 alike) and 1 with `kMenuSelection` for Import Records'
  file source (11.3).

**Corpus round trip** (`crates/om-data/tests/step_text_corpus.rs`,
ignored by default): all 55,643 steps of the 745 corpus files (the
templates, FileMaker Server's samples and the MBS examples) read back from
their text to the same step (0 failures).

| how the step came back | steps |
|---|---|
| by its display line alone | 35,057 (63.0%) |
| with `Stored …` options | 19,912 |
| carried whole (`Stored step`) | 674 |

Read as of unknown origin into their own files, 53,380 of the steps
(95.9%) come back the same and 48 more but for unused reference bytes;
2,215 fail: calculations whose stored code names fields but whose text
does not compile here (1,942, nearly all MBS plug-in calls: this machine
does not know the plug-in's function names), stored sort orders, find
requests and import and export orders (190), and 83 others: references
their own file does not have, scripts and data sources of other files,
and field targets the text shows only as a calculation. These counts
predate two stricter checks and were not rerun: plug-in calls the text
cannot name now fail whether or not they name fields, which should take
in most of the 14,170 MBS steps below, and calculations are compared as
code with their ids in order rather than as sets of ids.

Why options are needed:
- **14,170 steps:** MBS plug-in functions. The files cannot name them
  (`<Function Missing>`), so their calculations travel as stored bytes.
- **1,763 Set Field steps:** references with non-zero filler.
- **Show Custom Dialog:** input and password shapes that vary from writer
  to writer.
- **FileMaker Server's samples:** formulas stored without their text
  (`Stored without text`).
- **Sort orders and find requests:** stored specs, carried as
  `Stored key 0a` and `Stored key 06` (about 140 steps). A readable form
  for them is open.
- **Flag variants:** `kSetOnOff` with On, and `kMenuSelection` on Go to
  Layout.
- **Carried whole:** mostly comments whose text node another writer stored
  differently (a bare length header, an empty slot), and the corpus'
  unnamed step ids.

## Implementation notes (Script Workspace)

`om-gui`'s Script Workspace (`crates/om-gui/src/script_workspace/`) edits
scripts through `om-scripts`' edit API; these are its decisions.

**Oracle: the Scripts menu bit.** FileMaker Server's `FMDeveloperTool
--saveAsXML` (`tools/fms-script-harness/run.sh -x`) of the Contacts and
Tasks templates writes each catalog member's `KMemberFlags` as its XML
`Options` value with the attributes it implies. Contacts: folder 9 and both
scripts 8 are `hidden="True"`, the folder end 2 `hidden="False"`; Tasks: the
same for its folder (9), seven scripts (8) and folder end (2). So bit 3
(`0x8`) is "not in the Scripts menu" (`Script::in_scripts_menu`), which is
why FileMaker's Scripts menu lists no script for Contacts (capture 22). The
desktop's Scripts menu now leaves such members out.

**Order and folders.** The catalog lists its members in display order
`[17, 1, 03, n]` (as `meta::scripts`); a folder runs from its member (flag
1, an empty step list) to its end marker `--` (flag 2). New folders are
written that way; new separators are named `-`.

**What is saved when.** As in FileMaker, the scripts pane's changes (new,
renamed, duplicated, deleted and moved members, Grant Full Access
Privileges, Include in Scripts Menu) are committed at once: the workspace
sends the whole new catalog to `DataProvider::save_script_catalog`. Step
edits are drafts until Save (⌘S, or Save in the close prompt), which sends
the script to `save_script`. A provider that cannot save a change (the
native provider refuses new folders and separators; scripts.md, "The
catalog index") answers with an error; the
workspace shows it in an alert, as FileMaker shows a failed save, and
keeps the draft (its tab keeps the `*`). A new member the host stores
under another id than the workspace gave it (the host's next issued id)
is followed by its new id.

**Tabs.** A single click in the scripts pane shows the script in a preview
tab (the eye in the tab bar); an edit or a double click keeps it open, so a
second script opens beside it. Closing a tab or the workspace, or running a
script, with unsaved changes asks "Save changes to script “…”?" with Don't
Save, Cancel and Save. Switching scripts never discards a draft: the edited
script keeps its tab.

**Editing.**

- New steps go after the last selected step. If, Loop and Open Transaction
  come with End If, End Loop and Commit Transaction. A new Go to Related
  Record, here or as a button's action, starts on `<Current Layout>` (step
  value 1): FileMaker's stored steps hold 1 or 5 (11.8), and value 0 with
  no layout key names none [I: the step value FileMaker Pro gives a new
  step is not measured].
- Deleting or disabling a block step takes the other steps of its block
  along (Else If, Else and End If; End Loop; Commit Transaction), keeping
  the steps inside.
- Any edit whose result does not nest is refused and explained under the
  steps.
- Duplicated and pasted steps get new data keys and a new `KUniqueID`.
- The clipboard holds the steps in the process (OpenMaker's own format);
  the system clipboard gets their text.
- A click on a calculation option of the selected step edits it in place.
  A double click on it opens Specify Calculation, elsewhere on the line the
  step's options.
- Return or typing starts a new step line that completes step names
  (prefix matches first); Return or a click on a name inserts that step.
  `#` starts a comment.
- Keys: ⌘↑/⌘↓ move the selection, ⌘D duplicates, ⌘/ disables, Delete
  removes, ⌘Z/⇧⌘Z undo and redo, `+` opens the options.

**Steps pane.** FileMaker's categories in its order with the catalog's
category of each step (`script-steps.csv`, all 216 offered steps fall in
one). Within a category the steps are alphabetical, as the Claris reference
lists them: the order FileMaker's pane uses is not documented (open).
Obsolete ids (`OBSOLETE_STEP_IDS`) are not offered. Favorites last for the
session.

### Options dialogs

The gear (or a double click on a step) opens the step's options
(`script_workspace/options.rs`), built from its parameter schema (section
5); `pickers.rs` holds the dialog chrome and the pickers, `sort_dialog.rs`
and `find_dialog.rs` the stored specs' dialogs. Sources: capture 35b,
measured at its 1.74 px per point (title bar 28 pt, body 435 pt wide,
22/21 pt side insets, right-aligned labels, 208 pt fields, 84 x 20 pt push
buttons, rows 34 pt apart, 14 pt description lines, OK 14 pt right of
Cancel and in line with Specify…), the Claris pages `sort-records`,
`perform-find` and `sorting-records-options`, and the step catalog. No
FileMaker binary was read.

**Text.** The title is `"<Step Name>" Options` with straight quotes, as
35b shows. The description is 35b's for Go to Object ("Specify an object on
the current layout to make active. Object names are assigned using the
Inspector command in Layout mode."), and the Claris one-line summary
(`script-steps.csv` `description`, `om_scripts::step_info`) for every other
step. A labelled row's label is its schema name with the words capitalized
(`Object name` -> `Object Name:`; words with a capital already, such as
`cURL`, and joining words stay); checkboxes keep the schema's sentence case
(`Select entire contents`, `With dialog`). "With dialog" is a checkbox named
so, checked for On (the Claris pages call the option With dialog, On or
Off). Restore is named as Claris names it: "Specify sort order" (Sort
Records), "Specify find requests" (Enter Find Mode, Perform Find,
Constrain and Extend Found Set), "Specify import order" and "Specify export
order"; elsewhere "Restore".

**Rows.** One per schema parameter, in schema order, without `Collapsed`
and the cached indexes:

| kind | row | written with |
|---|---|---|
| Flag (one flag or numeric location) | checkbox | `set_flag`, `set_number_bool` |
| Choice (step value or slot list value) with labels | pop-up menu | `set_parameter` (step value and `kMenuSelection`), `set_list_value`; the parameter's flag bits are set with it (Loop's Flush is read only with `kOption`) |
| Calculation (one slot) | text field of the calculation text and Specify… (Specify Calculation above) | `set_calculation`; flag bits of the parameter follow the calculation (Set Field By Name's `kFieldSpecByCalc`, Read from Data File's amount) |
| Text (one slot; Insert Text's slot 1 or 0, the one holding the text) | text field | `set_text` |
| Number (one slot) | text field | `set_number` |
| Field, Target (one spec) | the field (`Table::Field`) or variable, Specify… (Specify Field, Specify Target) | `set_field_spec`, `set_variable_target`; a variable target made a field loses its name text at `0x20 + n` |
| Layout (`mReserved`) | the layout's name, Specify… (Specify Layout); for Go to Related Record `<Current Layout>` for step value 1, first in its Specify Layout list | `set_member_key`; Go to Layout and Go to Related Record also get step value 5; New Window gets value 5 without `kMenuSelection`. Go to Related Record's `<Current Layout>`, once chosen, is step value 1 (with `kMenuSelection`, as the choice setter writes it) and no layout key, as the step text and XML store it, also for a step that read as no layout (value 0, or 5 without a key); a value 1 step shows it whatever key it still holds. A layout by name or number calculation (values 3 and 4, inferred from Go to Layout, 11.8) keeps the row as stored |
| Script (`mReserved`) | the script's name, Specify… (Specify Script) | `set_script_key`; a file reference under `20` is removed (the script is this file's) |
| TableOccurrence | the occurrence (Go to Related Record, `[00 0, 11]` as one key) or base table (Truncate Table, `mReserved`), Specify… (Specify Table) | `set_slot_child`, `set_member_key`; Truncate Table also gets value 1 and `kMenuSelection` |
| Window (Move/Resize Window, Close Window, Select Window, Set Window Title: the step value and slot 0) | a pop-up Current Window / Window Name, and with Window Name the "Window Name" calculation (slot 0) with Specify… | the step value 0 or 1, without `kMenuSelection` (no stored window step has it; the oracle's `WindowReference` is the step value and the calculations only); `set_calculation(0)`, removed when back to Current Window, as FileMaker's XML importer stores no name there. A stored value 2 (by name) shows Window Name and is kept unless changed |
| File paths (`[00 0, 82]`; not with a data source `20`, `kFileReference`) | the first path, Specify… (Specify Output File for the steps that write a file: Export Records, Save Records as PDF, Excel, JSONL and Snapshot Link, Export Field Contents, Save a Copy as and as XML, Create Data File; Specify File for the others): a text box, one path per line | `set_path_list`: the 11.0 path stream as FileMaker's XML importer writes it (each line `kind:path` stored as its kind, an empty volume and the path; oracle, "Stored specs" below), `kFilePath` set, or both removed for an empty list. A line without its kind (`file:`, `filemac:`, `filewin:`, `filelinux:`, ...) is refused in the dialog: FileMaker's dialog takes one (a lone `$path`), but how it stores one is not established |
| Button labels and commit (Show Custom Dialog: text slots 0-2, `[00 3..5, 03]`) | "Default Button", "Button 2", "Button 3" text fields, each followed by its "… commits data" checkbox | `set_text` of slot 0, 1 or 2 (empty removes it); `set_number_bool` of slot 3, 4 or 5 when its checkbox changed, one byte as FileMaker writes it. A checkbox shows what the runtime reads: without a stored byte the default button commits and the others do not [I]. A button labelled in the dialog with no byte stored gets its byte with the label, the checkbox's value, as FileMaker Pro stores one beside every labelled button (its new step: OK 1, Cancel 0); the step text reads an absent byte as not committing. A button with a calculated label (slots 5-7), which the dialog shows instead of the text, keeps its label as stored |
| Input fields (Show Custom Dialog: input n's target at slot n - 1, its label the calculation in slot n + 1, "Use password character" at `[00 n - 1, 03]`) | for each of inputs 1-3: "Input n", the field or variable with Specify… (Specify Target); "Input n label", a calculation with Specify…; "Input n uses password character", a checkbox | the target as Field and Target rows write it (`set_field_spec` of slot n - 1, input 1 in `mReserved` with `kFieldSpec`; a variable with `set_variable_target`, its name at `0x20 + n - 1`); `set_calculation` of slot n + 1; `set_number_bool` of slot n - 1 when the checkbox changed. An input stored as the empty key (table 0, field 0), as FileMaker Pro stores an unused input of a new step, shows no target and stays as stored. An input is set or changed, not removed |
| Export order (Export Records, `85` with `kRestore`) | "Specify export order" checkbox and Specify… (Specify Field Order for Export, below) | `85` rewritten with `ExportOptions::encode` when the order changed; `kRestore` with the checkbox |
| Output format (Export Records, `86`) | "Type": a pop-up of the types whose profile has no options (Tab-Separated Values, Comma-Separated Values, Merge, HTML Table, Excel Workbooks (.xlsx), DBF), Tab-Separated Values when none is stored | `86` written as `Profile::new` of the type with the stored field names word, with `kExtendedData` (the parameter's flag in the step catalog), only when the type changed; a stored XML or undecodable profile is kept as stored. A type chosen while the step has no output file is refused in an alert: FileMaker reads `86` only with `kFilePath` (`GetProfile`, the step-level keys above) |
| Data source (Import Records, a file: step value 0 or 1, no data source `20`) | the first path, Specify… (Specify File) | `set_path_list` as for File paths, and the data source kind: step value 1 with `kMenuSelection`, as every stored file source has it (11.3), value 0 without it when the list is emptied |
| Import order (Import Records, `84` with `kRestore`, `87`) | "Specify import order" checkbox and Specify… (Import Field Mapping, below) | `84` and `87` rewritten with `ImportOptions::encode` and `ProfileEx::encode` where they differ from the stored bytes, `86` written when none was stored; `kRestore` with the checkbox |
| State (Allow User Abort, Set Error Capture: `kTurnOn` with `kSetOnOff`) | checkbox "State", checked for On | `kTurnOn` set or cleared; `kSetOnOff`, which some files store beside an On (`0x30000`) and Tasks' does not (`0x20000`), stays as stored [I: what `kSetOnOff` adds is not established] |
| Bounds (New Window and Move/Resize Window: slots 1-4) | four calculation rows, "Height", "Width", "Top", "Left" (Claris's New Window Options and Move/Resize Window Options), each with Specify… | `set_calculation` of slots 1, 2, 3, 4 (height, width, top, left; Tasks' Add Related Record stores 584 and 668 in slots 1 and 2; the runtime reads Move/Resize Window's from the same slots [I: Move/Resize Window's slot map is Low confidence in the parameter catalog]) |
| Pause (Pause/Resume Script: the step value, slot 0) | pop-up "Pause": Indefinitely (0), For duration (1); with For duration the "Duration (seconds)" calculation | the choice with `set_parameter` (step value and `kMenuSelection`); `set_calculation(0)`. Back to Indefinitely removes the duration, which the runtime and the step text read as the choice; For duration chosen, or a duration cleared, without one is refused at OK; a stored step left as it was OKs unchanged |
| Window style and options (New Window: the word at `[00 0, 03]`) | "Window Style": a pop-up of the words FileMaker is observed to write, Document (`00370602`, its default with seven options), Dialog (`80050204`), Card (`C0100010`); Document when none is stored | `set_number(0)` of the chosen word at `[00 0, 03]`, where Tasks stores it and the step text's parser writes it, only when it changed. A stored word other than these three is kept as stored: the option bits are not all told apart (New Window style word, above), so no option is offered on its own |
| Options (Go to Related Record: `kOption`, `kTurnOn` with slots 0-4 and `[00 0, 03]`, `kRestore`, `kOption2`; 11.8) | checkboxes "Use external table's layouts" (taking input only while the related table is another file's, as FileMaker Pro 26.0.3 greys it out for this file's; a checked box stays clearable), "Show in new window", "Show only related records" and, shown with the last, "Match all records in current found set"; shown with "Show in new window": "Window Name", "Height", "Width", "Top", "Left" (calculations) and "Window Style" (as New Window's) | `set_flag` of each flag when its checkbox changed; `set_calculation` of slots 0-4; the style word with `set_number(0)` only when changed. A row shown with an unchecked box is not written, and turning a box off keeps what its rows stored, as FileMaker keeps a stale style word |
| Replace with (Replace Field Contents: `[00 1, 03]`, `[00 2, 03]`, `[00 3, 01]`, slot 0; 11.8) | pop-up "Replace with": Current contents (1, also when nothing is stored), Serial numbers (2), Calculated result (3); with Serial numbers "Initial value" and "Increment by", both 1 when nothing is stored as the runtime reads them, and "Update serial number in Entry Options" (`[00 4, 03]`); with Calculated result its calculation; always "Perform auto-enter options for fields", FileMaker Pro 26.0.3's label, checked unless `kRestore` is set with `[00 6, 03]` = 0 (the XML reader's "Skip auto-enter options") | the choice with `set_number(1)`, 4 bytes, as the step text's parser writes it; `set_text(3)`, `set_number(2)`, `set_calculation(0)`, each only when changed. Calculated result without a calculation is refused at OK (the runtime would end the step with error 5). The two checkboxes show what is stored and take no input: Pro greys "Update serial number in Entry Options" out for a field without an auto-entered serial number, and reads "Perform auto-enter options for fields" checked with nothing stored [O: #798's Pro check]; what its own editor writes for either is not measured. Another stored choice, or "Use Entry Options" stored (`[00 5, 03]` set, which would override the initial value and increment), keeps the parameter as stored [I: the widths of `[00 1, 03]` and `[00 2, 03]` in FileMaker Pro's own steps are not measured] |
| Records (Save Records as PDF: the step value under its PDF options; Save Records as Excel: slot 1, two bytes; 11.7) | pop-up "Records": Records being browsed (1, also when nothing is stored), Current record (2); Save Records as Excel's only while Restore is checked, as FileMaker and the runtime apply it only then | PDF: the step value and `kMenuSelection`, as the step text's parser writes it; Excel: `set_number_u16(1)`, as FileMaker Server's XML writer reads it (11.7); each only when changed. Any other stored value keeps the parameter as stored; so do the PDF and Excel options this row does not cover. FileMaker Pro 26.0.3 read both back as Current record [O: #821's Pro check]. Print's records have a row of their own (below). |
| Print's records (the step value, and `printStyleSetting` in the macOS `M_PM` chunk of the stored print settings `88`; 11.6) | pop-up "Records": Records being browsed (1, also when nothing is stored), Current record (2), shown only while "Specify print options" (Pro's label for Restore) is checked | Both places, as FileMaker Pro 26.0.3 writes them [O: #843's twins]: the step value without `kMenuSelection`, and the digit in place, one byte in the stored piece that holds it; only when changed. Read-only unless `M_PM` holds exactly one `printStyleSetting` of 1 or 2 and no comment or CDATA section (Pro's have none), and unless the record is within the runtime's 1 MiB bound. "Specify print options" cannot be checked on a step with no `88`: Pro asks for the options in its Print dialog. Unchecking it clears only `kRestore`, keeping the step value, `87` and `88`, as Pro does [O: #843]. A blank-record value (3-6) keeps the parameter as stored. The runtime prints a stored current record only with `kRestore`, else the records being browsed [I: from Pro's dialog preselection, #843] |
| Send Mail (Send Mail by the email client: calculations 0-4, `[00 0..3, 03]`, `[00 0, 82]`; 12.3) | checkbox "One email per record" (`[00 3, 03]`); "To", "CC", "BCC" (calculations 0-2), each followed, while one email per record is unchecked, by "Collect … addresses across found set" (`[00 0..2, 03]`); "Subject" (3), "Message" (4); "Attachments", the path list as File paths | `set_calculation` of slots 0-4; the checkboxes with `set_number_bool` (one byte); the attachments with `set_path_list` (`[00 0, 82]` with `kFilePath`); each only when changed [I: the widths FileMaker Pro writes for `[00 0..3, 03]` are not measured]. A step sending by SMTP (`[00 4, 03]` set) or OAuth (`[00 7, 03]` set) keeps the parameter as stored, as do the SMTP and OAuth options; OpenMaker does not run Send Mail (#245) |

Step rules: Perform Script and Perform Script on Server show "Specified: From list / By name" (`[00 1]`
1 by name; the by-name bits `0x800` and `0x2000000` set with it, cleared
when back to the list, the name calculation in slot 2 kept), then the
script or the "Script Name" calculation. Go to Layout's first pop-up lists
original layout, Layout, Layout Name by calculation and Layout Number by
calculation (values 1, 5, 3, 4); "Specified Layout" shows with Layout,
"Calculation" (slot 5) with the calculated ones (both labels OpenMaker's).
Go to Record/Request/Page and Go to Portal Row show Exit after last with
Previous and Next, the number with By calculation, With dialog otherwise.
Animation pop-ups list the transitions in the step text's order (item 4
above); with no slot 6 stored they show None, which is how the runtime
reads it, and write nothing unless another is chosen (138 of the
templates' 160 Go to Layout steps store no slot 6). A calculation
parameter named Repetition with nothing stored shows 1, as 35b does; it
is written only when changed.

**Kept as stored.** A parameter no row edits shows "Kept as stored" when
something is stored for it, and is never touched: compound parameters
(Send Mail, PDF and Excel options, the AVPlayer and mobile steps, ...),
parameters spread over several slots (Perform JavaScript's parameters, the plug-in step's
parameters), enumerations without labels (the corpus's list choices, New
Window and Go to Related Record style words other than the three above),
menu sets,
privilege sets, data source references (Import Records' and Convert
File's sources, Open File, Perform Script in another file, and a file path
list beside a data source), Convert File's import order, Save Records as
Excel's export order, page setup and print settings.

**Specify Output File and Specify File.** FileMaker's dialog (Claris,
"Specify Output File" and "Creating file paths") holds the path list and,
for some steps, Add File…, the output file type, Automatically open file,
Create email with file as attachment and Create folders. OpenMaker's holds
the path list and its explanation: the three options are rows of the
step's own options, the file type is Export Records' Type row, and Add
File… (a file chooser) is not offered [OpenMaker's choice; the
dialog's look was not compared with FileMaker Pro].

**Lossless OK.** OK applies only the rows whose value differs from the one
the dialog opened with, through the setters above, to the step as it was:
OK without a change answers the same descriptor and data node, byte for
byte (unit tests on Go to Object, Set Variable, Set Field, Go to Layout,
Perform Script, Sort Records with both key forms, Perform Find with a
modification count, Commit Records, Insert from URL, and Export Records
with and without its type and order). A calculation is
compiled from its text at OK (no context occurrence, as steps store them);
one that does not compile shows FileMaker's message for its error in an
alert and the dialog stays open. A calculation answered by Specify
Calculation is stored as answered while its text is not edited again.

**Sort order** (Sort Records' Specify…, 11.1). FileMaker's Sort Records
dialog: the table pop-up and its fields (creation order) at the left, the
"Sort Order" at the right (drag to reorder; a double click takes a field
out), Clear All, Move and Clear between them; for the selected entry
"Ascending order", "Descending order", "Custom order based on value list"
with the file's value lists, "Reorder based on summary field" (Specify
Field), "Override field's language for sort" with the `Locale::Type` names
of 11.1.2; then the spec's "Keep records in sorted order" and "In
ascending order, sort blank records last" (`kSortBlanksLast`, which 11.1.3
shows applies in both directions; the label is Claris's). A moved field is
repetition 1; an entry keeps what the dialog does not show. OK in the
dialog checks "Specify sort order"; the step's `0a` is rewritten (with
`SortSpec::encode`, FileMaker's XML importer's form) only when the order
differs from the stored one, so an unedited order keeps its key form.
Unchecking "Specify sort order" clears `kRestore` and keeps `0a`, as
FileMaker keeps stale specs. A stored order that does not decode is kept
and its Specify… disabled.

**Export order** (Export Records' Specify…, 11.4). The controls Claris
names for Specify Field Order for Export ("Exporting data"): the table
pop-up and its fields at the left, Move, Move All, Clear and Clear All, the
"Field export order" at the right (drag to reorder; a double click takes a
field out), "Group by", "Output file character set" with the
`TExpCharacterSet` names, and "Apply current layout's data formatting to
exported data". A moved field is repetition 1 with an empty `sum_by`; an
entry keeps what the dialog does not show (a summary field's break field,
its repetition). Group by lists the stored group-by fields ("Unsorted"
without), kept as they are: at design time there is no sort to group by.
A new order is empty, UTF-8, with `ExpUseNameAlias` 1 as FileMaker's XML
importer writes it [I: a new order from FileMaker Pro's dialog was not
compared]. OK in the dialog checks "Specify export order"; `85` is
rewritten only when the order differs from the stored one. A stored order
that does not decode is kept and its Specify… disabled. The dialog's sizes
and places follow Sort Records', not a measurement of FileMaker Pro's.

**Import order** (Import Records' Specify…, 11.3). FileMaker reads the data
source when the order is specified, to list its fields (Claris, "Import
Records"); so does OpenMaker: the first path of the Data Source row as the
options give it that names an existing file (`file:` in this user's
Documents; a path with a variable does not resolve at design time), parsed
with the step's profile as Import Records parses it when it runs. At most
16 MB is read there (only the source's fields are needed; OpenMaker's
choice), and a source of more than 65,535 fields does not fit the order's
count. Without a readable source, with a type OpenMaker does not read
(FileMaker files, folders, ODBC, Excel 97-2004), or with a stored `86`,
`87` or `f0` that does not decode (a default would write over it),
Specify… says why in an alert. A source with
no stored profile takes one from its extension (`.tab`, `.txt`, `.csv`,
`.mer`, `.dbf`; written to `86` with the order); an Excel profile names a
sheet and a file, how FileMaker fills them for a new step is not
established, so `.xlsx` without a stored profile is refused. Import Field
Mapping (Claris's name; the run-time dialog of Import Records with dialog
has the same controls) lists the source's fields (their names, or the first
record's values) with a target field pop-up each (the target table's data
fields, no containers, or "Don't import"), the target table pop-up, the
three import actions with "Add remaining data as new records", a Match key
per mapped field with "Update matching records in found set", "Don't import
first record (contains field names)", "Perform auto-enter options while
importing" and, for text sources, the character set (the source is read
again with it). OK needs a mapped field, and with matching a mapped key. It
changes only what was changed: a column whose target or key was set gets
the entry the run-time dialog writes (Import, Match or Don't import); an
untouched column keeps its stored entry (a Don't import entry's field key,
an unimportable one, only Import and Match following a changed action);
entries past the source's columns stay; `MATCH_NAMES` ("matching names")
is cleared only when targets were set; a new order gets an entry per
column. An order arranged by matching names, on a source that names its
fields (merge, XML), shows the fields those names match, as Import Records
maps them (a field's first entry says whether it is excluded or a match
key); edited, it becomes a custom order with every column's shown target.
Choosing another target table clears every column's target, also when the
first table is chosen again. `84` and `87` that decode to the answered value keep their stored bytes.
`87` is rewritten when stored (the names row changed only when its
checkbox was) or written when the names row was set (names row 0, the
predefined id -1, the type's delimiter); `86` from the extension only for a
new or remapped order. `f0` stays as stored; an unchanged OK leaves the
step byte for byte. The layout follows
the run-time dialog, not a measurement of FileMaker Pro's.

**Find requests** (11.2). Specify Find Requests lists each request's action
(Find Records, Omit Records) and criteria (`Contacts::Status: [In
Progress]`, joined by `; `) with New, Edit, Duplicate and Delete; Edit Find
Request has the Action pop-up, the request's criteria (Field, Criteria),
"Find records when:" (or "Omit records when:") with the table pop-up and
fields, the repetition, "Criteria:", Insert Operator and Add, Change,
Remove (the Claris Perform Find page's controls). A request holds one
criterion per field and repetition: Add on a field already there replaces
its criterion. The step's `06` is rewritten with `encode_requests` only when
the requests changed (rewritten rows carry no `fc`); a stored set with a
cell the decoder cannot read (an unpacked key) is kept and not edited.

**Pickers.** Specify Field: the table occurrence pop-up (occurrences in
display order, the step's occurrence first shown), its table's fields in
creation order, a Repetition field (a spec with repetition 0, whose
repetition is the calculation in slot 10, shows 1 and keeps 0 until the
repetition is edited); a double click answers. Specify Target
adds Field and Variable radio buttons and a variable name (`$` or `$$`
required). Specify Layout: the layouts in display order with folders,
separators and each layout's occurrence at the right. Specify Script: the
catalog's scripts from `CalcSchema::scripts` in display order with folders
and separators. Specify Table: occurrences, or base tables for Truncate
Table. Folders and separators are shown, not chosen. Return answers OK and
Escape Cancel in the dialog on top.

**Colors.** These dialogs use 35b's colors: title bar `#FCFCFC` over a
`#F6F6F6` body, `#28C840` zoom button beside inactive `#E1E1E1` ones, white
text fields with a light edge and a `#89A2E8` focus ring, selected text on
`#C1CCF7`, and the default button's deeper blue (`#3648D0` to `#1E29C8`,
drawn flat as `#2B3CCC`). `theme::DEFAULT_BUTTON` (`#3E62EC`) is lighter;
no capture shows another enabled default button to settle which the rest
of the app should use.

Open (to check in FileMaker Pro 26; one capture each answers them):

1. Which steps' gears open a dialog like 35b and which a popover, and the
   controls each shows (With dialog as a checkbox or an On/Off pop-up).
2. Specify Target's form (OpenMaker's: Field and Variable radio buttons),
   Specify Field's field type icons and its Current Table entry.
3. Whether OK with Repetition 1 shown and unchanged stores a repetition
   calculation (OpenMaker stores nothing).
4. Perform Script back from By name to From list: what is cleared
   (OpenMaker writes `[00 1] = 0`, clears `0x800` and `0x2000000`, keeps the
   name calculation); Go to Related Record's step value when a layout is
   picked.
5. Whether FileMaker rewrites an edited sort order's absent keys as `00` or
   `02 01 00`, and whether rows it writes for edited requests carry `fc`.
6. Insert Operator inserts at the caret in FileMaker; OpenMaker appends.
7. The Sort Records radio labels "Ascending order" and "Descending order"
   (Claris names only "Custom order based on value list").
8. Specify Field Order for Export as the step's options open it: its
   layout, whether it has Move All and Clear All, the table pop-up's
   entries (Current Layout, Current Table, related tables), Group by at
   design time, the character set and `ExpUseNameAlias` of a new order, and
   the type Specify Output File gives a new Export Records step.
9. Import Field Mapping as Import Records' options open it: its layout and
   arrows, the type FileMaker gives a new step's source (and what an Excel
   source's profile holds), whether it writes `87` with predefined -1 or 0,
   and whether choosing a data source sets step value 1 before an order is
   specified.


The inline editor requires a calculation slot on the clicked display span.
A selector such as "Specified: From list" can share a schema parameter with
a name formula, but clicking its label selects the step or opens its options;
it does not edit that hidden formula. Options apply only visible rows, so
choosing From list after editing a name retains the chosen script.

### FileMaker XML on paste

The Script Workspace's paste also reads FileMaker's XML for steps
(`om_scripts::display::parse_xml_steps`, `display/xml.rs`; the step text's
`parse_steps` routes text starting with `<` there): a clipboard snippet
`<fmxmlsnippet type="FMObjectList">`, the `<Step>` elements of
FMDeveloperTool's Save as XML (FMSaveAsXML 2.3), and whole exported
documents.

**FileMaker Pro's clipboard was not observed.** Pro is not available on
this machine, so which pasteboard flavor it copies script steps as, and
whether that is `fmxmlsnippet` text at all, is unknown here. OpenMaker
reads XML only when it arrives as plain text, and assumes Save as XML's
`<Step>` shape inside the snippet wrapper (open point 1 below); nothing
here is checked against a step copied in Pro. Every `<Step>` in document order is one pasted step,
layout objects' single steps included. A small reader parses elements,
attributes, character data, CDATA, the five entities and character
references; it keeps characters as written (FileMaker writes a return in
an attribute as `&#13;`). Sources: black-box runs of FileMaker Server
26.0.3 only (below); no FileMaker binary was read.

**Oracle.** Three directions, all through `tools/fms-script-harness`:

1. *Export*: Save as XML (`run.sh -x`) of the 66 DynamicTemplates files
   (6 templates in 11 languages, account `Admin:`), each `<Step>` read
   with the file's own names and compared with the stored step
   (`canonical`).
2. *Probe*: a copy of the Tasks template written with OpenMaker's writer
   whose first script holds, for every named step id, the bare step, one
   step per schema parameter with only that parameter stored, targeted
   steps (find requests, sort orders, repetitions, window and account
   choices, by-name scripts, comments with line breaks) and 185 steps of
   step text; exported and compared the same way.
3. *Import*: the probe's `<Step>` XML applied as an FMUpgradeTool patch
   (`run.sh -p`, FileMaker's own XML importer) to a fresh Tasks copy, the
   stored steps compared with what OpenMaker reads from the same XML.

Plus a round trip: every step that reads is written back into a file, the
file is exported again, and each step's XML must equal the original's
(hashes, indexes and UUIDs aside). `crates/om-data/tests/step_xml_oracle.rs`
holds the comparison, the probe writer and the round trip (ignored tests,
paths in `OM_XML_ORACLE`, `OM_XML_PROBE`, `OM_XML_ROUNDTRIP`,
`OM_XML_ROUNDTRIP_CHECK`).

| run | steps | exact | equal but key filler | failed (not read) | mismatched |
|---|---|---|---|---|---|
| templates, script steps | 935 | 814 | 121 | 0 | 0 |
| templates, layout object steps (read only) | 462 | | | 0 | |
| probe | 955 | 724 | 32 | 152 | 47 |
| round trip, probe / Tasks / Contacts | 803 / 61 / 3 | equal XML: 803 / 61 / 3 | | | |

Key filler (the bytes after a key in `mReserved` and in slot field specs,
2.1) is not in the XML; the reader writes zeros (FileMaker's importer
writes uninitialized bytes, `46`, `7f`, `bebfef`). The probe's mismatches
are stored data the XML writer does not show and no flag announces: list
choices and numbers of the one-parameter steps, a variable name beside a
field target, a calculation hidden by a text label, stale specs without
Restore (dropped, as FileMaker's importer drops them), and the step text
forms listed at the end.

**Element mapping** [O unless marked]:

| XML | stored |
|---|---|
| `Step id`, `enable="False"` | step id; disabled = negated id |
| `Options` (decimal, signed 32-bit: `-2147483648`) | the flags word, taken as is; the Booleans only stand in when it is missing |
| `Boolean id=bits value` | those bits (inverted for "With dialog" 0x80, PSOS "Wait for completion" 0x100, Convert File "Open File" 0x100); without `id`: "Exit after last" 0x2000, "With dialog" 0x80 inverted |
| `Calculation position=n` (anywhere) | compiled into slot n against the open file's names; `datatype` is the result type |
| `FieldReference` + `TableOccurrenceReference` (Parameter `FieldReference` or `Target`) | target field spec (`mReserved`, slot n for Show Custom Dialog input n + 1) |
| `repetition` of a field or variable target | digits = the spec's repetition; anything else = spec repetition 0 and the calculation in its `position` (10 for a field, 0x20 + n for a target) |
| `Variable value` (in `Target`) | variable target: variable field spec, name text at 0x20 + n |
| `Comment value`, `Text value`, `Flow value`, `Name value` | text slot 0 (Insert Text's text is slot 0: the writer shows no other) |
| `Restore value` | `kRestore` |
| `List value` (Parameter of a step with one step-value parameter) | step value; `value="0"` stores nothing |
| `Records`/`Portal` `List value` | Go to Record/Portal Row step value, with the Booleans and number calculation inside |
| Loop `List value` | 1 Always: nothing; 2, 3: slot 1 list value |
| Perform Script, PSOS `List value` | 1 From list + `ScriptReference`; 2 By name: `[00 1]` = 1, name calculation slot 2 |
| `ScriptReference id name` | script key (serial id); a `repetition` inside holds the parameter calculation (Install OnTimer Script, callbacks) |
| `LayoutReferenceContainer value` + `LayoutReference id name` / `Calculation` / `Label` | step value + layout key in `mReserved` / the slot-5 calculation; the copy inside a new window repeats it |
| `Animation value` | slot 6 list value, `None` stored as 0 (templates and importer agree) |
| `WindowReference`: `Select kind`, `Name`, `Bounds`, `Rename`, `Style value`, `Options` | step value = kind (0 current, 1 and 2 by name); calculations by position; the style word = `[00 0, 03]`; `Options` spells out the style word |
| Go to Related Record `Related`: `TableOccurrenceReference`, `Options matchFoundSet ShowRelated` | occurrence key at `[00 0, 11]`; the `Options` attributes repeat flags |
| `SortSpecification maintain blanksLast` / `Sort type` / `PrimaryField` / `SummaryField` / `ValueListReference` / `LanguageReference id` | `0a` (11.1): absent summary `02 01 00` keys, repetition 1; no `ValueListReference` = value list key `00`; id -1 = key `02 01 00` (shown as `Custom`); `Descending` order bit |
| `FindRequest` / `FindRequestSet` / `FindRequest action index` / `find criteria` + `FieldReference repetition` | `06` written with `encode_requests` (11.2.6) |
| `UniversalPathList` / `ObjectList` / `Location` | path list `[00 0, 82]` (`kind:path`, empty volume) and `kFilePath` |
| `Encoding type` (directly in `ParameterValues`) | data file steps' step value |
| Show Custom Dialog `ButtonN value` + `Boolean Commit` + `Calculation`; `FieldN` → `Parameter Target`, `Boolean Password`, `Parameter Label` | text label slot n-1 (no label calculation beside it: the importer writes none), calculated label slot n+4, commit `[00 n+2, 03]`, password `[00 n-1, 03]`, label calculation slot n+1 |
| Perform JavaScript `Parameter` | the calculations and their count at `[00 0, 03]` |
| Truncate Table `List value` + `BaseTableReference` | step value + base table key |
| `CustomMenuSetReference id` | menu set key by id (no lookup by name) |
| `DataSourceReference id=0`, `PrivilegeSetReference id=0`, `HiddenStepsCount`, `UUID`, `SourceUUID`, `OwnerID`, `hash`, `index`, `name`, `membercount`, `UUID=` | nothing (a pasted step gets its own UUIDs and data key) |
| other wrappers without attributes (`Name`, `value`, `Start`, `End`, `height`, nested `Parameter type`) | what they hold |

**Names only.** Table occurrences, fields, layouts, scripts, base tables
and value lists are looked up by the XML's `name` in the open file
(`StepNames`). The XML's ids are the source file's: the same id may name a
different object here, or nothing yet and a later object. So a name the
file lacks, or that several of its objects share (the XML gives a layout
no occurrence to choose by), fails the step: it goes in as a disabled
comment holding its XML, marked with the reason, which survives editing,
saving and copying. Menu sets have no lookup by name: the XML's id is
taken only when the file's menu set of that id has the XML's name (the
providers name no menu sets yet, so a custom menu set fails).

**Never half filled.** A step fails (`Pasted::Failed` with that step's XML
and the reason) when it holds an element, attribute or text the reader
does not map, a calculation that does not compile, or when `Options` says
the step holds data the XML did not give: a calculation, field, layout or
table, script, text, paths, file reference, stored spec, or a list choice
with no choice element. FileMaker's writer leaves out what a step's
choices do not use, and always leaves out Go to List of Records' record
ids and Execute SQL's query. Not read (they fail): plug-in steps (186),
Send Event, Send Mail, Speak, Perform AppleScript, Perform Find/Replace,
Insert File's options, Import and Export orders, Save Records as Excel and
PDF options, Print PDF, Insert from Device, the AVPlayer play and option
lists, the AI steps' lists, Replace Field Contents (its "Skip auto-enter
options" reads differently in the writer and the importer), the account
steps' non-zero choices (the XML shows a Boolean for step values 1, 2 and
4 alike), scripts and data sources in other files, privilege sets, and
`LayoutReferenceContainer External="True"`.

**FileMaker's importer normalizes** what it stores from the same XML (the
import run): it recomputes the flags from the Booleans and the data
(dropping "Current file only" 0x80000000, `kSetOnOff` 0x10000 and flags
of hidden data), stores a field's or variable's repetition `1` as a slot
calculation, Loop's Always as slot 1 = 1 with `kOption`, Perform Script's
"From list" with `[00 1]` = 0, `kMenuSelection` and a zero script key, an
empty `0a` for Sort Records without a spec, empty slot pushes for path
parameters, and text with control characters in its chunked form. The
paste reproduces the stored step the XML came from instead (templates:
FileMaker Pro's own forms), so these differ there by design.

**Seen on the way** (oracle evidence for other parts):

- 11.1.1: the Contacts and Tasks "Sort by Name" steps store the value
  list key as `00`, which is also what FileMaker's importer writes; a
  value list key `02 01 00` shows as `Sort type="Custom"` with
  `ValueListReference id="-1"` and reads back without the entry's
  direction.
- The step text's reader wrote forms FileMaker's XML does not show:
  Insert Text's text in slot 1 (shown empty); a variable target's
  repetition as a calculation with spec repetition 1 (shown as 1);
  Perform Script and PSOS by name without `0x800 | 0x2000000` (shown as
  From list), PSOS's name in slot 1 (not shown); a named window, New
  Window's layout, Truncate Table's table and Import Records' source
  without the step value that shows them. It now writes the forms of
  "Text form of steps", "Forms FileMaker shows". Go to Layout without an
  animation stores no slot 6, as 138 of the templates' 160 Go to Layout
  steps do (22 store slot 6 = 0).

Open (FileMaker Pro is not available on this machine):

1. FileMaker Pro's clipboard snippets were not seen. The reader assumes
   their `<Step>` elements have Save as XML's (V2) shape; architecture.md
   names a V1 and a V2 step serializer, and a snippet in another shape
   fails step by step with the element it does not know. Inferred only:
   the `fmxmlsnippet` wrapper itself, `Options`-less steps (flags from the
   Booleans), and whether Pro's paste stores the importer's forms above or
   the original's.
2. The stored values behind the account steps' Booleans ("Expire
   password", Activate/Deactivate) and `External="True"` in Go to Related
   Record. (Replace Field Contents' options are now specified in 11.8; the
   reader still refuses the step.)
3. Data sources (`DataSourceReference`, file references `20`) and menu
   sets are not looked up by name (`StepNames` has no lookup for them), so
   steps naming them fail.
