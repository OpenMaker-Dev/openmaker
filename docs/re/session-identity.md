# Authenticated account privilege metadata

This describes the root file's signed-in account. Per-related-file logins
and the client's display name are separate workstreams. Names and `Get`
answers are descriptive metadata, not authorization for an operation.

## 1. Login result and account reference

Evidence levels: **[O]** observed protocol or file structure, **[B]** binary
interface observation recorded in the private prototype, **[C]** FileMaker
26.0.3's code read statically (from bin; each section names the functions),
**[I]** inference or an implementation boundary, not a measured desktop
answer.

The security catalog root is **decimal 23** (raw segment `17` hex).
Successful Login's `result_key` names the account member under `[23,1,5]`:
`[u8 segment_length][raw segment]`. FileMaker Server's reply to a login
as the lab test file's account `02` carries `01 02` (capture s154,
network-protocol.md 5.6.5) [O]. A reference must contain exactly
one nonempty segment; serial bytes remain opaque rather than being
converted to numeric ids [I: strict framing policy]. The account's child
raw `0b` uses the same framing and selects its set at `[23,2,5,P]` [B, O].
Do not match a display name or substitute another account when the login
key, reference or member is missing.

A signed-in client keeps the privileges its Login gave it. Claris's
FileMaker Pro help says that privilege set changes made while clients use
a shared file "do not take effect until the client closes and reopens the
file", that a client whose password changes is not interrupted and needs
the new one the next time it opens the file, and that a client whose
account is made inactive keeps its use of the file until it closes it
("Managing security"; FileMaker Pro 18 help, "Creating and editing account
access"). This is documented, not measured. What happens to a client whose
account is deleted is not documented (2.6).

In a native FileMaker Pro 26.0.3 check on 2026-11-13 (#735), after a
successful hosted Login as an account in [Data Entry Only], Pro requested
the whole member `[23, 1, 5, A]` with op90 six times. OpenMaker Server
answered each with lock error 200. Pro fetched the Contact Details layout
and its theme but left the canvas blank [O: private
`re/work/issue-735/capture-entry/conn-000`, server log and screenshot].
With the Contacts starter's Admin account on the same server build, Pro
received its account member and drew that layout [O: `capture/conn-000`].
In a second [Data Entry Only] pass with the same file and account, the
server returned a filtered copy of that member without the password
verifier `06`. Pro requested the member once, continued to record queries
and drew Contact Details [O: `capture-entry-fixed/conn-000`, server log
and screenshot]. Thus the refused account-member download caused the
blank canvas in this test. In the final pass, OpenMaker also withheld the
derived checksum `04`. Pro drew Contact Details under both [Data Entry
Only] and [Read-Only Access] with a 122-byte own-member reply and no
retries [O: `capture-final/conn-000` and screenshots]. Under [Data Entry
Only] Pro created and deleted a disposable record and disabled Manage
Database; under [Read-Only Access] it showed the existing record and
disabled New Record, Delete Record, Edit Layout and Manage Database [O:
screenshots and user interaction]. FileMaker Server's exact reply to a
non-full-access account's own member has not been captured. Omitting
`04`, `06` and unknown account children is OpenMaker's security policy
[I].

The prototype's written interface observations (macOS arm64 DBEngine):

| Interface | Address | Established fact | Level |
|---|---|---|---|
| `DBUserAccount::GetPrivilegesKey` | `0x19e5224` | account child raw `0b` | B |
| `HBPosition::Get(HBAbstractKey&, ...)` | `0x142f160` | `PStringProxy` preserves one-segment key framing | B |
| Support `PStringProxy` constructor | `0x126b58` | writes after the key's internal total-length byte | B |
| `DBPrivileges::GetName` | `0x190cc10` | custom set's child raw `10` is its name | B |

No implementation is copied or translated from those interfaces.

## 2. Privilege set members `[23, 2, 5, P]`

Evidence [C]: FileMaker 26.0.3's engines, read statically. Linux
`libDBEngine.so` (DWARF; nm addresses): `DBPrivileges::GetTableAccess`
`0x109ae00`, `SetTableAccess` `0x109f050`, `GetFieldAccess` `0x109df30`,
`SetFieldAccess` `0x10a0980`, `GetFileOptions` `0x109d930`,
`SetFileOptions` `0x109c6d0`, `GetApplicationAccess` `0x10a0dd0`,
`GetDefaultApplicationAccess` `0x10a0d20`, `AccessQuad::AccessQuad(uchar)`
`0x10a0cc0`, `DBAccess::Load` `0x109c0c0` and `Save` `0x109c340`,
`DBValueListCatalog::GetDefaultPrivileges` `0x1220240`,
`DBFileUser::HasAccess` `0xf2a080`, `HasNormalMenuAccess` `0xf313d0`,
`GetExtendedPrivSetString` `0xf34c50` and `GetValue` `0xf398b0`; the DWARF
enums `DBAccess::RowLevel`, `DBAccess::Selector`, `DBAccessLevel` and
`DBPrivileges::FileFlags`. Mac x86_64 `FMEngine` (nm addresses):
`FMLayoutCatalog::GetPrivileges` `0x2cd702`, `GetLayoutDataAccess`
`0x2cd778`, `FMScriptCatalog::GetPrivileges` `0x491016`,
`XMLPrivileges::GetMenuCommandVal` `0x8cbb2`,
`ObjectTransferPrivilegeSets::CreateAccessNode_Layout` `0x1c983a` and
`CreateAccessNode_Script` `0x1c8dfc` (Save as XML), and
`FMFileUser::GetValue` `0x5f752e`. Evidence [O]: the sets of the 66
FileMaker Pro 26.0.3 starter files (the three built-in sets only), of
four example files of the MBS plug-in with custom sets and of the s33
privilege fixture (calc-functions.md 9). The custom sets hold per-table
rules (two with a calculation limiting delete), per-layout and per-script
entries and five different other-privilege values; each reads as
described below. FileMaker was not run to change a set.

| Key | Content |
|---|---|
| `02` | record access of tables without an entry of their own (2.1) |
| `03` | description, text (`KDescription`) |
| `04` | checksum, 16 bytes (`KCount`; issue #474) |
| `05, T` | record and field access of base table `T` (2.1) |
| `09` | layout, script and value list access (2.2) |
| `0a` | other privileges, a big-endian u16 (2.3) |
| `0c` | "Must be changed every N days", a big-endian u16; absent or 0 means never (local-files.md 2.2) |
| `0d` | "Minimum password length", a big-endian u16 |
| `10` | name, text; built-in sets 1, 2 and 3 have English names in the shipped starters and German names in blanks made under the German UI (2.7) |
| `d8`, `d9`, `fc`, `fc01`-`fc03` | member UUID and modification stamps, as for every catalog member |

### 2.1 Record and field access

A record access value (`DBAccess`) is two bytes [C]:

- byte 0 holds four 2-bit fields, from the high bits down: create
  (bits 7-6), view (5-4), edit (3-2) and delete (1-0). Each is 0 no, 1 yes
  or 2 limited by a calculation (`RowLevel` `kNo`, `kYes`, `kLimited`).
- byte 1, bits 1-0: the access of fields without an entry of their own,
  a `DBAccessLevel`: 1 no access, 2 view only, 3 modifiable (0 unknown).
  FileMaker keeps bits 7-2 when it rewrites the value; they are 0 in every
  observed set.
- A missing value reads as no for all four actions and no access to
  fields.

`[23, 2, 5, P, 02]` applies to every table without an entry: the record
privileges menu's "all tables" choices and the "[Any New Table]" row
[I: dialog wording]. A table with custom privileges has a node
`[23, 2, 5, P, 05, T]`, `T` the base table's key (`80 01` is table
32769), holding:

| Key | Content |
|---|---|
| `02` | the table's record access value |
| `05, F` | field `F`'s access, one `DBAccessLevel` byte; fields without one take byte 1 of `02` |
| `06`, `07`, `08`, `09` | the calculation limiting create, view, edit and delete, present only for an action that is limited: a stored formula (`04` context occurrence key, `05` formatted stream, `06` compiled stream; calc-encoding.md) |

A limited action whose formula does not load counts as no [C]. The
default value at `02` sits beside the layout access node `09`, so a
"limited" default would read `09` as its delete calculation; FileMaker's
dialog limits actions per table only [I].

Observed values [O]: `55 03` everything, fields modifiable ([Full Access]
and [Data Entry Only]); `54 03` no delete; `14 03` view and edit only;
`10 03` view only ([Read-Only Access]: its fields default to modifiable,
but edit is no); `56 03` create, view and edit, delete limited by the
formula at `09`.

### 2.2 Layout, script and value list access `09`

Each entry is one byte holding four `DBAccessLevel` fields
(`DBPrivileges::AccessQuad`), from the low bits up: first (bits 1-0),
second (3-2), third (5-4) and fourth (7-6) [C]. The defaults are
`[23, 2, 5, P, 09, 04]` for layouts, `09, 11` for scripts and `09, 21`
for value lists: the catalogs' top-level keys `KLayoutDefs`,
`KScriptDefs` and `KValueLists`. An item with privileges of its own has
an entry under `09` whose single key segment is the catalog key followed
by the item's key: `09, 04 13` is layout `13`, `09, 11 80 59` script
`80 59`. An item without an entry takes the default; a set without the
default reads no access (first, second and third 1, fourth 3) [C].

| Catalog | First field | Second field |
|---|---|---|
| layouts | the layout: 3 modifiable, 2 view only, 1 no access | records via this layout: 3 modifiable, 2 view only, 1 no access |
| scripts | 3 modifiable, 2 executable only, 1 no access | not read |
| value lists | 3 modifiable, 2 view only, 1 no access | not read |

The layout, script and value list code examined reads only these fields;
the third and fourth are 1 and 3 in every observed entry. Observed bytes
[O]: `df` modifiable layout, modifiable records ([Full Access]); `de`
view-only layout, modifiable records ([Data Entry Only], [Read-Only
Access]); `dd` no access to the layout; `d7`, `d6`, `d5` a script or
value list modifiable, executable only (view only), no access. Save as
XML reports a catalog's access as custom when any item entry exists,
and otherwise from the default's first field [C].

### 2.3 Other privileges `0a`

`0a` is a big-endian u16 stored XOR `0x6000` [C: `GetFileOptions` and
`SetFileOptions`], so a set whose two menu bits are clear on disk has all
menu commands. After the XOR its bits are (`DBPrivileges::FileFlags`; the
matching Edit Privilege Set option is inferred from the names and the
dialog's labels [I], except where marked):

| Bit | Name | Option |
|---|---|---|
| `0x0001` | `kCanCreateValueLists` | Value lists: Allow creation of new value lists |
| `0x0002` | `kNoIdleDisconnect` | "Disconnect user from server when idle" is **off** |
| `0x0004` | `kCanChangePassword` | Allow user to modify their own password |
| `0x0008` | `kCanModifyExtensions` | Manage extended privileges |
| `0x0010` | `kCanModifyAccounts` | Manage accounts that don't have Full Access |
| `0x0020` | `kCanManageDatabase` | Manage database, data sources, containers, and custom functions |
| `0x0040` | `kUNUSED` | none |
| `0x0080` | `kCanManageCustomMenu` | Manage custom menus |
| `0x0100` | `kPrintAllowed` | Allow printing |
| `0x0200` | `kExportAllowed` | Allow exporting |
| `0x0400` | `kDataEntryOverride` | Allow user to override data validation warnings |
| `0x0800` | `kCanCreateScripts` | Scripts: Allow creation of new scripts [C: Save as XML] |
| `0x1000` | `kCanCreateLayouts` | Layouts: Allow creation of new layouts [C: Save as XML] |
| `0x2000`, `0x4000` | `kEditMenusAllowed`, `kOtherMenusAllowed` | Available menu commands: both All, `0x2000` alone Editing only, neither Minimum [C: `GetMenuCommandVal`] |
| `0x8000` | `kAllowOpenQuicklyLayoutsAndScripts` | Allow Open Quickly access to layouts and scripts |

`0x4000` alone is not a dialog choice [I]; the DDR writer reports it as
Minimum, and normal menu access needs both bits [C: `HasNormalMenuAccess`].
A set with full access passes every check whatever its bits [C:
`HasAccess`]. There is no "Manage scripts" option: scripts are governed
by script access (2.2) and `0x0800`.

Stored values [O]: `9f ff` [Full Access] (every bit); `03 04` [Data
Entry Only] and [Read-Only Access] (printing, exporting, own password,
all menus, disconnected when idle); `03 00` (printing, exporting, all
menus); `02 00` (exporting, all menus); `60 00` (nothing, minimum menus);
`1f 0d` (new layouts, scripts and value lists, override validation,
exporting, printing, extended privileges, own password, all menus).

#### What denied printing and exporting do

FileMaker Pro 26.0.3, on disposable local copies of a Contacts file whose
custom set has all menu commands and one of the two options off [O:
[#651](https://github.com/OpenMaker-Dev/openmaker/issues/214), 2026-11-14
20:18Z to 23:15Z, one reading per row]. Each script read
`Get ( LastError )` right after the step. Errors were not captured: the
Admin control runs showed FileMaker's prompt for a canceled step [I from
that prompt].

| Step or command | Option off | FileMaker Pro |
|---|---|---|
| [Print (43)](https://github.com/OpenMaker-Dev/openmaker/issues/214) | printing | no dialog and no alert; 3 |
| [Save Records as PDF (144)](https://github.com/OpenMaker-Dev/openmaker/issues/214) | printing | no chooser and no alert; 3 |
| [Print Setup (42)](https://github.com/OpenMaker-Dev/openmaker/issues/214) | printing | opens Page Setup, as with the option on; 1 after Cancel |
| [Export Records (36)](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | no chooser and no alert; 3 |
| [Save Records as Excel (143)](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | no chooser and no alert; 3 |
| Export Field Contents (132): [step](https://github.com/OpenMaker-Dev/openmaker/issues/214), [menu](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | saves the field; 0 |
| Copy All Records/Requests (98): [step](https://github.com/OpenMaker-Dev/openmaker/issues/214), [menu](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | "Your access privileges do not allow you to perform this action." with OK, the clipboard unchanged; 200 |
| [Save a Copy as (37)](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | opens its chooser; Save fails with the alert that the file could not be created on this disk; 800 |
| Save Records as Snapshot Link (152): [step](https://github.com/OpenMaker-Dev/openmaker/issues/214), [menu](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | saves a `.fmpsl`; 0 |
| Import Records (35) from a CSV file: [step](https://github.com/OpenMaker-Dev/openmaker/issues/214), [menu](https://github.com/OpenMaker-Dev/openmaker/issues/214) | exporting | imports; 0 |
| [Import Records from a FileMaker file](https://github.com/OpenMaker-Dev/openmaker/issues/214) open under an account whose exporting is off | that source's exporting | the privilege alert before Specify Import Order; 714 |

With printing off, [Preview Mode](https://github.com/OpenMaker-Dev/openmaker/issues/214) is entered and draws the
page; its Print button is disabled. Unmeasured: whether these steps alert
with errors captured, Copy All Requests in Find mode, and what a hosted
file does.

#### Further local menu and import checks

FileMaker Pro 26.0.3 on disposable Contacts copies also gave these results
[O, #651]. Menu readings are enabled states in the stated Browse context,
not proof that every enabled command completes. The closed-source import
checks distinguish matching credentials from a source requiring another
account; they do not establish the identity of an unprompted source session.

| Set and context | FileMaker Pro result |
|---|---|
| Minimum, printing and exporting on, no field focused | File's open and close choices, View > Go to Layout, Records > Go to Record, Window > Minimize, and Tools > Data Viewer stayed enabled. New Record, mode and view changes, Print, Export Records, Save a Copy As, and New Window were disabled. Enabled parent menus could have no enabled child. |
| Editing only versus Minimum, printing and exporting on | In matched Browse contexts, Editing only additionally enabled Copy and Copy All Records with no field focused; Paste, Select All, Insert > From Index and Export Field Contents with a text field focused; and Cut, Copy and Clear when its text was selected. Both levels disabled the tested Print, Export Records, New Record, mode/view changes and New Window choices. |
| FileMaker-file import from a closed source | No source sign-in prompt appeared. With an Admin target, an export-off source yielded 2 records. With an `editonly` target whose own exporting was off, the export-off source was refused before Specify Import Order, while an export-on source yielded 2 records. All files held the same account names and passwords. Reuse of the target session's credentials for the closed source is inferred; the source's actual session identity was not shown. |
| FileMaker-file import from a closed source with different credentials | The target Admin account had an empty password; the closed source Admin account had a different, nonempty password and no automatic login. File > Import Records > File opened the source's sign-in dialog before Specify Import Order. The account field defaulted to the Mac user name, not the target account. Signing in as source Admin reached import mapping; Cancel at sign-in returned to the target without an alert or data change. After a Pro restart, the source sign-in prompt appeared again. |

Evidence: [Minimum menu](https://github.com/OpenMaker-Dev/openmaker/issues/214),
[matched menu levels](https://github.com/OpenMaker-Dev/openmaker/issues/214),
[closed source with matching credentials](https://github.com/OpenMaker-Dev/openmaker/issues/214),
and [closed source with different credentials](https://github.com/OpenMaker-Dev/openmaker/issues/214).
Hosted behavior, shortcuts and custom menus remain unmeasured.

### 2.4 FileMaker's answers to the privilege Get functions

- **LayoutAccess** (42) [C: `FMFileUser::GetValue`,
  `FMLayout::GetLayoutDataAccess`]: the records-via-layout access of the
  context window's layout, from its entry or the default. It answers 0
  when the layout itself or records via it are no access, 1 when records
  via it are view only, and 2 otherwise, also under full access. The
  built-in [Data Entry Only] and [Read-Only Access] give records via every
  layout modifiable (`de`), so both answer 2, as FileMaker Server did for
  every probed set (calc-functions.md 9) [O]. It does not reflect record
  privileges; RecordAccess does.
- **RecordAccess** (43) [C: `DBFileUser::GetValue`]: the record access of
  the context window's current record cursor, 0 without one. That this
  applies the current table's own entry (2.1) rather than the default is
  inferred from the accessors [I].
- **CurrentExtendedPrivileges** (69) and **AccountExtendedPrivileges**
  (95) [C: `DBFileUser::GetValue`, `GetExtendedPrivSetString`]: the
  keywords of the extended privileges whose set list holds the set,
  separated by ¶, in the order of the extended privilege catalog's name
  index `[23, 3, 1, 01]` (a name iterator over `KNameList`), which is
  keyed by the keywords' collation keys. For the built-in lowercase ASCII
  keywords this is alphabetical, as FileMaker Server answered [O].
  FileMaker Pro's `FMFileUser::GetValue` passes both selectors to the same
  engine function (Mac x86_64 `DBEngine`
  `DBFileUser::GetExtendedPrivSetString` `0x1a1b30c`), so the desktop
  answers in the same order [C]; this was not run on the desktop.

`AccountPrivilegeSetName` (94) and `AccountExtendedPrivileges` (95) always
refer to the authenticated account. `CurrentPrivilegeSetName` (34) and
`CurrentExtendedPrivileges` (69) refer to the effective set. During a
Run with full access script step, the effective name is `[Full Access]`
and its extended list comes from that set's catalog memberships; the
account selectors retain their answers. See script-runtime.md 9.1 [B/I]
and the server oracle in calc-functions.md 9 [O].

### 2.5 What OpenMaker reads and answers

OpenMaker reads a set's name `10` and record access `02`, and the
extended privileges `[23, 3, 5, E]` (keyword at `10`, enabled set keys as
children of `05`), selecting by exact raw set key; no keyword is
fabricated for full access. It recognizes the three built-in sets from
their members (2.7, `om_data::host::security::builtin_set`) and applies
their limits; a local file opens only with an account in one of them
(local-files.md 2.3):

- [Read-Only Access] creates, edits and deletes no record (2.1: `10 03`):
  the desktop disables New Record, Duplicate Record, Delete Record and
  Import Records, as FileMaker Pro 26.0.3 does (#156's reading; Pro keeps
  Import under [Data Entry Only]), and its provider and the host refuse a
  record write with 200 ("Record access is denied"; the code is
  OpenMaker's choice, unmeasured) and Pro's text "Your access privileges
  do not allow you to perform this action." [O]. Pro lets the user enter
  a field in Browse; typing in it shows that alert, and OK leaves the
  value unchanged (#156's reading [O]).
- [Data Entry Only] and [Read-Only Access] change no design (2.2: layouts
  view only, scripts executable only; 2.3: no "Manage database" option):
  File > Manage's dialogs, Layout mode (its menu item, shortcut and Edit
  Layout button) and Script Workspace are disabled, and the host refuses
  their catalog uploads and file-changing query programs, as it does for
  every set but [Full Access].
- What else FileMaker Pro disables under these sets (menu commands and
  status toolbar buttons) is not measured; OpenMaker leaves the rest
  enabled.

OpenMaker decodes every set's members as 2.1-2.3 describe them
(`om_data::host::privilege_set`): record access by default and per
table, field access, the compiled formulas limiting an action, the
layout, script and value list entries and the other privileges. An
unknown member, a value that does not decode, or a formula for an action
that is not limited makes the set unreadable. It recognizes the built-in
sets through that decoding (2.7). A custom set is enforced when every
member it has is one OpenMaker enforces (`privilege_set::enforced`): each
table's create, edit and delete yes, no, or limited by a calculation of
the table's own entry, its records viewable, viewable only where a
calculation is true, or never (its own entry or the set's default for
the other tables), every field modifiable, view only or without access,
layouts, scripts and value lists with any access, the records through
every layout it may see modifiable or view only; printing and exporting
may be denied, and
menu commands may be Editing only or Minimum. Then [I: OpenMaker's
choices, not compared with FileMaker Pro, except where marked]:

- the host refuses a new record, a serial for one, a container count
  change, and a commit that creates, edits or deletes a record its set
  does not allow on that table (200); a commit's actions are decided
  under the write lock by the records the file holds, so a record that
  does not exist is created whatever the commit marks it; an action
  limited by a calculation is decided there by the formula
  (`rules::recalc::RecordLimit`) on the stored record for an edit or a
  deletion and on the uploaded record for a new one, and a formula that
  cannot be evaluated refuses (which record state FileMaker evaluates is
  unmeasured); a new record's id and serials are reserved for a limited
  create;
- a table whose records may never be viewed (its own entry, a view limit
  without its formula, or the set's default for tables without an entry)
  is treated as a view formula no record meets, so every record below
  applies to it; a field without access is left out of every record the
  host serves (`views::ViewFilter`) and below one, its table serves its
  indexes and container library empty, finds on it match nothing, the
  desktop leaves it out of Browse and refuses a write to it (201), and an
  edit's commit keeps its stored value whatever the upload holds
  (`views::keep_hidden`), as the client never had it; the desktop draws
  `<No Access>` in it (`DataProvider::no_access`), as FileMaker Pro 26.0.3
  does for a record it may not view (#137's reading; for a field without
  access, inferred);
- a record a table's view formula is not true on keeps its id and
  modification count, so it stays in found sets and lists, but every read
  of the file on the host (downloads op90, op15 and op92, query programs
  op86) has its field values withheld (`views::ViewFilter`, which always
  evaluates the formula on the stored record, never on the fields a
  request names): the desktop draws `<No Access>` in its fields, as
  FileMaker Pro 26.0.3 does (#137's reading [O]; their values are empty),
  and a find never matches it. A portal leaves such a record out, and one
  of a table the set may never view: FileMaker Pro 26.0.3 lists only the
  rows it may view, its creation row after them (#695's reading [O]), and
  so does the desktop. A related field without access shows `<No Access>`
  in every row, in the creation row and placed on the layout, and cannot
  be entered: a click gives no insertion point, and Shift-Tab passes it
  (#695's reading [O]). Not measured: a portal with more rows than fit,
  and a related field on the layout whose first related record is one
  the set may not view. Such a record
  cannot be edited, whatever the set's edit access: the client never had
  its values, and its upload would replace the stored record. The host
  refuses (200) an edit of a stored record the formula is not true on, or
  one the session was served withheld since it opened the file under its
  account, even if the formula has turned true since; the desktop refuses
  first on the record it just read (#670). A new record the formula is
  not true on (or one of a table the set may never view) is created all
  the same, as FileMaker Pro 26.0.3's New Record creates one under #137's
  `limited` set, and Delete Record removes it (#137's reading [O]). The
  host serves it back as its id and modification count, so the desktop
  checks only those after a create, when it judges the formula false both
  on the values it wrote (as on a page's records) and on an empty record,
  and the host serves no values. Where the formula is true on an empty
  record, or cannot be evaluated, an empty read-back could be a record the
  host kept empty: the values are checked, and the create is reported as
  not verified [I: OpenMaker's choice]. It then draws `<No Access>` in the
  new record; what FileMaker Pro draws there is unmeasured [I]. A view-limited
  table serves its indexes (which hold every record's values) and
  container library empty and builds no index on demand, so its finds and
  sorts read records; while any table has a view limit, the file's own
  container library and thumbnails are withheld too, and container uploads
  (op88) and reference count changes (op87) are refused, as their answers
  (an existing member found by deduplication, a count) come from libraries
  the account may not read;
- the desktop evaluates the same formula first (an edit on the stored
  values, a new record on the values the write sets, a deletion on the
  stored record) and refuses with 200; values a new record's rules enter
  are not seen there, only by the host;
- the desktop leaves a disallowed action out of Browse, refuses it with
  200, and refuses a write's own change to a view-only field with 201
  wherever it is computed
  (`prepare_record`: Browse, scripts, imports, Commit Transaction, and a
  related file's writes under that file's own account); values the
  record's rules enter are not checked;
- FileMaker Pro 26.0.3 lets the user enter a view-only field, and a field
  of a record the set may not view (`<No Access>`); typing shows "This
  action cannot be performed because this field is not modifiable." in
  the first and "Your access privileges do not allow you to perform this
  action." in the second, with the value unchanged (#137's reading [O]).
  A record the set may view but not edit, records through a layout that
  makes them view only, and a field without access are unmeasured;
- a layout the set gives no access is left out of the layout menu. A
  script's Go to Layout to it (a button's included) enters it as FileMaker
  Pro 26.0.3 does: a gray page showing `<No Access>`, its name blank in
  the layout menu, New, Delete, Sort and Quick Find disabled, no alert
  (#137's reading [O]), and the desktop reads nothing through it. In a
  local Pro 26.0.3 copy whose opening layout was hidden from the account,
  Pro instead opened a blank gray page with no `<No Access>` text, empty
  toolbar layout name and no alert [O: #696]. This held both when File
  Options named the hidden layout and when that option was off and the
  remembered first layout was hidden. On both the opening page and the
  later `<No Access>` page, Data Viewer in the restricted account's context
  returned the hidden layout's name from `Get ( LayoutName )`, `0` from
  `Get ( LayoutAccess )` and `0` from `Get ( LayoutNumber )` [O: #696].
  A script's New Window to it, and Go to Related Record to it without a
  new window, complete with 0 in FileMaker Pro 26.0.3 [O: #696, local]:
  New Window opens a second window on a blank gray page with an empty
  layout name; Go to Related Record moves the window to the gray
  `<No Access>` page with the related records found (1 of 2) and the
  layout picker then names the layout; `Get ( LayoutName )` is its name
  and `Get ( LayoutAccess )` 0 after each. The desktop does the same, and
  runs no layout trigger there [I]; Go to Related Record with a new
  window to such a layout is unmeasured (it opens on the blank page [I]).
  The desktop's first window of a file (one a script opens, likewise)
  opens on the file's "Switch to layout", else on its first layout, also
  when the account may not see it, and draws that blank gray page with an
  empty layout name; it runs no OnLayoutEnter there [I]. Pro, with "Switch
  to layout" off, opens on the layout the file was last left on, which in
  the measured copy was also the first; OpenMaker keeps no such layout
  [I]. Its provider refuses to show such a layout (9) and gives no value
  through it, only its found set's ids and count, which the window's
  toolbar shows; a find or sort through it is refused (3), as what it
  found and how it ordered them would tell the values [I]. A script on
  it still reads fields and exports records as the table's rules allow,
  as on a layout whose records have no access (#707) [I: unmeasured for
  a hidden layout]. Its fields and objects
  are dropped from the calculation schema, so `FieldNames`, `FieldBounds`,
  `LayoutObjectNames` and `GetLayoutObjectAttribute` read nothing in it.
  OpenMaker answers `Get ( LayoutNumber )` `0` on a hidden current
  layout too. Its `Get ( LayoutCount )`,
  `LayoutNames` and a layout number in a script include hidden layouts;
  FileMaker's answers for those and for the design functions are unmeasured;
- records through a layout (the layout entry's second field, 2.2) view
  only are enforced by the desktop alone [I: OpenMaker's choice]: a
  commit names a table, not the layout it came through, so the host
  cannot check them. They show but take no edit in Browse, and every
  write through the layout fails with 200 and FileMaker Pro's privilege
  alert text (a new, changed, imported or deleted record, a script's
  write, a container stored into a field, and a commit holding a deleted
  portal row's record, refused before anything is written). What
  FileMaker Pro shows and alerts there, and whether it lets a portal's
  related records on such a layout be edited, are unmeasured; OpenMaker
  refuses those edits too;
- records without access through a layout the set shows: FileMaker Pro
  26.0.3 admits the account and draws the layout, its labels, tab panel
  and frames, with every record-bound edit box, pop-up, container and
  merge text showing `<No Access>`, the toolbar still counting the found
  set; `Get ( LayoutAccess )` is 0 and `Get ( RecordAccess )` 2 there,
  and an ordinary (not full-access) script reads the field and writes it
  with Set Field, saved (#707's readings [O]). The restriction is
  presentation only. The desktop does the same: the layout's objects are
  masked in the window while a script reads and writes as the table's
  rules allow (a script's write through a view-only layout stays refused,
  above). Not measured, and conservative in OpenMaker [I]: conditional
  formatting, hide conditions, calculated labels and tooltips are not
  evaluated there and no tooltip shows (Pro showed none for that account,
  for a reason not isolated); portals on such a layout show
  `<No Access>` in their cells; web viewers and charts show no data (as
  everywhere in OpenMaker today); New, Duplicate and Delete Record
  through it are refused. Through a hidden layout the records are never
  read;
- a script the set gives no access is left out of the Scripts menu, and
  calling it fails with 9 and its steps do not run unless a caller runs
  with full access, as FileMaker's `CallScript` and step dispatch do
  [C: script-runtime.md 2.2, 3.7]; that FileMaker's Scripts menu leaves it out
  is inferred;
- a value list the set gives no access has its items read nowhere:
  `ValueListItems` answers empty, a layout control offers no items and
  refuses a choice (9), a sort by the list ignores its order, ExecuteSQL's
  value-list table has no rows, and the provider refuses the list (9, so
  a Sort Records step by it fails) and leaves it out of the value-list
  catalog (Manage Value Lists, agents). `ValueListNames`, `ValueListIDs`
  and the field's validation by the list are unchanged. What FileMaker
  shows and answers for such a list is unmeasured;
- printing (`0x0100`) covers printing and saving records as PDF, and
  exporting (`0x0200`) covers exporting records, saving them as Excel and
  copying all records of the found set (FileMaker Pro Help, "Editing other
  privileges"). A set that denies printing disables File > Print, Page
  Setup and Save as PDF (menus, shortcut, toolbar); one that denies
  exporting disables Export Records and Save as Excel. Their steps do what
  FileMaker Pro 26.0.3 does (the table above): Print, Save Records as PDF,
  Export Records and Save Records as Excel fail with 3 and no alert; Copy
  All Records/Requests shows the privilege alert, OK only, leaves the
  clipboard and fails with 200; Print Setup and Export Field Contents run.
  A step of a script set to run with full access in that file is allowed
  them. With errors captured, Copy All Records shows no alert [I]. Save a
  Copy as and Save Records as Snapshot Link have no implementation (3),
  and Import Records reads no FileMaker file, so OpenMaker reproduces
  neither their outcomes nor the 714;
- under Editing only or Minimum menus (FileMaker Pro Help, "Editing other
  privileges": Minimum keeps opening, closing and creating files,
  switching windows, performing scripts, Help and Settings; Editing only
  adds basic editing in Browse mode; both disable New Window), the menus
  follow FileMaker Pro 26.0.3's readings of #137's `editonly` and of
  #651's Minimum copy of it, in Browse mode [O]:
  - both levels keep View > Go to Layout, Records > Go to Record, View >
    Status Toolbar, Formatting Bar and zoom, and Window > Minimize Window.
    File > Manage, Import Records and Save/Send Records As open with every
    command in them disabled;
  - both disable the modes and views, Page Setup, Print, Export Records,
    Insert's Picture and File, Records' New, Duplicate and Delete Record,
    Omit Record, Omit Multiple, Modify Last Find, Sort and Unsort, and New
    Window;
  - Editing only also keeps Show All Records, Show Omitted Only, Refresh
    Window and, for a selected container, Export Field Contents, and
    disables the toolbar's New and Delete Record, Find, Sort, Quick Find
    and Share and the layout bar's View As and Preview. Minimum leaves
    out Show Omitted Only, Refresh Window, Export Field Contents and
    Change Password.

  OpenMaker's own choices [I]:
  - Minimum leaves out Show All Records too, as Pro's reading had every
    record shown;
  - Revert Record needs all menus, as Pro's readings had no record
    changed;
  - Change Password needs all menus, as Pro's comparison of the two levels
    named no difference there;
  - Delete All Records, the Requests menu and Insert's other commands stay
    disabled under both;
  - Minimum disables the status toolbar and the layout bar, of which Pro's
    reading names only New and Delete Record, Find and Sort as disabled;
  - the previous and next record shortcuts work under both, as Go to
    Record does, and an action that is only a menu command a level leaves
    out (modes, record commands, menu-run steps, Quick Find) is ignored
    when its shortcut is pressed.

  Scripts, buttons, fields and dialogs are unaffected, and so are going to
  a record and committing it by clicking on the layout. OpenMaker's field
  editors do their own cutting, copying and pasting under both; the Edit
  menu's Cut, Copy and Paste, which Pro enables under Editing only, are
  not implemented. Shortcuts under either level are unmeasured (#651);
- it changes no design, and its other options (managing the database,
  creating layouts and so on) are never granted, whatever they say.

View-only layouts, scripts and value lists need nothing more, as a
custom set is granted no design change. The host does not check view-only
fields: it writes whole records, which also hold what the client's rules
entered. A set with its default for all tables limited by a calculation
is refused rather than given more than it allows.
Any other custom set is refused locally with 9. The `Get` answers below
still read only `02`:

- On the desktop, under a set it enforces, both are answered for the
  context [I: OpenMaker's reading of 2.4, unmeasured]. `LayoutAccess` is 0
  on a layout the set gives no access or whose records have no access, 1
  where its records are view only, and 2 otherwise; `RecordAccess` is 0
  for a record the account may not view, 1 for one it may view but not
  edit (its edit access, an edit formula not true on it, or the layout's
  records view only), and 2 otherwise, and 0 without a current record. In
  GUI calculations the record's page answers; in a script the host asks
  the provider when `Get ( RecordAccess )` is evaluated, for a custom set
  only, and a built-in set keeps the answer below. In Find mode, whose
  cursor is a request, both readers keep the answer below [I].
- Otherwise `RecordAccess` comes from the set's default `02` alone: 2 when
  the first byte has any `0x45` bit, otherwise 1 when `0x10` is set,
  otherwise 0. This checks the low bit of the create, edit and delete
  pairs; it does not recognize a limited action from its other bit. For
  example, `18 03` (view yes, edit limited) answers 1 in both readers.
  Missing or empty flags mean unknown on the desktop, but OpenMaker Server
  substitutes 0 [I: OpenMaker's approximation].
- `LayoutAccess` without a set the desktop enforces: the desktop answers
  empty and OpenMaker Server answers 2 for every set [I].
- The desktop keeps the extended privileges' catalog (member id) order,
  and OpenMaker Server sorts the keywords case-insensitively; FileMaker
  orders them by the name index (2.4) [I: both are OpenMaker's choices].

The runtime establishes full access for an elevated step (layout and
record answers 2) and restores the account at the step boundary,
including calls to unelevated scripts. Unknown effective extended
metadata answers empty; it never borrows the account's keywords. These
empty answers for unread metadata, unread layout and record access
included, are not FileMaker's: a layout object's hide condition that
reads one is unresolved rather than hidden or shown
([conditional-visibility.md](../conditional-visibility.md)). Scripts keep
reading the empty answer.

### 2.6 Unknowns

- The hosted form of the hidden-layout Go to Related Record and New Window
  checks, other New Window styles, a Go to Related Record step with no match,
  and a script set to run with full access were not measured in #696's
  local probe.
- Whether FileMaker Pro requires the `fmapp` extended privilege to open a
  local file: no starter solution grants it to [Data Entry Only] or
  [Read-Only Access] (`[23, 3, 5, 03, 05]` is absent), and their only
  account in those sets is the inactive [Guest]. OpenMaker does not check
  it locally.
- Which menu commands and toolbar buttons FileMaker Pro disables under
  [Data Entry Only] and [Read-Only Access], and what a [Read-Only Access]
  user sees on typing into a field, New Record or Delete Record.

- No set was changed in FileMaker Pro and dumped, so the option labels of
  2.3 not marked [C] are inferred, and field entries `05, T, 05, F` and
  value list item entries are from the code only (no observed file has
  one).
- What the third and fourth fields of a layout, script or value list
  entry mean, and bits 7-2 of a record access value's second byte.
- Whether FileMaker Pro's Get ( LayoutAccess ) and Get ( RecordAccess )
  match FileMaker Server's under custom sets: not run.
- What FileMaker Server does with a signed-in client whose account is
  deleted (section 1), and whether a Perform Script on Server session
  takes its caller's privileges as at the caller's Login or as the
  account's set is now.

### 2.7 The built-in sets as stored

[O] Every English starter solution of FileMaker Pro 26.0.3 (Assets,
Contacts, Content Management, Inventory, Meetings, Tasks) stores the three
sets with these members, identical in all six (checksums `04`, UUIDs and
stamps aside), and no other member:

| Member | [Full Access] `01` | [Data Entry Only] `02` | [Read-Only Access] `03` |
|---|---|---|---|
| `02` record access (2.1) | `55 03` | `55 03` | `10 03` |
| `09, 04` layouts (2.2) | `df` | `de` | `de` |
| `09, 11` scripts | `d7` | `d6` | `d6` |
| `09, 21` value lists | `d7` | `d6` | `d6` |
| `0a` other privileges (2.3) | `9f ff` | `03 04` | `03 04` |
| `05` table entries | none | none | none |

Two blank files made with FileMaker Pro 26.0.3's German UI (Create New >
Blank) stored these names at `10` under set keys `01`, `02` and `03`:
`[Voller Zugriff]`, `[Nur Dateneingabe]`, `[Nur Lesezugriff]` [O: local
#876 probe]. Their checked `02`, `09, 04`, `09, 11`, `09, 21` and `0a`
values matched the English Pro-made blank and the table above. The
set keys stayed the same while the names changed. All 66 shipped starter
files in eleven languages stored the English names [O: #873]; a starter's
language does not establish what a newly created file under that UI
stores. Other UI languages, renamed or changed built-in sets and files
converted from older versions have not been measured.

Their accounts are Admin (`[23, 1, 5, 02]`, reference `01 01`, active) and
the built-in [Guest] (`01`, reference `01 03`, inactive). The extended
privilege `fmapp` (`[23, 3, 5, 03]`) lists no set; only
`fmreauthenticate10` (`04`) lists all three. These members match what
2.1-2.3 decode for the sets as Claris documents them: [Data Entry Only]
creates, edits and deletes records but changes no design; [Read-Only
Access] views records only (Claris FileMaker Pro Help on the predefined
privilege sets) [I: the option names of 2.3 not marked there].

### 2.8 Custom sets as FileMaker Pro writes them

[O] A Contacts starter to which FileMaker Pro 26.0.3 for macOS added five
custom sets, each duplicated from [Data Entry Only] with one change made
in Edit Privilege Set (#137's fixture; the dialogs were read back after
save, close and reopen), stores them as 2.1-2.3 decode them:

| Set | Change in the dialog | Stored |
|---|---|---|
| `viewonly` | Contacts: field Last Name view only | `05, 80 02, 02` = `55 03`; `05, 80 02, 05, 02` = `02` (field 2 view only) |
| `limited` | Contacts: View and Edit limited by `First Name = "Allowed"` | `05, 80 02, 02` = `69 03` (create yes, view limited, edit limited, delete yes); the formula at `07` (view) and `08` (edit), each with members `05` and `06` (formatted and compiled streams; no `04`) |
| `nolayout` | layout Contact List no access, its records modifiable | `09, 04 02` = `dd` |
| `noscript` | script Sort by Name executable only (as its default) | `09, 11 03` = `d6` |
| `editonly` | available menu commands Editing only | `0a` = `43 04`: `0x2304` after the XOR, `0x2000` without `0x4000` |

Every other member is its base's: record access `55 03`, `09, 04` `de`,
`09, 11` and `09, 21` `d6`, other privileges `03 04`. These confirm the
member layout and bit order of 2.1-2.3, until now read from the engine's
code only. FileMaker Pro's behavior under these sets was not measured.

## 3. Desktop transport and sensitive-data lifetime

op90 can return a terminal key's value inline (network-protocol.md
5.6.6c). OpenMaker's desktop requests `[23,1,5,A,0b hex]` directly,
avoiding the account parent whose siblings can include authentication
hashes (`04`, `06`, per the s33 catalog observations). FileMaker Pro
instead requests the whole signed-in account member after Login (section
1). OpenMaker Server supplies its known metadata but omits `06`, its
derived checksum `04` and unknown children when op90, op15, op92 Download
List or a node query reads that whole member [I: OpenMaker's policy].
Other whole account members and their parent subtrees remain refused to
restricted sessions. Direct reads of any member's child other than `04`
and `06` remain available, as before this change [I: OpenMaker behavior].
Only the referenced set, the built-in full access set for elevation, and
extended-privilege members are read by OpenMaker's desktop.
Missing/refused/malformed optional metadata must not prevent file open.
There is no account-list download, raw security cache or diagnostic log.
Manage Security and `get_security` do not download `[23]` either. They
ask OpenMaker Server's Describe Security (authoring operation 5,
`docs/authoring.md`), which decodes the accounts, sets and extended
privileges on the host. That authoring reply never reads `04` or `06`,
so they never cross the wire in it [I: OpenMaker's policy]. FileMaker Pro's
own Manage Security transport was not observed.

Provider buffers holding returned inline values or slots use zeroing
scope guards, including unexpected node replies and decode/error paths;
borrowed HBAM trees die before their guarded backing buffers. Only decoded
set names, selected extended keywords and optional aggregate access values
survive in session metadata. Account references and other accounts' values
do not cross the worker boundary. The existing open reply carries metadata
before the first desktop frame; other windows share the file's metadata.
This is a data-minimization policy [I], not a claim about clearing the
transport/TLS implementation's temporary allocations.

## 4. Checksums of privilege sets, extended privileges and file access

Privilege sets `[23, 2, 5, P]`, extended privileges `[23, 3, 5, E]` and
file access entries `[23, 4, 5, F]` keep a 16-byte checksum at `04`
(`KCount`), and the file access catalog keeps one for itself at
`[23, 4, 1, 09]` (`KSummary`). Each is the MD5 (Support's `CRCContext`)
of the bytes listed below. Accounts have their own (issue #148).

Evidence [C]: Linux `libDBEngine.so` (DWARF; nm addresses, with the
disassembly for argument sizes): `DBPrivileges::ComputeCRC` `0x109d120`,
`DBAccess::UpdateCRC` `0x109c490`, `DBPrivilegeExtensions::ComputeCRC`
`0x10967e0`, `DBAuthFile::ComputeCRC` `0xd4c910`,
`DBAuthFileCatalog::ComputeCRC` `0xd4dfb0` and `GetCatalogCRC`
`0xd4e180`, the members' `DoCommitProcessing` and the catalogs'
`HasBeenTampered`. Evidence [O]: a reimplementation reproduces every
stored checksum of the 66 FileMaker Pro 26.0.3 starter files (198 sets,
660 extended privileges, 132 file access entries, 66 catalogs) and of 673
FileMaker-written example files of the MBS plug-in (2,023 sets, 5,393
extended privileges, 270 file access entries, 673 catalogs), custom sets
with per-table rules, a limiting formula, layout and script entries
included; and of every file of the lab's script oracle harness.
OpenMaker's own implementation (`om_data::security_checksum`, written from
these recipes and local-files.md 2.4) recomputes all 1,188 checksums of the
66 starter files, accounts included (`crates/om-data/tests/metadata_corpus.rs`,
`every_security_checksum_recomputes`); it refuses a set with an action
limited by a calculation, whose formula part it does not compute.

The recipes below cover well-formed members, with the explicit absence
defaults stated below: UUIDs have 16 bytes, type-0 hashes have at least
32 bytes, type-1 tokens have at least 64 bytes, and application entries
under `09` have a value byte. The author's reproduction also supplies
fallbacks for malformed or incomplete members; those fallbacks are not
established as native behavior by the disclosed vectors and are outside
this recipe's scope.

**Path keys.** Where a key is hashed it is written as a one-segment path
key: the segment's length plus 1, its length, its bytes (`02 01 04` for
`04`, `03 02 80 01` for `80 01`). `mInternalCreation` is the file's
`[2, 254]`, 4 bytes as stored (a big-endian u32).

### 4.1 Privilege set `04`

In order (`DBPrivileges::ComputeCRC`):

1. The loaded record access `02` (section 2) as five bytes: the create,
   view, edit and delete levels (0 no, 1 yes, 2 limited) and the field
   default (byte 1, bits 1-0); a missing `02` gives `00 00 00 00 01`.
   Before hashing these bytes, normalize each limited action whose
   formula has no compiled stream (`06` of the formula node) to level 0.
   It contributes no formula or context bytes [C: `DBAccess::Load`;
   reproduced by the author's checksum reader]. Then, for each action
   still limited, in that order, hash its compiled stream and its context
   occurrence key as stored at `04`, after one byte holding the key's
   length. The per-table access in step 5 uses the same normalization.
2. A big-endian u32: `0a` XOR `0x6000`, plus `mInternalCreation` as a
   number, modulo 2^32 (a missing `0a` counts as 0).
3. The description `03` as UTF-16 code units, big-endian (nothing when
   absent).
4. `0c` and `0d`, two bytes each as stored (`00 00` when absent).
5. For each table node `05, T` in key order: `T` as a path key, its record
   access as in step 1, then for each field entry `05, T, 05, F` in key
   order, `F` as a path key and the entry's byte.
6. For each entry under `09` in key order (defaults and item entries
   together: `04`, `04 13`, `11`, `11 80 59`, `21`): its key as a path
   key and its byte.

The set's key and name are not hashed. Vector, the English Tasks starter
file's [Full Access] (`mInternalCreation` `dadd00d6`): MD5 of `0101010103`
`dade00d5` `0000` `0000` `020104` `df` `020111` `d7` `020121` `d7` is
`5f6796ff3ed6536204fc13bd7324301c`, its stored `04`.

The catalog header keeps an index of these checksums: `[23, 2, 1, 08,
<checksum>, P]` is an empty leaf for every set, and no other [O: every
observed file]. The code that maintains it was not located; a writer that
changes a set's `04` updates this entry with it [I].

### 4.2 Extended privilege `04`

In order (`DBPrivilegeExtensions::ComputeCRC`): the keyword `10` as
UTF-16BE code units, the description `03` the same way (nothing when
absent), one byte from `02` (a one-byte key, `01 01` reads 1; absent or
another form reads 2; `GetSharingState`), then each set key under `05` in
key order as a path key. Nothing file-specific is hashed: the same
extended privilege has the same checksum in every file [O: `fmwebdirect`
is `94d51eae387ea8ed4c050d4010d98d55` in all starter files]. Vector,
Tasks's `fmreauthenticate10`: the keyword's code units, `01`, `020101`
`020102` `020103` give `92f8884732ae4993a3c8d24a1bead8e6`.

### 4.3 File access entry `04` and the catalog's `09`

An entry (`DBAuthFile::ComputeCRC`): `mInternalCreation`; the entry's key
as a path key; its type `02` as a big-endian i16 (`AuthType`: 0 inbound,
1 outbound; absent 0); the 16-byte UUID `0a`; the file names `10` as
UTF-16BE; the authorization date `0b` after one byte holding its length
(absent: the key of 1/1/1024 at midnight, `06 05 90 00 41 80 00`
[C, never observed]); the account name `0c` as UTF-16BE; then for type
0 the first 32 bytes of `0d` (`GetAuthHash`, a SHA-256), for type 1 its
first 64 bytes (`GetAuthToken`), for other types nothing.

The catalog (`DBAuthFileCatalog::ComputeCRC`): `mInternalCreation`, the
catalog's 16-byte UUID `[23, 4, 1, 10]`, then `00 00 00 01` when either
of the low two bits of `[23, 4, 1, f0]` (a big-endian u32,
`GetRequiresAuthorization`: 0 none, 1 file, 3 file and host) is set,
else `00 00 00 00`. Vector, Tasks: `dadd00d6`
`1a40e6a143b54e24be9884c5323d1a41` `00000001` gives
`54f80051fc0a43eb8c48c2db92100c51`, its `[23, 4, 1, 09]`.

### 4.4 When FileMaker checks and writes them

Committing a set, an extended privilege or a file access entry recomputes
its `04` (`DoCommitProcessing`); committing the file access catalog
recomputes `09` (`UpdateCRC`) [C]. When FileMaker Pro opens a file
(`DBSession::InternalCreateFileUser`, Mac x86_64 `DBEngine` `0x1bae6a6`) it
runs each catalog's tamper check and refuses the file with **808**
("Cannot open file because access privileges are damaged") when one fails
[C]:

- privilege sets and extended privileges (`DBPrivilegesCatalog::
  HasBeenTampered` `0x10a31a0`, `DBPrivilegeExtensionsCatalog::
  HasBeenTampered` `0x10992c0`): every member's computed checksum must
  equal its stored `04`; a missing or shorter `04` fails, and so does a
  catalog without members;
- file access (`DBAuthFileCatalog::HasBeenTampered` `0xd4e1f0`): without a
  `[23, 4]` catalog, the check fails when the file version `[2, 5]` cannot
  be read or is above 10; with one, `[23, 4, 1, 09]` must be 16 bytes equal
  to the catalog's checksum and every entry's `04` must match.

So a writer that changes any hashed member must store the new checksum.
Because a set's checksum hashes `mInternalCreation`, copying a set to
another file changes it; an extended privilege's does not [C].

Unknowns: what the sharing state `02` of an extended privilege means;
when exactly the open runs these checks (FileMaker Pro was not run with a
damaged file); and who maintains the set checksum index `[23, 2, 1, 08]`.
