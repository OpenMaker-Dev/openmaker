# Field auto-entry and validation metadata

Status: metadata recognition is implemented in `om-hbam::FieldOptions`;
`om_data::rules` (used by `om-provider` and by OpenMaker Server's scripts)
evaluates auto-entry, stored calculations and validation for native
creation and text updates with `om-calc`. It supports
auto-enter calculations and stored calculations of any result type but
container (recomputed when a field they read changes, honoring "do not
evaluate if all referenced fields are empty", option `0x20`), clock, account,
user and UUID `Get` selectors, constants (`0d`), account and date/time audit
variables, required values, maximum length, validation by calculation with
the field's message, and uniqueness of text fields. Contacts' actual stored
Initial formula is decoded from its compiled stream without field-name
special cases. Lookups are performed only in the exact-match form below
(Lookups); serial numbers as below (Serial numbers), reserved on the host.
A container field a formula reads is read as its data, through the
write's container reader. An auto-entered or validation calculation may
read one repetition of a non-container global of its own table. Other
global reads, entry from the last visited record, summary fields, plug-in
functions, custom functions that read fields and dependency cycles fail
preflight. The supported lookups, related reads and unstored calculations
are listed in docs/record-writes.md, Limits.

The provider preserves creation metadata on updates, refreshes modification
variables, checks unique values with the server under a lock on the field's
index (write-path.md 1.3), uploads the complete record, commits it and verifies
generated values and the modification count by reading the record back.
Local dates/timestamps use the observed US storage form; user-name variables
use the desktop's user name (Settings' User Name, calc-functions.md 6.1.3),
separately from the authenticated account. Full locale, expression and validation coverage remains
incomplete. The static observations below supplied the implementation boundary.

## Evidence boundary

Read-only corroboration used the user's local `test project.fmp12` capture
copy. Static evidence comes from the installed FileMaker Pro 26.0.3.307 arm64
frameworks, before ASLR:

| Thin image | SHA-256 |
| --- | --- |
| DBEngine | `43ddff25a568fe3843b1aaadf4f295ca1c1a20c87ce91add75b990f0175bdb61` |
| FMEngine | `a134427e0d7294403c1e0a903a12ab02b3ea665f28ee11bfa6f137efd7e80921` |

Focused LLVM disassembly and exported symbols supplied these observations.
The shared REA large-image Ghidra startup timed out, so this investigation has
no REA Evidence IDs. None of the observations below are a successful native
write or a demonstration of the server's behavior after a raw op70 commit.

## Auto-entered calculations reading their own field

Claris's [Self reference](https://help.claris.com/en/pro-help/content/self.html)
says that `Self` returns the associated object's content and is allowed in
field auto-enter calculations. Its [automatic entry help](https://help.claris.com/en/pro-help/content/automatic-data-entry.html)
describes self-modifying auto-entry with replacement enabled.

**Inference (#1156):** a named reference to the auto-entered field itself
reads its current value, just as `Self` does, rather than forming a stored
calculation cycle. Keep this reference for change triggers and the
"all referenced fields are empty" guard; omit only its ordering edge.
An explicit change to that field can therefore evaluate its replacement
formula once, using the entered value. A stored calculation referencing
itself, or a dependency cycle involving different fields, is outside this
inference and remains refused. Native timing and the named-reference
substitution have not been measured for this slice.

FileMaker Pro 26.1.1 [measured the `Self` token](https://github.com/OpenMaker-Dev/openmaker/issues/1059)
in a local Number field. A formula with an empty branch returning 0 stores
0 at New Record and after an explicit clear. With replacement enabled,
`Self + 1` in the nonempty branch stores 26 after Set Field of 25. With
replacement disabled the same assignment stores 25. An unrelated edit
keeps the value. The result is visible immediately and survives commit.
Inferred: `Self` reads the proposed field value. Hosted files and other
field types were not measured.

## Fixed field definition

The definition is at `<base-table>/3/5/<field>/2`. The decoder accepts exactly
26 bytes and preserves all of them. `DBField::FieldDefinition::Read`
(`0x17e7eb4`) reads the structure and byte-swaps the five words at offsets
4, 8, 12, 16 and 20 and the final halfword at 24. `Write` (`0x17e7f54`) reverses
that transformation. Bytes 0..3 are not swapped.

| Offset | Size | Meaning |
| --- | ---: | --- |
| 0 | 1 | Field kind: normal 1, calculation 2, summary 3 |
| 1 | 1 | Data type, using the existing `DataType` identifiers |
| 2 | 1 | Opaque |
| 3 | 1 | Selected `EntryVariable`; it is active only with option bit 0 |
| 4 | 4 | Big-endian index-language identifier; retained raw |
| 8 | 4 | Big-endian option word |
| 12 | 4 | Big-endian validation word |
| 16, 20 | 4 each | Opaque |
| 24 | 2 | Big-endian maximum repetitions |

`SetOptions` (`0x17ead9c`), `SetValidationOptions` (`0x17eb1a4`),
`SetEntryVariable` (`0x17eb1d4`), and `GetIndexLanguage` (`0x17eacac`)
independently confirm their offsets.

For normal fields, the supported option masks are:

| Mask | Meaning | Static evidence |
| --- | --- | --- |
| `0x1` | Auto-enter selected variable | `AutoEnterData`, `0x17552c4` |
| `0x2` | Auto-enter serial | Same; `DoAutoEnterSerial`, `0x175598c` |
| `0x4` | Auto-enter constant | Same; FMEngine XML attribute `constant` |
| `0x8` | Auto-enter calculation | Same; XML attribute `calculation` at `0x2eb04` |
| `0x80` | Calculation replaces an existing value | Existing-value guard at `0x17555d0`; XML `overwriteExistingValue` at `0x2ec18` |
| `0x100` | Prohibit editing auto-entered value | XML `allowEditing` is the inverse at `0x2ecec` |
| `0x10` | Auto-enter the value from the last visited record | FileMaker Pro 26's own save (schema-path.md 5.9, E30) |
| `0x200` | Auto-enter serial generated on commit, set instead of `0x2` | FileMaker Pro 26's own save (schema-path.md 5.9, E41) |

The validation word masks `0x4`, `0x8`, `0x10` correspond to XML children
`StrictValidation`, `NotEmpty`, `Unique`, respectively, in
`XMLField::GetValidationNode` (`0x2f2dc`, `0x2f0fc`, `0x2f150`). Other bits
remain raw. In particular, timestamp validation `0x2000` is not interpreted.
These flags describe requirements; recognizing them does not perform them.

## Index bits

schema-path.md 5.5 names the option word's index bits: `0x80000000` value
index, `0x20000000` word index, `0x10000000` and `0x08000000` their relaxed
forms, `0x40000000` prevent index. The files agree [O]: `0x80000000` is set
exactly when the field has a value index (`<table>/1/<field>` holds its counts
leaf `0x10`), `0x20000000` exactly when it has a word index (leaf `0x11`);
query-program.md 7.1 describes both indexes. Checked on all 4,750 fields of
test project and the 132 `.fmp12` files shipped with FileMaker Pro and
FileMaker Server (templates and samples), with no exception. No shipped file
sets any other bit of the top byte. FileMaker Server sets the bits when it
builds an index on demand (query-program.md 7.1); they do not change entry or
calculation behavior. `0x40000000` ("Automatically create indexes as needed"
off, schema-path.md E38) is set in a solution FileMaker Server hosts, whose
data entry is in production use, on four fields of one table: two
auto-entered calculations (`0xc0000008`) and the modification date and time
(`0x40000001`) [O, #994]. That it changes nothing but indexing is [I] from
its name and E38; OpenMaker's record planner reads it as an index bit, and
reads whether a field has a value index from `0x80000000` alone. Bytes 4..7 hold the index language (query-program.md
7.2): 21 (English) in test project and the English templates; each localized
template uses its own (20, 24, 25, 30, 31, 34, 39, 41, 45 or 76).

For a nonglobal calculation field, option `0x20000` disables stored calculation
results: `XMLField::GetStorageNode` tests kind 2, excludes global fields, and
exports `storeCalculationResults` as the inverse of this bit at `0x2f898`.
Contacts::Initial has kind 2 and option word 0, so its result is stored.

`0x80` (`kOverwriteExisting`, the auto-entry "replace existing value" bit)
is set on an unstored Date calculation (`0x00060080`) in a solution FileMaker
Server hosts, in production use [O, PM relay, sanitized]. No other
calculation field carries it: none of the 110 in the 66 `.fmp12` files
FileMaker Pro 26 for macOS ships, nor the 402 in the maintainer's other test
files [O]. It has no
auto-entry for it to qualify; that it is left over from an earlier
definition of the field, and changes nothing, is [I]. What FileMaker Pro's
dialogs show for it is unmeasured.

## Built-in variables and Get() expressions

| Byte 3 | Variable | Byte 3 | Variable |
| ---: | --- | ---: | --- |
| 0 | Creation date | 5 | Modification date |
| 1 | Creation time | 6 | Modification time |
| 2 | Creation timestamp | 7 | Modification timestamp |
| 3 | Creation user name | 8 | Modification user name |
| 4 | Creation account name | 9 | Modification account name |

`DBDependencyList::DoAutoEnterVariable` (`0x1755b18`) selects these pairs and
requests `kDBCurrentDateOp`, `kDBCurrentTimeOp`, `kDBLocalTimeStampOp`,
`kDBUserNameOp`, or `kDBAccountNameOp` from the expression environment. Account
name and user name are distinct variables. The table describes the selected
variable; it does not establish when creation and modification defaults are
scheduled, including initialization of modification fields on creation.

FileMaker Pro 26.1.1 allows Creation Time on a Text field and retains the
option when a Time field is converted to Text. Both Text fields use the
creation selector `01` and option word `0x00000001`. With a local English
12-hour file and system locale, New Record stored the same time-of-day
string as a Time field's stored text, including seconds and the AM/PM
suffix. An unrelated edit, a later commit, reopen and the Time field's
24-hour layout display kept that Text value. A 24-hour file or system
locale remains unmeasured.
[Native results](https://github.com/OpenMaker-Dev/openmaker/issues/1035#issuecomment-7185794163).

On FileMaker Pro 26.1.1, a local Text field with the calculated
auto-enter `$$Marker` read the current file's variable at New Record.
The two Text assignments produced two corresponding stored values. An
unset variable produced empty and left out the stored cell. Set Field on
an unrelated field, Commit and reopening kept those results. The check
did not change the variable between New Record and Commit.
[Native results](https://github.com/OpenMaker-Dev/openmaker/issues/1037#issuecomment-7185794811).

In this file, a field calculation subtree is `<base-table>/3/5/<field>/5`.
Child 4 contains its context, child 5 the formatted stream and child 6 the
unformatted stream. `DBGetFunction::WriteAsPolish` (`0x16c6930`) writes
opcode `9b` followed by a one-byte selector. `DBCalcSolver::SolveGetFunction`
(`0x17032c8`) reads that byte, adds 1000 and requests the corresponding value
from the expression environment. `DBCalcScan::LoadGetFunctionRow`
(`0x1a54934`) maps the same selector to DBResource131; the names are in
[`catalogs/get-selectors.csv`](catalogs/get-selectors.csv).

| Unformatted bytes | Expression |
| --- | --- |
| `9b 02` | `Get(CurrentDate)` |
| `9b 1d` | `Get(CurrentTime)` |
| `9b 1f` | `Get(UserName)` |
| `9b 20` | `Get(AccountName)` |
| `9b 3e` | `Get(ScriptParameter)` |
| `9b 3f` | `Get(CurrentTimestamp)` |
| `9b 65` | `Get(UUID)` |

`SimpleGetSelector` recognizes only an entire two-byte expression. A longer
calculation beginning with those bytes is not classified as a simple Get().
Unknown selectors stay explicit. The parser does not generate UUIDs, convert
timestamps, execute formulas, or interpret the formatted source stream.

## Actual Contacts requirements

Contacts is base table `0x8002`. All fields below have one repetition.

| ID | Field | Options | Validation | Generated value |
| ---: | --- | --- | --- | --- |
| 8 | Initial | `0` | `0` | Stored calculation of names; not auto-entry |
| 9 | PrimaryKey | `0x80000188` | `0x1c` | Auto-entry calculation `Get(UUID)` |
| 10 | CreatedBy | `0x101` | `0xc` | Variable 4, creation account name |
| 11 | ModifiedBy | `0x101` | `0xc` | Variable 9, modification account name |
| 12 | CreationTimestamp | `0x101` | `0x200c` | Variable 2, creation timestamp |
| 13 | ModificationTimestamp | `0x101` | `0x200c` | Variable 7, modification timestamp |

All five fields 9..13 prohibit manual editing and require nonempty values with
strict validation. PrimaryKey also requires uniqueness. Its complete
unformatted calculation is exactly `9b 65`; context child 4 is `03 d0 00 02`.
The formatted stream is 20 bytes and must not be confused with that two-byte
code. The high option bit in PrimaryKey is preserved, not treated as an
auto-entry flag. Fields 1..7 have none of the four supported auto-entry modes.

## Filenames from a repeating Container

FileMaker Pro 26.1.1 on a disposable local file saved a 24-repetition Text
field with option word `0x88` and `GetContainerAttribute ( Attachment ;
"Filename" )`, where Attachment was a 24-repetition Container of the
same occurrence. "Do not evaluate if all referenced fields are empty"
was on. Creating and committing a record with Attachment empty left no
stored Text cells. Inserting or replacing a container affected only the
matching Text repetition. Clearing the second container removed the
second Text cell and kept the first. A separate Text edit left both
repetitions as they were. Each write returned 0 with no alert, and
closed copies confirmed the stored cells.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/39)

Entering a value directly in the result field, preparing a new record
with a nonempty source, other formulas and hosted writes are unmeasured.

## Lookups

Option `0x400` auto-enters a looked-up value. Its definition is the
field member's `08` (schema-path.md 5.5): the occurrence the lookup
starts from, the occurrence it looks up from and the field it copies,
and "don't copy contents if empty".

**Calculated match fields (#1194).** Claris's primary
[relationship help](https://help.claris.com/en/pro-help/content/relationships.html)
allows calculation results as match fields and requires the related-side
match field to be indexable. Its
[Relookup Field Contents help](https://help.claris.com/en/pro-help/content/relookup-field-contents.html)
says that, when the matching value is calculated, a relookup targets an
editable input of that calculation. This documents calculated matching,
not the timing of automatic lookups during creation or an input edit.

**Relookup from the Records menu (documented, unmeasured).** Claris's
[lookups help](https://help.claris.com/en/pro-help/content/lookups.html):
"In Browse mode in the current table, select the match field, then choose
Records menu > Relookup Field Contents"; it "updates all fields looked up
by the match field across all records in the current found set", and "You
can't undo your actions when you update lookup values". Neither the
command nor the step commits the active record: uncommitted records the
lookup affects stay uncommitted until the active record is committed
([Committing data in records](https://help.claris.com/en/pro-help/content/committing-data.html)).
Not measured:
- the confirmation's wording and buttons;
- whether the item is enabled with no field entered, or in a field that
  is not a match field, and what that field answers;
- what the open record holds afterwards.

**Observed, bounded local case (#1194):** in FileMaker Pro 26.0.3, one
new record in a two-table fixture used stored Text calculation match
fields with option word `0xa0000020`. After an input field lost focus,
the key and looked-up Text changed to the uniquely matching payload,
first for one input value and then for its replacement. While the
replacement was still focused, the previous key and copied value remained
visible. The second field exit updated both before one deliberate blank
layout click. The closed source contained the replacement key and payload;
the target's entire active branch and field/layout/hook definitions were
byte-identical. The private result, raw delta and screenshots are sealed
by manifest `e609e98b5116b5192630a2839bdf4ae489a4318b77354d3e5809f51cd81b5011`.

**Inference and limits:** OpenMaker orders the copied field after the
calculated key and its consumers after the copy. A missing or deferred
key is not an empty match value. The native case does not isolate any
implicit commit during field exit or establish same-value input writes,
hosted behavior, general related calculations, or keys with option
`0xa0000000` and all inputs empty. Those conditions remain unmeasured;
the C126 aggregate observations do not establish them. A private read-only
complete-table planning check established a source-key type refusal,
not runtime creation or successful lookups in that application.

What FileMaker Pro 26.0.3.307 does [O, local, one A record over two B
records, `A::k = B::k`, both lookups "if no exact match: do not copy"]:

| A::k becomes | empty OFF | empty ON |
|---|---|---|
| a key B holds with `x` = `COPIED_VALUE` (new A record) | `COPIED_VALUE` | `COPIED_VALUE` |
| a key no B record holds, after both were typed over by hand | the typed value | the typed value |
| a key B holds with an empty `x` | empty | the typed value |

- The copy is made when the match field gets its new value: the value is
  visible once Tab leaves the field, before the commit, and stays after
  the commit and after closing and reopening the file.
- The copied value replaces what the field held, including a value typed
  by hand. With no matching record, "do not copy" leaves the field as it
  is.
- With a matching record whose value is empty, the lookup copies the
  empty value, unless "don't copy contents if empty" is on.
- The dialog says the lookup "will copy the value from the first matching
  related record in the table" [O, dialog text]. Through a sorted
  relationship that is the first in its sort order (s42, below); which
  record is first through an unsorted one is not measured.

**From a parent table** [O-run: script harness battery s41 on contacts-en
with Contacts::L2 added by FMUpgradeTool, looked up from
`Phone Numbers::Number` starting from Contacts, "Do not copy", "Don't copy
contents if empty" off; Contacts::PrimaryKey, an auto-entered and
replacing `Get ( UUID )`, = Phone Numbers::ForeignKey, creation allowed on
the Phone Numbers side]. Each step is Set Field and Commit Records in one
Contacts record:

| step | L2 after it |
|---|---|
| new record, PrimaryKey auto-entered, nothing related | empty |
| a related record made through the relationship, Number "555" | empty |
| First Name set (not a match field) | empty |
| PrimaryKey set to its own value | `555` |
| L2 typed over with "typed" | `typed` |
| PrimaryKey set to its own value | `555` |

- FileMaker Server's engine performs the lookup when the starting match
  field is set, as from a child table, even to the value it had. Making
  a related record does not perform it. The calculated values of
  "Calculations over related fields" below behave the same.
- So a lookup keyed on a parent's auto-entered `Get ( UUID )` is performed
  when the record is created. Nothing can match a new key, so it copies
  nothing, and it is performed again only when the key is set again.

**Through a sorted relationship** [O-run: script harness battery s42 on
assets-en with Assets::LA added by FMUpgradeTool, looked up from
`Assignments::Note` starting from Assets, "Do not copy"; the starter's
relationship Assets::PrimaryKey = Assignments::AssetForeignKey, sorted by
Assignments::Sort By descending (`If ( IsEmpty ( Date Returned ) ;
1/1/2099 ; Date Returned )`), creation allowed on the Assignments side].
Two assignments are made for one asset, one returned on 1/1/2020 and one
not returned, and then the asset's key is set to its own value:

| made first | made second | first in the sort | LA after the key is set |
|---|---|---|---|
| `first` (returned) | `second` (not returned) | `second` | `second` |
| `early` (not returned) | `late` (returned) | `early` | `early` |

- The lookup copies the first related record in the relationship's sort
  order, whichever was made first.
- Neither related record made performs the lookup; setting the key does,
  as in s41.

**Other data types** [O-run: script harness battery s43 on assets-en
with, added by FMUpgradeTool, Assets::AT (Time) and eight Assignments
lookups from Assets through the starter's relationship, "Do not copy",
"Don't copy contents if empty" off]. Each asset gets one assignment made
through the relationship, which sets its foreign key and performs the
lookups; the values are then read through it:

| lookup | from | asset 1 | asset 2 |
|---|---|---|---|
| LN, Number | Price (Number) | `12.50` (+1: 13.5) | `7 units` (+1: 8) |
| LD, Date | Purchase Date | `1/2/2020` (+1: 1/3/2020) | empty |
| LT, Time | AT (Time) | `13:45:30` (+60: 13:46:30) | empty |
| LTS, Timestamp | CreationTimestamp | the asset's own | |
| LXN, Text | Price | `12.50` (length 5) | `7 units` |
| LXD, Text | Purchase Date | `1/2/2020` | |
| LNT, Number | Name | `Widget 42` (+1: 43) | `1/2/2020` (+1: 122021) |
| LDT, Date | Name | `Widget 42` (+1: 1/1/0001) | `1/2/2020` (+1: 1/3/2020) |

- A lookup copies the related field's stored text as it is, whatever the
  two fields' types: a price entered as `12.50` stays `12.50`, not the
  number 12.5, and a date copied into a text field is its stored text.
- The target keeps text that is not of its type ("Widget 42" in a date
  field, "7 units" in a number field) and reads it as its type does
  elsewhere: the number of its digits, an invalid date. No validation or
  data-entry conversion is applied.
- An empty related value is copied as empty (asset 2's date and time).

A local Text equality check in FileMaker Pro 26.1.1 used a regular Text
key with calculated auto-enter. A constant formula produced its key and
copied value at New Record. A formula that read a Text input left the
new key and copy empty until Set Field supplied the input. With "Do not
replace existing value" off (option word `0x00000088`), changing the input
changed the key and copied value at Set Field. With it on
(`0x00000008`), the original key and copied value stayed. The values
survived commit and reopen.
[Native steps and results](https://github.com/OpenMaker-Dev/openmaker/issues/1036#issuecomment-7185794524).

FileMaker Pro 26.1.1, a local two-table Text lookup over an equality
relationship with "If no exact match: Use" selected
([native steps and results](https://github.com/OpenMaker-Dev/openmaker/issues/1020#issuecomment-7185793871)):

- New Record and Commit with the key untouched and empty leave the target
  empty. Setting the key to a missing match copies the fallback immediately.
  A blank fallback clears the target.
- An exact match copies the related value. If that value is empty, "Don't
  copy contents if empty" preserves the prior value. Neither empty-match
  setting copies the fallback.
- Explicitly clearing an existing key copies a nonempty fallback. The
  results survived commit and reopen, with LastError 0 and no alert.
- Lookup definition `08` stores mode byte `03`. FileMaker writes member
  `0b` for a nonempty fallback and omits it for a blank fallback.

Unmeasured for this option: explicit clear with a blank fallback, a replacing
`Get ( UUID )` match key, hosting, imports and other field types. More generally,
copying the next lower or higher value, container fields, Relookup Field
Contents, which of several matches an unsorted relationship gives, and
additional scripted and import cases remain unmeasured here.

## Reading a global from an unrelated table

Claris documents that a calculation can read a global field from any
context of the same file, including an unrelated table occurrence.
[Global fields](https://help.claris.com/en/pro-help/content/global-fields.html) and
[calculation context](https://help.claris.com/en/pro-help/content/calculation-evaluation-context.html).
In FileMaker Pro 26.1.1, a local Text field with calculated auto-enter
copied an unrelated table's Text global at New Record. An unrelated edit
kept that value. Changing the global affected subsequent new records,
and the earlier record kept its stored value. The same read and copy
worked with a saved global and zero records in its source table.
[Native Browse check](https://github.com/OpenMaker-Dev/openmaker/issues/1051).

## Calculations over related fields

**Stored calculated match fields (#1204).** Claris's
[relationship help](https://help.claris.com/en/pro-help/content/relationships.html)
permits calculated match values. A private complete-table static planning
check independently established a local kind-1 restriction as a refusal
for a general related-field read: both keys were stored Text calculations,
nonglobal and one repetition, with own stored inputs. The related Number
was stored; reading it does not imply evaluating its auto-entry formula.
This is separate from the lookup path described above.

**Inference:** compute a stored source key after its inputs, then evaluate
the general related read with that final key, followed by its consumers.
An input edit can recompute the key and reader. A stale prefetched answer
or a key whose calculation has not settled is not a successful empty read.
Native automatic timing, replacement and pending-value order for this
specific calculated-key related-read case remain unmeasured. These facts
do not establish all calculated joins or application creation support.

What FileMaker Server 26.0.3's script engine does with an auto-entered
calculation and a validation calculation that read a related field
[O-run: script harness battery s40 on contacts-en with three Contacts
fields added by FMUpgradeTool: A1 = `Phone Numbers::Number`, A2 the same
with "Do not replace existing value of field", V1 validated Always by
`Phone Numbers::Number ≠ ""`; `Contacts::PrimaryKey =
Phone Numbers::ForeignKey`, creation allowed on the Phone Numbers side].
Each step is Set Field and Commit Records in a Contacts record:

| step | A1 / A2 after it |
|---|---|
| new record, PrimaryKey "K1", nothing related | empty / empty |
| a related record made through the relationship, Number "555" | empty / empty |
| First Name set (not read by the formulas) | empty / empty |
| PrimaryKey set again to "K1" | `555` / `555` |
| PrimaryKey "K2", nothing related | empty / `555` |
| PrimaryKey "K1" | `555` / `555` |
| the related record's Number set to "777" | `555` / `555` |
| PrimaryKey "K2" then "K1" in one commit | `777` / `555` |

- The formula is evaluated when the record's match field of the
  relationship it reads through is set, even to the value it had, and
  then reads the related record as it is. These are the triggers
  schema-path.md 5.13 derives from the field's `fb` node. Making or
  changing the related record does not evaluate it.
- "Do not replace existing value" keeps a value the field has; the
  replacing formula writes what it reads, empty with nothing related.
- V1: a commit with V1 set and nothing related fails with 507; once a
  related record exists the same record commits (0). The validation
  calculation reads the related record as it is at the commit.

Not measured: several related records (which is first), sorted
relationships, paths of more than one relationship, aggregates of related
fields, and FileMaker Pro's own data entry.

## Formulas over an unstored calculation or a summary field

What FileMaker Server 26.0.3's script engine does with auto-entered
calculations and a validation calculation that read an unstored
calculation or a summary field of their own table [O-run: script harness
battery s44 on contacts-en with six Contacts fields that
`tools/fms-script-harness/s44_unstored_schema.py` adds (`SCHEMA=`): U =
`First Name & "!"`, unstored, "Do not evaluate if all referenced fields
are empty" on; A1 = `U & "?"`, replacing; A2 the same with "Do not replace
existing value of field"; V1 validated Always by `U ≠ "x!"`, also when not
modified; S = Count of First Name, a summary; A3 = `S & "#"`, replacing].
Each step is in one new record, Commit Records after it unless said:

| step | U / A1 / A2 / A3 after it |
|---|---|
| New Record, nothing set (before and after the commit) | empty / empty / empty / `0#` |
| First Name set to "a", before the commit | `a!` / `a!?` / `a!?` / `1#` |
| the same after the commit (error 0) | `a!` / `a!?` / `a!?` / `1#` |
| First Name "b" | `b!` / `b!?` / `a!?` / `1#` |
| Last Name "z" (read by no formula) | `b!` / `b!?` / `a!?` / `1#` |
| First Name "x" | the commit fails with 507 (V1) |

A second run, in a new record of a table holding the first run's record:
First Name "x" fails with 507, then "y" commits with 0, U `y!`, A1 `y!?`,
A3 `2#`.

- The unstored calculation is evaluated on the record as it is,
  committed or not, with its own "Do not evaluate if all referenced
  fields are empty": U is empty while First Name is.
- A formula reading it is evaluated again when one of the
  calculation's inputs is set, at the Set Field, before the commit: A1
  follows First Name, as if it read First Name itself. Its own "Do not
  evaluate if all referenced fields are empty" looks at U's value, which
  is empty while First Name is [I: no step separates the two].
- "Do not replace existing value" keeps the value A2 was given first.
- V1 reads U as the record holds it at the commit.
- A3's summary counts the records of the window's found set, the new
  record among them (1, then 2 in the second run), and is evaluated again
  when the summarized field is set.

Not measured: an unstored calculation over a related or global field, a
chain of unstored calculations, a summary of another kind or over a
sorted found set, and FileMaker Pro's own data entry.

## Creation Date with a calculated auto-entry value

FileMaker Pro 26.1.1 kept both options on one local Date field after
reopening, in either authoring order. New Record yielded the creation
date, including with a calculation that yields a different date and
with replacement enabled. An explicit date assigned before the first
commit survived reopening. The conditional formula reads its own field
and returns that value when present. It does not read `Note`, so the
unchanged date after editing `Note` does not establish re-evaluation.
Closed copies store option words `0x00000009` (keep an existing value)
and `0x00000089` (replace), both with Creation Date selector `0x00`
and a saved calculation member. In a subsequent local run, changing a
referenced `Start` date after reopening immediately replaced the creation
date with `Start + 30` when replacement was enabled. With replacement off,
clearing the target left it empty until a later `Start` edit calculated a
new date. The latter formula did not read the target itself. The timing
of a dependency edit before the first commit, the internal evaluation
order at creation, the always-evaluate option, Prohibit modification and
hosted behavior remain unmeasured.
[Native measurements](https://github.com/OpenMaker-Dev/openmaker/issues/39#issuecomment-7184309937).
[Closed-file readback](https://github.com/OpenMaker-Dev/openmaker/issues/39#issuecomment-7185113183).
[Post-creation measurements](https://github.com/OpenMaker-Dev/openmaker/issues/39#issuecomment-7185359635).

## A new record's own self-join

A new record's auto-entered calculation, reading its own table through a
self-join that no other record matches, reads the relationship as empty
[O, #1035's two native runs: FileMaker Pro 26.0.3, a local file, one portal
creation each].

- **Count/List case.** The witness `v & "|" & Count ( S::k ) & "|" &
  List ( S::v ) & "|" & List ( S::w )` is entered on a new portal row
  (k given by the portal, v typed 5) with no other record. It stores
  `5|0||`.
- **First-record case.** The witness reads the first related record's `g`
  and `r` through a self-join on `k` and `g`, both given. It shows them
  empty, while the same evaluation shows the record's own `k`, `g` and
  `r`. The table's only other record does not match.

**Computed non-empty key (C126).** A third local FileMaker Pro 26.0.3
portal-creation case starts with an empty child table. Typing `e=E1`
auto-enters `kk=KE1`; entering `v=5` and saving leaves the witness
`kk=KE1|n=0|vs=` (Count of the self-join's key and List of its value).
The closed file has exactly one child record with those values. This
observes an empty self-join at the witness's last stored evaluation,
including with a computed non-empty key; it does not establish a general
evaluation order. The only changed field-definition bit activates the
key's value index; other field definitions, formulas and relationship
catalogs are unchanged. Evidence: retained private C126 relay, four
checksum-verified artifacts, including closed-cell and raw-delta reports.

**One existing match (C126 matching follow-up).** The next disposable
local case starts with that committed row (`e=E1`, `kk=KE1`, `v=5`). A
new portal row is given `e=E1`; after Tab, before `v` is typed, its witness
shows `kk=KE1|n=1|vs=5`. It stays so while `v=7` has focus, after one
blank-layout click to save, and in the closed file. The old row and the
parent's active record leaves remain byte-equal. Thus at the witness's
last stored evaluation it saw the older matching row, without the new
row's value. This is a timing-bound observation, not proof of persistent
self-exclusion or its internal cause.

Evidence: five checksum-verified matching-relay files, including a closed
audit and all 28 raw deltas (11 new record paths, 9 index changes, 8
system/table-metadata or unclassified paths). Field definitions, graph,
layouts and trigger caches, scripts and file options are unchanged;
globals are absent. Base-catalog/table-version counters change: neither
the whole file nor every metadata byte is unchanged.

The first three cases had no other related record; the fourth has exactly
one older match.

**Records a script makes or edits** [O: FileMaker Server 26.0.3's engine,
script oracle `s47-self-join-ae`, 2026-11-19]. Contacts got an occurrence
Mates joined on `Contacts::Company = Mates::Company` (equality, unsorted)
and a text field SJ auto-entering, replacing, the witness `Company | First
Name | Count ( Mates::PrimaryKey ) | List ( Mates::First Name ) |
Mates::First Name` (List's returns as commas), each evaluation also
appended to a variable. Each run line is a server session; each record
was made or edited by Set Field and committed. Stored witnesses:

| Probe | Records of the group before | Stored SJ |
|---|---|---|
| New a1, Company G2 then First Name | none | `G2\|a1\|0\|\|` |
| New a2, the same | a1 | `G2\|a2\|1\|a1\|a1` |
| New n3, the same | a1, a2 | `G2\|n3\|2\|a1,a2\|a1` |
| New k4, First Name then Company | a1, a2, n3 | `G2\|k4\|3\|a1,a2,n3\|a1` |
| Stored c moved from G0 into G2 | a1, a2, n3, k4 | `G2\|c\|4\|a1,a2,n3,k4\|a1` |
| Stored a1 (in G2) renamed a1x | a1, a2, n3, k4, c | `G2\|a1x\|5\|a1x,a2,c,n3,k4\|a1x` |
| New h1 and h2 (G5) in one Open Transaction ... Commit Transaction | none | h1 `G5\|h1\|0\|\|`, h2 `G5\|h2\|0\|\|` |

Each Set Field of a field the witness reads evaluated it again, and the
stored value is the last evaluation's (a2: `G2||1|a1|a1` after Company,
then `G2|a2|1|a1|a1`; k4: `|k4|0||` with Company still empty). A later
change to the group did not change the stored witnesses (the final dump
read a2's as stored).

**The reading.** The self-join finds the records whose committed key
matches the record's key as it is being written: the others committed
before it, in creation order. A record not yet committed is not among
them, whether it is the record being written (new, or moved into the
group by this edit: its committed key is G0's) or another record held in
the same transaction (h2 does not see h1). A stored record whose
committed key already matches is among them, in its creation place, read
with the values being written (a1x, not a1). Aggregates and the first
related record read that set.

**Unsorted group join with unequal row keys.** FileMaker Pro 26.1.1
measured two predicates on the same table: equal Text group keys and
unequal Text record keys, with no relationship sort. The Time field's
do-not-replace formula read fields of the first related row directly.
New Record, setting the date and time, and the first commit left the
result empty while the group key was empty. After assigning the group
key on the second and third rows, the result became `0:00:10` and
`0:00:30` during Set Field. Commit and reopen retained those values.
The two intervals identify the oldest matching row in this setup. The
first row retained an empty result after the later rows joined its group.
[Native measurements](https://github.com/OpenMaker-Dev/openmaker/issues/297#issuecomment-7184309703).

Still unmeasured:
- sorted joins and other non-equality and multi-predicate joins,
  stored calculations and validation, and portal rows created in one
  commit (the transaction case is a script's);
- other stored outcomes for auto-entry that keeps its first value,
  including additional writes between the group assignment and commit;
- whether FileMaker Pro reads the same in a local file for these cases (the
  one-older-match case above agrees). Applying the reading to keys the
  record auto-enters itself is inference.

## Auto-entered value checked against a value list

One local FileMaker Pro 26.0.3 synthetic portal-creation case (C125,
outcome B) saved an auto-entered value absent from its custom value list
without a validation dialog. The closed record stores that value and a
second witness field that reads it. Its field-definition/formula subtree,
including auto-entry `0x8` and validation `0x8180`, is unchanged across
166 keys; relationship and list catalogs are unchanged too (retained
private relay, closed raw differential).

This is a native-authored calculated-literal-message `0x8180` variant.
It does not directly measure the legacy `0x180` plain-message definition
or related-values lists. Carry-over to them is inference, and this case
does not distinguish validation being skipped from other internal timing
that saves without a dialog. Hosted behavior is unmeasured.

## Stored Evaluate with explicit dependencies

FileMaker Pro 26.1.1 stored a Text calculation of the form
`Evaluate ( "Given & Char ( 32 ) & Family" ; [ Given ; Family ] )`.
New Record left it empty. Changing either listed field recomputed it at
Set Field. A field the text also read and the list omitted did not trigger
recalculation. The next listed change read its current value. The
results persisted through commit and reopen. Reading the expression
from a field was also allowed and writing the expression field itself
triggered that variant. The literal definitions used option word
`0x00080000` and stored their result cells. Hosted files were not compared.
[Native results](https://github.com/OpenMaker-Dev/openmaker/issues/1072)

## Get ( CurrentTimeUTCMilliseconds )

FileMaker Pro 26.1.1 in a local file displayed the UTC millisecond value
of a Text auto-enter calling a custom function at New Record. Its value
fell inside a bracket around that operation and survived an unrelated
Text edit and close. A direct Number auto-enter was stored as well. With
replacement enabled, one record stored the direct and custom results
one millisecond apart, so different field evaluations can take different
samples. These checks did not change an input the calculation reads.
[Native Browse check](https://github.com/OpenMaker-Dev/openmaker/issues/1055).

## Get ( RecordNumber )

FileMaker Pro 26.1.1 [measured sorted ordinary New Record](https://github.com/OpenMaker-Dev/openmaker/issues/1011)
with a stored Number calculation consisting of `Get ( RecordNumber )`.
When sorted, a new row initially follows the current row. When unsorted,
it goes at the end. A new window kept the found set, sort and current
record. The stored value held the insertion position across commit and
later reordering. The live record number changed from 3 to 2 in one
sorted case while the stored value remained 3.

**Stored Number calculation.** [O: FileMaker Pro 26.1.1, local disposable
file, [#1011](https://github.com/OpenMaker-Dev/openmaker/issues/1011#issuecomment-7184565879).]
FileMaker lets a Number calculation whose entire formula is
`Get ( RecordNumber )` store its results (option word `0x00080000`).
After scripted New Record in an unsorted found set, the field showed the
record's position immediately. A later Set Field on another field and Commit
kept that number. Omitting an earlier row before making a record gave the
new row's position in the reduced found set. A later sort or omit changed
the window position while the stored result stayed as first evaluated. In
one descending sorted set, New Record produced a stored 2 and window
position 2. The closed copies held the stored numbers across unrelated
edits and reopening. All script steps returned 0 with no alerts. Timing
before the first read remains unmeasured, as do duplicates, portals and
hosted files for this stored formula.

Two cases [O: FileMaker Pro 26.0.3, a local disposable pair made in the
native UI, #1038, relayed sanitized].

**The setup.**
- Root has two records, and its current record is at position 2.
- B has five records.
- `Root::MatchKey = B::MatchKey`, with creation allowed in B.
- Root's layout has a portal on B that neither sorts nor filters.
- B's Number field auto-enters `Get ( RecordNumber )`, with "Do not
  replace existing value" on.
- The two cases differ in what was related before:
  - none of B's records;
  - B's records 1 and 2, shown as the portal's first two rows.

**What happened.** A value was typed into the portal's creation row (the
first row, then the third), then an ordinary commit (a click on the
layout):
- **While typing,** the field stayed empty. OnObjectEnter and
  OnObjectModify scripts reading `B::Number & "|" & Get ( RecordNumber )`
  gave `|2`.
- **At the commit,** the row showed `1`, then `3`. An OnObjectSave
  script gave `1|2`, then `3|2`.
- **The closed file's new B record 6** stores `1`, then `3`.
- Schema, field options, layouts and relationships were unchanged by the
  second case [O: raw catalog comparison].

**The reading.**
- **The value is entered at the save,** and an OnObjectSave script
  already sees it. The script's own `Get ( RecordNumber )` is Root's
  position, 2.
- **The value is the new row's place among the portal's related records,**
  after those related before. It is not Root's position (2) or B's record
  count (6).
- **That a hosted file answers the same** is [I], OpenMaker's choice
  (parity-unverified).

**Records a script makes** [O: FileMaker Server 26.0.3's engine, script
oracle `s46-record-number-ae`, 2026-11-19]. Each run line is a server
session, as Perform Script on Server or a schedule runs one. Contacts got
two text fields auto-entering `Get ( RecordNumber )` and `Get (
FoundCount )`, both replacing an existing value ("Do not replace existing
value" off). Each probe made one record in its window's found set (most
then set its First Name), committed it and read both fields and the
window's own selectors:

| Probe | Found set before | Stored (both fields) |
|---|---|---|
| New Record after Show All Records (3 records) | 3 | 4 |
| the same, the current record moved to the first | 4 | 5 |
| Show All Records (5), the first two omitted | 3 | 4 |
| a find that matched nothing (error 401) | 0 | 1 |
| two New Records in a row (7 shown); the first one's read right after its New Record, before its commit | 7, then 8 | 8, then 9 |
| Duplicate Record of the first (9 shown) | 9 | 10, not the copied record's 1 |

**The reading.** A record a script makes joins the end of an unsorted
found set, wherever the current record is, and its auto-entry reads that
position: n + 1 over the n records found before, whatever the table holds
(6 records when the omitted probe stored 4). It is entered when the record
is made: the first of two new records read 8 before its commit. A
duplicate's replacing auto-entry is evaluated again, not copied; one that
does not replace an existing value is not measured. FileMaker Pro's own
New Record command runs the same window operation, so it is inferred to
store the same [I].

Unmeasured [U]:
- portals that sort or filter their rows;
- more than one new row in one commit;
- a sorted found set's new record, and one whose place changes before its
  commit (an earlier record omitted in a transaction);
- New Record from FileMaker Pro's menu or toolbar (inferred above, not
  run).

## Serial numbers

Option `0x2` generates a serial number when the record is created, `0x200`
(set instead) when it is committed; the increment is `mSerialIncrement`
and the next value is the field member's `0c`, text (schema-path.md 5.5,
5.9 E41). What FileMaker does:

- The client takes each value from the host: op72 with `kIncrement` and
  the field's increment returns the next value and stores the one after
  it (write-path.md 3.2) [O, cap:916]. On creation, FileMaker Pro sends it
  right after New Record's op71, before the user sees the record [O,
  cap:914-916]. On commit, the value is taken when the record is
  committed [doc: `automatic-data-entry.html`]; its op72 was not captured.
- The host advances the text's last run of digits and keeps the run's
  width ("INV0099" + 1 = "INV0100") [Med, write-path.md 3.2].
- Duplicate Record does not copy an auto-entered value: the copy gets a
  new one, a lookup's or a calculation that does not replace an existing
  value excepted [doc: `duplicate-record-request.html`].
- Revert Transaction does not take back a serial number's increment [doc:
  `revert-transaction.html`].

Not measured: whether reverting a new record outside a transaction gives
its serial back, whether an existing record whose on-commit field is
empty gets a serial when it is next committed, which value a record keeps
when a script or the user sets the field before an on-commit serial is
generated, text without digits, and increments below 1.

## Auto-entry into global storage

FileMaker Pro 26.1.1 kept a Text field's Auto-Enter setting when its
storage changed to global. In each disposable file the option word gained
only `0x10000`. The constant-data member `0d`, calculation member `05`,
or next-serial member `0c` remained, according to the option used.
With the global initially empty, New Record set it to `X` for the
constant and calculation cases, and to `1` for the serial case. Editing
another field and committing kept that value. Set Field on the global
then replaced it with `Y`, which survived a local reopen. All scripted
steps returned 0 and continued without alerts. The hosted constant
case on FileMaker Server 26.0.3 followed the same sequence in the open
session. After reopening, the hosted global was empty again.
[O, disposable FileMaker Pro runs](https://github.com/OpenMaker-Dev/openmaker/issues/987#issuecomment-7183655256).

A second local run started each variant with an empty global, created
and committed a record, set the global to `Y`, then created and committed
another record. The second New Record entered `X` for a constant or a
replacing calculation, retained `Y` for a calculation with replacement
off, and entered `2` for the creation serial with an increment of 1.
All steps returned 0 and the resulting globals survived a local reopen.
[O, second New Record](https://github.com/OpenMaker-Dev/openmaker/issues/987#issuecomment-7184310177).

Closed copies read back the serial's next-value member `0c` as `1`
before creation, `2` after one creation, and `3` after two. The resulting
records had no cell for the global field. This measures the counter
persisted at close. The timing of the counter write before close and
the result of reverting a newly created record were not measured.
[O, closed-file readback](https://github.com/OpenMaker-Dev/openmaker/issues/987#issuecomment-7185027447).

### Self in a calculated global Text auto-entry

[O, FileMaker Pro 26.1.1] A local single-repetition global Text field
stores option `0x00010088` for a replacing calculation and `0x00010008`
with replacement off. The measured formulas were
`If ( IsEmpty ( Self ) ; "Initial" ; Self )` and the same formula with
`Self & "!"` as the final branch. The empty-input control was left on.
New Record sees the current global through Self. Set Field sees its
proposed text through Self. With replacement off, a nonempty value
survives either operation. Clearing the field re-enters the default.
Writing an unrelated field leaves the global unchanged.
These scripts all continued with LastError 0. The local global persisted
across reopen. Hosted files, Browse typing, Duplicate Record and portal
creation were not tested.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1095#issuecomment-7191245589)

## A stored calculation before the commit

A stored calculation of the record being edited reads its new inputs
before the record is committed [O, #1104's native run: FileMaker Pro
26.0.3, a local file, one script run]. A script on a new record does Set
Field `Count` to 4. Then, before any commit, it reads `Doubled` (a stored
`Count * 2` in the same table, own context) as 8, with
`Get ( RecordOpenState )` 1. It then reverts. The closed file holds no
record, and its schema is unchanged.

The run covers only this: a script's Set Field, a local file, a stored
calculation of the record's own table over its own field. The user's
typing, OnObjectModify, a hosted file, related or lookup inputs, and
auto-entered calculations are not measured.

## Client versus server: what is established

The desktop DBEngine implements default evaluation, not just metadata
forwarding. `DBDependencyList::EvalOneRep` (`0x1754de4`) calls
`AutoEnterData`; its calculation branch reads the calculation code, calls
`DBCalcFormula::Solve` (`0x17556d4`) and writes the resulting typed value
through the record cursor. `DoAutoEnterSerial` explicitly calls
`DBField::GenerateSerialValue` for a newly created record. Serial reservation
must not be replaced by a local counter.

This supports treating auto-entry, dependency recalculation and validation as
client responsibilities until an official-client capture and a native
round trip establish otherwise. It does **not** prove that the server never
evaluates them in other execution contexts, or that the server repairs an
incomplete raw upload. The op70 commit framing alone provides no such promise.

Creating a row with a blank PrimaryKey or blank required audit fields is
therefore not a valid CRUD implementation. Updating First Name or Last Name
also requires the stored Initial result to be recalculated. Creation values
are preserved by the bounded planner during later updates; the hosted round
trip verifies account/timestamp generation, UUID uniqueness, stored Initial
recalculation and exact version advancement. General formula coverage,
additional validation and storage conversions remain unsupported and fail
schema preflight. A successful string upload alone is insufficient to expose
editing on an unsupported schema.

## Reproduction

`cargo run -p om-provider --example field_options -- <owned-local-file>` prints
only field metadata and recognized selectors, without record values or full
calculation bodies. The opt-in real-file test is:

```sh
OPENMAKER_CONTACTS_TEMPLATE='<owned-local-file>' cargo test -p om-hbam --test auto_entry -- --ignored
```

Unit tests cover inactive variable 0, unknown bits/codes, exact definition
length, validation masks and complete-expression recognition. The real-file
test checks all definitions, the five actual generated fields, UUID code and
Initial's stored-calculation flag without making changes.
