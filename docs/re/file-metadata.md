# File metadata: locale, file options, relationships, summaries, value lists, triggers, ids

What a file stores about itself and its schema beyond tables and fields,
to the depth a host needs to compute FileMaker's results: the file's
language and formats, the document preferences that hold the file script
triggers, relationships with every operator and their related-record
order, summary field definitions, value lists, the complete layout and
object script trigger record, and how catalog keys map to the ids
FileMaker shows.

Status (2026-10-29): every structure below is decoded by
`crates/om-hbam/examples/file_metadata.rs` across all 746 corpus files with
no undecodable record, and the decoded values equal FileMaker's own Save as
XML on every compared record (section 9). Values the corpus never uses were
produced by FileMaker itself from a probe document and checked the same
way; relationship, summary and value-list semantics were executed with
FileMaker Server's script engine.

Legend: **[O]** observed (engine code, stored bytes, an engine run or
FileMaker's XML), **[I]** inference, **[U]** unknown. Addresses are `nm`
values of FileMaker Server 26.0.3 (`libDBEngine.so`, `libFMEngine.so`,
`libSupport.so`); names are DWARF names. Integers are big-endian. Key
paths are written with hex segments as in schema-path.md (`[3, fb]`,
`[21, 5, V]`; file-format.md 3.4 lists the same top-level keys in
decimal).

Evidence:

| source | what it gave |
|---|---|
| `Draco::Locale::Write`/`Read` (libSupport 0x1c1620, 0x1bd570), DWARF `Saved_International`, `International` vtable, `International::GetFirstDayOfWeek` (0x156ec0), `Locale::GetNonLocalizedName`/`GetNonLocalizedLanguage`/`GetISO639_1FromType` (0x157660, 0x157680, 0x158f70) | the locale record and the values derived from it |
| `DocumentPrefs::Read` (libDBEngine 0xf526a0), DWARF `DocumentPrefs`; `DynamicTemplateXML_Director::CreateMetadataXML_*` (libFMEngine 0xbf0d30-0xbf4190) | the file options record and its option bits |
| `DBRelation::Definition::Read` (0x10fef10), `DBJoinPred::Read` (0x10fcd30), DWARF `DBRelation::Definition::Options`, `DBJoinPred::Type` | relationships |
| `DBField::GetSummaryDef` (0xee89c0), DWARF `DBField::SummaryDef`, `SummaryDef::GetSubTotalFunctionFromSummaryInfo` (0xeea1c0), `XMLField::GetSummaryOperationVal`/`GetSummarizeRepetitionVal` (libFMEngine 0xd1c0a0, 0xd1c1a0), `ObjectTransferFields::XMLSerialize_SummaryOptions` (0xc499f0) | summary definitions |
| `DBValueList::Definition::Read` (0x121ecd0), DWARF `DBValueList::Options`, `ValueListType`, `DBValueList::IsCalculationBased` (0x121e4d0), `XMLValueList::GetTypeVal` (libFMEngine 0xdcd7d0) | value lists |
| `ScriptTrigger::Read` (libFMEngine 0xfe9de0), `ScriptTriggerCache::Read` (0xfea760), `LayoutObject::LoadScriptTriggerCache` (0xf84e20), DWARF `ScriptTrigger` | layout and object triggers |
| `tools/fms-calc-harness` on the 11 Contacts templates and three other files | `Get ( FileLocaleElements )` and locale-dependent results |
| FileMaker Server's `FMDeveloperTool --saveAsXML` of 143 corpus files (`re/work/scripting/specs/xml/`) | the meaning of every stored value, compared mechanically (section 9) |
| `FMUpgradeTool --generateDBFile` on a probe document (`tools/file_metadata_probe.py`) and the script engine of `tools/fms-script-harness` | FileMaker-written values the corpus lacks, and their semantics (sections 4.4, 5.4, 6.4) |

---

## 0. Where things are

| structure | key | reader | section |
|---|---|---|---|
| file locale and formats | `[2, 0d]` (`KStatus`, `KFormat`) | `Locale::Read` | 2 |
| document preferences: file options, default layout, file script triggers | `[2, 18]` (`KDefaults`) | `DocumentPrefs::Read` | 3 |
| relationships | `[3, fb, 5, R]`: `02` definition, `03, n` predicates | `DBRelation::Definition::Read`, `DBJoinPred::Read` | 4 |
| summary definition of field F | `[T, 3, 5, F, 09]` (`KSummary`) | `DBField::GetSummaryDef` | 5 |
| value lists | `[21, 5, V]` (`KValueLists`): `02` definition, `03` custom values, `10` name | `DBValueList::Definition::Read` | 6 |
| layout triggers | `[4, 5, L, 30]` (`KScriptTriggers`) | `ScriptTriggerCache::Read` | 7 |
| object triggers | `[4, 5, L, 5, O, 30]`; table view objects `[4, 5, L, 0d, 5, O, 30]` | same | 7 |
| catalog member ids | the member's key segment, a serial key | `HBSerialKey` | 1 |
| field annotation and display names | `[T, 3, 5, F]`: `30` annotation, `31` display-names formula, `29` advanced flags | `DBField::GetDisplayNameJson`, `FieldInfo` | 1.2 |
| a table occurrence's saved found set, sort and current record | `[TO, 07, 01]`; last find `[TO, 07, 02]` | | 11 |

Stream conventions (as script-steps.md 11.0): a **key** is `[u8 n][n
bytes of segments]`, each segment `[u8 len][bytes]`; a **field spec** is
occurrence key, field key, u16 repetition; a **string** is a UTF-16 unit
count (one byte below `0x80`; u16 with `0x8000` set below `0x4000`; `c2` +
u16; `c4` + u32) followed by SCSU bytes XOR-masked with `0x5a`
(`SwapOStream::InternalSCSUCompress`, libSupport 0x1f3e60) [O]. A **sort
spec** is `DBSortSpec::Write` (script-steps.md 11.1.1); inside relationship
and value-list definitions an absent value-list key is written `00`, not
`02 01 00` [O].

---

## 1. Catalog ids are serial keys [O]

Every catalog member is stored under `[cat, 5, key]`, and the id FileMaker
shows (XML `id` attributes, Design functions such as `ScriptIDs`,
`LayoutIDs`, `ValueListIDs`, `FieldIDs`, `TableIDs`, script and layout
references in steps and triggers, Perform Script on Server's script key, which FileMaker Pro sent after a length byte for two scripts, `01 0a` for script 10, inferred for the rest, script-runtime.md 11.2) is that key segment decoded as an
`HBSerialKey` (query-program.md 2.6, `om_hbam::serial::decode`): the count
of leading 1 bits of the first byte gives the length, the remaining bits
plus the length's offset give the value. One-byte keys `01`..`7f` are the
same either way; from 128 on they differ: script key `80 0d` is id 141
(not 32781), table `80 02` is 130, occurrence `d0 00 02` is 1065090, field
`80 80` is 256 [O: all 143 exported files, every catalog].

| catalog | member path | ids in the corpus ≥ 128 |
|---|---|---|
| base tables | `[3, 10, 5, T]` | all (first table is 130) |
| table occurrences | `[3, 11, 5, O]` | all (first occurrence is 1065090) |
| relationships | `[3, fb, 5, R]` | none |
| fields of table T | `[T, 3, 5, F]` | 159 of 8,734 |
| layouts | `[4, 5, L]` | none |
| scripts and script folders | `[11, 5, S]` | 91 of 3,639 (e.g. `80 0d` = 141) |
| value lists | `[21, 5, V]` | none |
| custom functions | `[10, 5, C]` (`KGlobalFunctions`) | none |
| accounts | `[17, 1, 5, A]` | none |

Ids are allocated from `[cat, 1, 00]` (schema-path.md 5.1) and the
display order is `[cat, 1, 03, n]`; `ValueListIDs`, `ScriptIDs` and the
other list functions return the display order, not id order: a probe
whose value list 103 was created before 102 returns `…,101,103,102` [O,
script engine]. An empty key (`00` or the segment `00`) is id 0 ("none").
The Guest account's stored name is `[` U+E002 `]`; FileMaker shows
`[Guest]` [O].

Consequence for `om-scripts`: `Script::id` must be the serial value of the
key, not its big-endian reading (docs/scripting.md open item).

### 1.1 Display order, member flags, comments, data sources [O, 2026-10-29]

- Each display-order entry `[cat, 1, 03, n]` is one key segment with its
  length byte: `01 1d` (member `1d`), `02 80 02`, `03 d0 00 02` [O: every
  probed file]. FileMaker Server's `LayoutNames`, `ScriptNames` and
  `ValueListNames` follow it (calc-functions.md 5.8); `TableNames` and
  `BaseTableNames` do not (they sort by name).
- Layouts `[4, 5, L, f0]` and scripts `[17, 5, S, f0]` (an 8-byte
  big-endian word, also in the layout index `[4, 1, 07, L, f0]`): `0x01` a
  folder (`0x04` and `0x08` seen beside it), `0x02` the end of a folder
  (named `--`, XML `isFolder="Marker"`), `0x08` not shown in the layout or
  Scripts menu, `0x10` a separator (named `-`) [O: FMS sample, the
  templates, MBS examples; `LayoutNames` leaves out `0x01` and `0x02`
  members, keeps the rest]. A script folder keeps an empty step list (key
  `4`), so the flags, not the step list, tell it from a script. A layout
  separator has a table occurrence like a layout.
- A base table's comment is `[3, 16, 5, T, 03]` (text, `BaseTableComment`).
- An auto-enter serial field's next value is `[T, 3, 5, F, 0c]` (text,
  `GetNextSerialValue`; schema-path.md 5.5).
- External data sources are `[32, 5, D]` (`[0x20]`), named at `10`
  (`Event Management AuditLog`); key `82` holds the file reference. An
  occurrence of another file's table has the definition `02 01 <data
  source> <base table key in that file> ...` where a local one has `02 01
  00` (query-program.md 8.7). The Design functions list such occurrences;
  a single file's catalogs do not hold their fields.
- A data source's file reference `82` is its file path list [O,
  2026-10-29: the data sources of a hosted multi-file solution]: a `u32`
  count of alternatives, then three strings per alternative (section 0's
  string form): a scheme, a location and a file name. A file on the same
  host as the referencing file, named relative to it, is `("file", "",
  "Related")`. That the counts are
  UTF-16 units, as in every string of section 0, is [I]: the observed
  names are ASCII. An `fmnet:/host/File` path is `("fmnet", "\u{E000}host\u{E000}",
  "File")`, and `fmnet:/host:5003/File` keeps the port the user typed,
  `("fmnet", "\u{E000}host:5003\u{E000}", "File")` [O: FileMaker Pro 26.0.3
  saved each as the one path of a local file's data source,
  `fmnet:/example.invalid/Related-214.fmp12` in 48 bytes and the `:5003`
  form in 53, [#232](https://github.com/OpenMaker-Dev/openmaker/issues/232)]. The
  two U+E000 characters are stored, each written through a dynamic SCSU
  window at U+E000, and the dialog shows them as the path's `/`. Not
  measured: a host with a folder after it, IPv6 addresses, several
  alternatives of mixed forms, and `filemac:`, `filewin:` and the other
  forms of Claris's file path syntax ("Creating file paths"), which
  presumably take the same three strings [I].
  The desktop client follows a relative `file` alternative on the host
  and port of the file whose data source it is, and an `fmnet` one on the
  host and port it names (no port is port 5003), in their order
  (network-protocol.md 5.5, "Related files"), and imports the file's
  calculation schema (calc-values.md 13, "Related files' formulas"). On
  the opened file's host it opens it on that file's connection with the
  same account. A file on another host or port is sent no account of the
  opened file's: the window asks for one, and only then is that host
  contacted ([#272](https://github.com/OpenMaker-Dev/openmaker/issues/272)). Hosts are compared by their text in any case, never
  looked up. A folder on a host is not followed. FileMaker Pro following
  an `fmnet` path from a hosted file, to its own host or another, is
  unmeasured.
- Both leading parts of an occurrence definition are keys (section 0),
  not single bytes: the data source (`02 01 00` for this file, else its
  `[32, 5, D]` member key), then the base table's key in that source's
  file [O, 2026-10-29: a hosted multi-file solution]. A member key is a
  serial key (section 1), so data source 128 is `03 02 80 00`; reading
  the byte after `02 01` misses it. OpenMaker reads another file's tables
  by importing that file's calculation schema (calc-values.md 13,
  "Related files' formulas").

  A native hosted capture has an occurrence whose definition begins
  `02 01 09 03 02 80 01`: source key byte `09` (id 9), then raw base-table
  key bytes `80 01` (serial id 129; section 1) [O, s03 conn-000, op90
  request 786 for the source file's `[3]`]. Source `9` exists at
  `[32, 5, 9]` in that file's op90 request 788. The related
  file's complete `[3]` reply to op90 request 386 has only base-table
  member with raw key bytes `80 02` (serial id 130), not `80 01` [O,
  159,024 slot bytes, continuation reassembled]. The occurrence therefore
  names a base-table key absent from that related file as the host sent
  it. This is a catalog observation; the capture does not show what
  FileMaker Pro drew for the occurrence, and it does not establish that a later copy of the
  solution has the same catalog.

  A later read-only hosted probe downloaded complete `[3]` and `[32]`
  catalogs from nine files after successful Login under one account
  [O, 2026-11-16, private `current-mapping.md`, 18 raw slot streams and
  SHA-256 manifest]. In one file, seven occurrence definitions reference
  source keys `01`, `03` and `04`, while its `[32, 5, D]` members are only
  `05` and `06`. In a second file, one occurrence references source key
  `15` (serial id 21), while its 15 source members omit `15`. Each
  definition's two length-prefixed keys decode cleanly; the remaining
  graph bytes are retained unchanged. No
  name `10` or path-list `82` can be read for those four referenced keys
  because their entire source members are absent from the catalog as
  sent. This distinguishes these cases from sources whose members exist
  but use an unknown name or path encoding. It does not show whether the
  host filters members for this account or whether the on-disk file has
  them.

  Three more occurrences in the same probe have present source members
  and reference raw target base-table key `80 01` (serial id 129). Two
  resolve to one related file and one to another; both related files'
  complete `[3, 10, 5, T]` catalogs have only raw key `80 02` (serial id
  130). This confirms the dangling keys in the current host replies for
  this account, including the pattern in the older native capture. It
  does not establish what FileMaker Pro displayed in the current files.

  Two other source members are present with decodable path lists: one
  offers an empty-scheme variable path followed by a relative `file`
  path; the other offers a relative `file` path followed by an `fmnet`
  path with a nonempty location. Their related-file Open misses in the
  OpenMaker run do not establish a catalog-decoder defect. Native Pro's
  path selection and the listed files' current availability were not
  measured [O: stored paths and OpenMaker report; U: native resolution].

  A local FileMaker Pro 26.1.1 check on disposable files measured a source
  whose only path is a global variable ([#988](https://github.com/OpenMaker-Dev/openmaker/issues/988#issuecomment-7183655010)).
  Opening on a layout that reads only local fields showed no alert even
  with the variable unset. Switching to a layout that read the source
  then showed a Not Found alert naming the variable and a file chooser
  in the opening file's folder. Cancel left the related field showing
  `<File Missing>`.
  A startup script with error capture on ran hidden Open File on this
  source with four values: relative `file:Peer.fmp12` and absolute
  `filemac:` each opened the peer with error 0, unset returned 802, and
  a missing sibling returned 100. Each continued without an alert or
  chooser. A related read after setting the variable also opened the peer;
  assigning another path and rereading still used the first peer in this
  session [O, one sequence]. The closed source copies were byte-identical
  to the original [O].

### 1.2 Field annotation and display names [O, 2026-11-11]

FileMaker 26's Advanced Options for a field ("Add annotation in Data
Definition Language (DDL)", "Customize field display names") are three
members of the field `[T, 3, 5, F]` (schema-path.md 5.5):

| key | constant | value |
|---|---|---|
| `30` | `KAIAnnotation` | the annotation, text (masked SCSU like the field's comment `03`); absent when empty |
| `31` | `KFieldDisplayNames` | the display-names formula, stored as every field formula is (calc-encoding.md 8): `04` the context occurrence's key string, `05` the formatted stream, `06` the Polish stream |
| `29` | `KAdvancedFlags` | a big-endian `u32`; bit `0x2` (`DBField::AdvancedFlags::kFieldDisplayNames`, the only enumerator) is "Customize field display names". Absent when 0 |

The formula is kept when the option is off: then `31` is present and `29`
is absent or lacks `0x2`.

`FieldAnnotation` returns `30` as stored. `FieldDisplayNames` returns empty
unless `29` has `0x2` and `31` exists; it then evaluates the formula and
returns its result text, unchanged, when that text is JSON of any kind
(an object, an array), and empty otherwise. The formula is evaluated on
the current record of its context occurrence, and without a record when
that occurrence is another file's [from bin]. Claris documents the
object's keys ([Advanced field options](https://help.claris.com/en/pro-help/content/advanced-field-options.html)):
`fm_export` (the export field order dialog and exported field names),
`fm_sort` (the Sort Records dialog), `fm_table_view` (Table View column
headers) and `fm_common` (each of those features unless its own key is
set); a feature with neither key uses the field name, and custom keys
are allowed (`fm_` is reserved). FileMaker's own lookup
(`DBField::GetDisplayFieldNameOnly`) does exactly that and needs the
result to be a JSON object [from bin].

Evidence:

- **Code** (FileMaker Server 26.0.3, DWARF): the Design function handler
  `FieldInfo` (`libFMEngine.so` 0x9f6e70) answers function 20
  `FieldComment` from `KDescription` (`03`), 25 `FieldAnnotation` from
  `KAIAnnotation` and 26 `FieldDisplayNames` by testing `KAdvancedFlags`
  bit `0x2` and calling `DBField::GetDisplayNameJson` (`libDBEngine.so`
  0xef1c10). That function solves the formula of
  `DBField::CalculationSelector` `kFieldDisplayNamesCalc` (5), whose key
  `DBField::GetCalculationKey` (0xeea7c0) maps to `KFieldDisplayNames`,
  and keeps the result only if it parses as JSON.
  `DBField::GetDisplayFieldNameOnly` (0xee24c0) reads `fm_export`,
  `fm_sort` or `fm_table_view` by `FieldNameAliasType` and falls back to
  `fm_common` (`DBField::kAlias_*`, built in `libDBEngine.so`'s static
  initializer). FMEngine's Save as XML writes each field's `Annotation`
  (with `Text`) and `DisplayNames enable="True|False"` (with the formula
  as `Calculation`) last in the `Field` element
  (`ObjectTransferFields::XMLSerialize_AnnotationOptions`,
  `XMLSerialize_DisplayNamesOptions`, FMEngine x86_64 0x1075b0, 0x10711e).
- **A file FileMaker wrote** (oracle harness: FileMaker Server's
  `FMUpgradeTool --generateDBFile` on the Contacts starter solution's
  Save as XML with six fields given annotations and display names, then
  the script engine's Design functions on the result):

  | Field: stored | `FieldAnnotation` | `FieldDisplayNames` |
  |---|---|---|
  | First Name: `30` "Given name of the contact"; `29` = 2; `31` = `JSONSetElement ( "{}" ; [ "fm_common" ; "Given Name" ; JSONString ] ; [ "fm_table_view" ; "First" ; JSONString ] ; [ "my_short" ; "FN" ; JSONString ] )` | Given name of the contact | `{"fm_common":"Given Name","fm_table_view":"First","my_short":"FN"}` |
  | Last Name: `30` "Ä – 日本" (a node: `00` = `u32` 10 bytes, `u32` 6 characters; `01` the bytes, file-format.md 3.6); `29` = 2; `31` = `"not json"` | Ä – 日本 | empty |
  | Company: `31` only, option off | empty | empty |
  | Job Title: `29` = 2; `31` = the text `{ "fm_sort" : "Job", "n" : 1 }` | empty | `{ "fm_sort" : "Job", "n" : 1 }` (spacing kept) |
  | Title: `29` = 2; `31` = the text `[1,2]` | empty | `[1,2]` |
  | Website: `30` only | Only an annotation | empty |

  None of the 936 other `.fmp12` files of the research data (the
  templates of the Mac and Windows installers, the MBS examples, the
  harness fixtures and earlier probe files) has `29`, `30` or `31`.

**FileMaker 26.1.1** stores nothing new here; it reads more keys of the
same JSON object (Claris release notes, FileMaker Pro 26.1.1): `fm_import`
names the target field in the Specify Import Order dialog, `fm_sql` names
the field in SQL results and queries (execute-sql.md 4.3), `fm_odata` in
OData API responses and requests. A JSONNull value of `fm_export` or
`fm_sort` hides the field from the Specify Field Order for Export or the
Sort Records field list, and a JSONNull `fm_common` from both unless the
feature's own key is set; Sort Records, Specify Field Order for Export and
Modify Table View get a context menu to show display names or field
names. [O, DBEngine 26.1.1] `DBField::GetDisplayFieldNameOnly` (0x19f7bee)
looks up the key of its `FieldNameAliasType`, in the order `fm_export`,
`fm_sort`, `fm_table_view`, `fm_sql`, `fm_odata`, `fm_import` (the
`DBField::kAlias_*` constants), and falls back to `fm_common`;
`DBField::IsFieldAliasNull` (0x1a018b0) answers whether that key, else
`fm_common`, holds null. Claris also reserves the `fm_` prefix for its own
keys; other keys are the developer's.

---

## 2. File locale and formats `[2, 0d]`

### 2.1 The record [O]

The value of `[2, 0d]` is the stream `Draco::Locale::Write` produces for
the file's `International` (a `Saved_International` when read back):

| # | size | member (`Saved_International`) | notes |
|---|---|---|---|
| 1 | u32 | `mCountryCode`: the `Locale::Type` | the file's language, catalogs/locale-types.csv (21 English, 25 German, ...) |
| 2 | u8 | — | written 0, skipped on read |
| 3 | u32 | flags, below | |
| 4 | u16 | `mGeneralSeparator` | the list separator (`,` en, `;` de); `Get ( FileLocaleElements )` `Text.Sep` |
| 5 | u16 | `mLeadingSmartQuote` | `“` in every file |
| 6 | u16 | `mTrailingSmartQuote` | `”` |
| 7 | u16 | `mThousandsSep` | `,` en, `.` de, U+00A0 fr and sv |
| 8 | u16 | `mDecimalPoint` | |
| 9 | u16 | `mDateSeparator` | `/` en, `.` de, `-` sv |
| 10 | u16 | `mTimeSeparator` | `:`; `.` in the Italian template |
| 11 | u8 | — | written 2, skipped on read (format version) |
| 12 | u8 | `mDateOrder` | `International::DateOrder`: 0 MDY, 1 DMY, 2 YMD, 3 MYD, 4 DYM, 5 YDM |
| 13 | 4 × u8 | `mDateElement[4]` | the long date format, `International::DateElement` (2.3) |
| 14 | string | `mAMString` | ` AM` (with its leading space), ` 午前`; empty where the 24-hour clock is used |
| 15 | string | `mPMString` | |
| 16 | string | `m24HrString` | a suffix for 24-hour times; empty in every file |
| 17 | string | `mCurrSymbol` | |
| 18, 19 | string | — | `+` and `-`, written as constants, skipped on read |
| 20 | 7 strings | `mDayName` | Sunday first |
| 21 | 7 strings | `mAbbreviatedDayName` | |
| 22 | 12 strings | `mMonthName` | |
| 23 | 12 strings | `mAbbreviatedMonthName` | |
| 24 | 4 strings | `mQuarterName` | |
| 25 | 4 strings | `mAbbreviatedQuarterName` | |
| 26 | 5 strings | `mDateElementSeparator` | before element 1, between 1-2, 2-3, 3-4, after 4 |

Flags (field 3), read from the first two bytes; the reader stores each bit
in a `bool` of `Saved_International`:

| bit | mask | member | meaning | `Get ( FileLocaleElements )` |
|---|---|---|---|---|
| 31 | `0x80000000` | `mHourLeadingZero` | hours padded to two digits | `Time.HMS.0h` |
| 30 | `0x40000000` | `mDayLeadingZero` | day padded | `Date.DateNums.0d` |
| 29 | `0x20000000` | `mMonthLeadingZero` | month padded | `Date.DateNums.0m` |
| 28 | `0x10000000` | `mShowCentury` | four-digit years in short dates | `Date.DateNums.YYyy` |
| 27 | `0x08000000` | `mAMPMFormat` | 12-hour clock | `Time.12h` |
| 25 | `0x02000000` | `mNumLeadingZero` | `0.5`, not `.5`, in display formats | `Num.Lead0` |
| 24 | `0x01000000` | `mMetric` | | `Misc.Metric` |
| 23 | `0x00800000` | `mCurrencyLeading` | symbol before the amount | `Currency.Leading` |
| 22 | `0x00400000` | `mAMPMSuffix` inverted: set means AM/PM **before** the time | | `Time.NightDay.12hSuffix` = bit clear |

Other bits are written 0 and ignored [O code]. The 11 Contacts languages:
en `0x0a800000`, de/fr/sv `0xf3000000`, es/pt `0x73000000`, it/ja
`0x73800000`, nl `0x13800000`, zh `0x11800000`, ko `0x19c00000` (12-hour,
AM/PM before the time, no leading zeros) [O].

### 2.2 How the engine uses it [O code, O run]

The file's `DBFile::mLocale` (DWARF, `DBFile` +0xa20) is this record,
read when the file opens; `DBFileUser::GetLocale` (0xf40c10) returns it
(or the engine default without a file), every `DBData` carries it in
`mLocale`, and every locale-dependent rule of calc-values.md reads it:

| behaviour (calc-values.md) | record members |
|---|---|
| number text, `GetAsNumber`, number literals in `Evaluate` text (2.7, 2.8, 8.9) | decimal point, thousands separator, leading zero |
| date and timestamp text, `GetAsDate` parsing (4.5, 4.6) | date order, date separator, day and month leading zeros, century |
| time text and parsing, AM/PM (4.5, 4.6) | time separator, 12-hour flag, AM/PM strings and position, hour leading zero |
| `DayName`, `MonthName` and the long date | day, month and quarter names, date elements and separators |
| text comparison and sorting in calculations (5.3) | the language (`Locale::Type`) selects the collation |
| list separator shown in formulas | general separator |

Engine runs on the same expressions in different files [O, fms-calc-harness]:
`GetAsText ( Date ( 2 ; 17 ; 2014 ) )` is `2/17/2014` in Contacts en
(no leading zeros), `02/17/2014` in a generated English file whose record
sets the day and month bits, `17.02.2014` de, `17/02/2014` es,
`2014-02-17` sv, `2014/02/17` ja, `2014-2-17` ko, `2014/2/17` zh;
`DayName` is `Montag` in de, `월요일` in ko; `GetAsNumber ( "1.234,5" )` is
1234.5 in de and 1.2345 in en and ja; the literal `1234567.5` in formula
text is `?` in de, es and sv (the decimal point is `,` there). A timestamp
in ko displays as `2014-2-17  오후1:05:09` (two spaces: the date-time
separator and the AM/PM string's own leading space, AM/PM before the
time).

**System formats.** FileMaker Pro's File Options "Data entry" choice is
stored in the document preferences (3.2: `0x400` always use current system
settings, `0x200` ask whenever settings differ, neither: always use the
file's saved settings). `FMFileUser::ResetHasForeignFormat` (libFMEngine
0xa07e80) compares the file's locale with the system locale
(`Locale::Equivalent`) and keeps the answer as file-user state flag
`0x10` [O code]. Where FileMaker Pro substitutes the system locale for
display and data entry when the choice is "system" was not traced in the
client [U]. The effect of the choice on one date field is observed in
FileMaker Pro 26.0.3: on an en-US Mac, a disposable de-DE file with
"Always use current system settings" interpreted typed `2.1.03` as
February 1, 2003, displayed `2/1/2003`, and stored `01.02.2003` after
close. Switching to "Always use file's saved settings" immediately
redisplayed that record as `01.02.2003`; in a second record, the same
typed `2.1.03` became January 2, displayed and stored as `02.01.2003`
[O, issue #534]. FileMaker Server always uses the file's record [O:
engine runs]. The general substitution point in the client and the effect
on calculations are still unknown. A desktop host uses the chosen locale
for date entry and display when the file and system settings differ.

### 2.3 Date elements and the long date [O]

`International::DateElement`: 0 empty, 1 day, 2 short day name, 3 full day
name, 4 month, 5 short month name, 6 full month name, 7 short year, 8 full
year, 9 short quarter, 10 full quarter, 11 Asian quarter, 12 half year,
13 reign year, 14 short reign year, 15 alpha reign year, 16 Asian day name,
17 short Asian day name.

The long date is `sep[0] e[0] sep[1] e[1] sep[2] e[2] sep[3] e[3] sep[4]`,
an empty element ending the format [I from the record shape; the JSON in
2.4 stops at the first empty element, O]. English stores `3,6,1,8` with
separators `"", ", ", " ", ", ", ""` ("Monday, February 17, 2014");
Japanese `8,4,1,8` with `"", "年", "月", "日", ""`; Korean `8,4,1,0`. The
German template stores `3,1,6,3` (day name in the year slot; FileMaker's
own data, reported as is) and 39 German MBS files `3,1,6,13`.

### 2.4 `Get ( FileLocaleElements )` [O, 11 languages + 3 files]

`DBFileUser::GetLocaleElementsAsJSON` (0xf3f980) renders the record as
JSON. Every value below matched the stored record for all 11 Contacts
languages, the Japanese FileMaker Server sample, an MBS file and a
FileMaker-generated file:

| JSON | from |
|---|---|
| `APIVers` | 1 |
| `Currency.Leading`, `Currency.Symbol` | flag 23, `mCurrSymbol` |
| `Date.DMQ.1stDayOfWeek` | derived: 1 when the stored first day name (Sunday) equals English's `Sunday` or Japanese's `日曜日`, else 2 (`International::GetFirstDayOfWeek`); en, ja 1; de, es, fr, it, nl, pt, sv, ko, zh 2 |
| `Date.DMQ.DaysOfWeek.NameList`/`AbbrvList`, `Months.*`, `Quarters.*` | the name lists, Sunday and January first |
| `Date.<order>.ElementArray.NameList` | elements 1, 2, 3, then 0, stopping at the first empty one, as codes: 1 `D#`, 3 `D$`, 4 `M#`, 6 `M$`, 8 `YYYY#`, 13 `ReignY` (other codes not observed) |
| `Date.<order>.ElementArray.SepList` | separators 1 to 4, as many as the elements listed |
| `Date.<order>.MustUseLocalesSep` | `false` in every file (`MustUseDateSeparator`, not stored) |
| `Date.DateNums.0d`, `0m`, `YYyy` | flags 30, 29, 28 |
| `Date.DateOrderID`, `DateOrderName`, `Date.Sep` | date order (0 `MDY`, 1 `DMY`, 2 `YMD`), date separator |
| `LocaleID.IDNum` | the `Locale::Type` |
| `LocaleID.IDStr` | `Locale::GetNonLocalizedName` (`Chinese` for 45) |
| `LocaleID.LID` | `Locale::GetNonLocalizedLanguage` (`Simplified Chinese` for 45) |
| `LocaleID.ISOLangCode` | `Locale::GetISO639_1FromType`: `nl fr de it ja pt es sv zh ko` for 20, 24, 25, 30, 31, 34, 39, 41, 45, 76, `en` for every other type |
| `LocaleID.Name` | the language's display name from string resources: `English`, `German`, `French`, `Swedish`, `Japanese`, `Spanish (Modern)`, `Italian`, `Dutch`, `Portuguese`, `Korean`, `Chinese (Pinyin)` [O values, I source] |
| `Misc.Active` | `true` for the file's locale, `false` for `Get ( SystemLocaleElements )` [O; meaning I] |
| `Misc.Metric` | flag 24 |
| `Num.1000s`, `Num.Decimal`, `Num.Lead0` | thousands separator, decimal point, flag 25 |
| `Text.SQuotLead`, `SQuotTrail`, `Text.Sep` | smart quotes, general separator |
| `Time.12h`, `Time.HMS.0h`, `Time.HMS.24h`, `Time.HMS.Seconds` | flag 27, flag 31, `m24HrString`, `false` (`MustUseSeconds`, not stored) |
| `Time.NightDay.12hSuffix`, `amStr`, `pmStr`, `Time.Sep` | flag 22 clear, AM/PM strings, time separator |

Keys appear in alphabetical order (the order above); the text uses `\r`
line breaks, one tab per nesting level and ` : ` between key and value
[O].
The names, languages and ISO codes of every `Locale::Type` are in
`catalogs/locale-types.csv` (extracted from the libSupport tables).

### 2.5 Corpus [O]

All 746 files carry the record; 11 languages occur (628 German MBS files,
59 English, 6-8 of each template language). Element combinations:
`3,1,6,8` (598), `3,6,1,8` (48), `3,1,6,13` (39), `3,1,6,3` (36),
`8,4,1,0` (12), `8,4,1,8` (6), `1,6,8,1` (6), `8,4,1,3` (1). No file has
trailing bytes.

---

## 3. Document preferences `[2, 18]`

### 3.1 The record [O code, O 746 files]

`DocumentPrefs::Read` reads, in order; every member after the two print
styles is optional and an older record simply ends early (the reader then
keeps the defaults in brackets):

| # | size | `DocumentPrefs` member | meaning |
|---|---|---|---|
| 1 | u32 | `mOptions` | option bits (3.2) [default `0x403`] |
| 2 | u8 | `mFieldViewOrder` | Manage Database field list order (`DBCatalog::OrderingID`) |
| 3 | u8 | `mFieldViewAscending` | |
| 4 | key | `mFirstWndOpenScript` | **OnFirstWindowOpen** script (201) |
| 5 | key | `mLastWndCloseScript` | **OnLastWindowClose** script (202) |
| 6 | key | `mDefaultLayout` | File Options "Switch to layout" (kept when the box is off) |
| 7 | u16 | `mDocPrintType` | `FMDocPrintStyle` [1] |
| 8 | u16 | `mDefRelationsPrintStyle` | `JoinGraphPrintStyle` [0] |
| 9 | u16 | `mThumbnailOptions` | `ThumbnailOptionsFlags` [1] |
| 10 | u32 | `mThumbnailCacheSizeInMB` | [512] |
| 11 | key, key | `mWndOpenScript`, `mWndCloseScript` | **OnWindowOpen** (204), **OnWindowClose** (205) |
| 12 | u16, u16 | `mCustomLayoutWidth`, `mCustomLayoutHeight` | [0, 0] |
| 13 | key | `mFileAVPlayerChangeScript` | **OnFileAVPlayerChange** (206) |
| 14 | u16 | `mIconType` | 100 FileMaker icon, 999 custom icon (`[2, 1818]` holds the images) [100] |
| 15 | u16 | `mScaleFactor` | [1] |
| 16 | key | `mWndTransactionScript` | **OnWindowTransaction** (209) |
| 17 | string | `mWndTransactionField` | OnWindowTransaction's "field name" (XML `scriptParameterFieldName`) |

Script keys are script ids (section 1); an empty key (`00`) is "no
trigger". File triggers have no parameter calculation and no mode flags;
FileMaker's XML writes them with `browseMode="True"`. The file-reference
of a file trigger is always this file. Example (Audit MBS file): `00000443
04 01 02010c 00 020136 0001 0000 0001 00000200 00 020155 0000 0000 00
0064 0001 00 00` = OnFirstWindowOpen script 12, default layout 54,
OnWindowClose script 85.

### 3.2 Option bits [O: FileMaker's XML writers, 143 files]

| bit | meaning (XML `Metadata`) |
|---|---|
| `0x0001` | Text: use Asian language line breaking (`Handle asian`) |
| `0x0002` | Text: use Roman language line breaking (`Handle roman`) |
| `0x0008` | Text: use smart quotes (`Handle smartQuotes`) |
| `0x0040` | Open: switch to layout `mDefaultLayout` (`Defaults` present only then) |
| `0x0200` | Data entry: ask whenever settings are different (`DataEntry Ask`) |
| `0x0400` | Data entry: always use current system settings (`AlwaysUseSystem`); with neither bit, always use the file's saved settings (`AlwaysUseFile`) |
| `0x4000` | hide toolbars (`HideToolbars enable`) |
| `0x8000` | use default fields (`UseDefaultFields enable`) |
| `0x0080` | sharing: "Don't display in Open Remote File dialog" (`HideSharing enable`). A host copies it into the file's `hHideFromPeerToPeer` flag and leaves the file out of `fmapp` file lists (network-protocol.md 5.3) [O code; O anonymous lists: files without the bit exactly matched the returned `fmapp` entries; inventory counts withheld] |
| `0x1000` | WebDirect sharing: hide the file from the WebDirect file list (`WebHideSharing enable`; by name the "Don't display on FileMaker WebDirect home page" option [I]); copied into `kHideFromIWP` and checked for `fmwebdirect` lists [O code] |
| `0x0004`, `0x0010`, `0x0100`, `0x2000` | DWARF `DocumentPrefFlag` names only: `prefBottomline`, `prefVisualSpellingOff`, `prefICHotPlugAction`, `prefNoCatalogMods`; `0x0010` is set in a few files [U] |

The XML element names are from `DynamicTemplateXML_Director::CreateMetadataXML_FileVisibility`
(Mac FMEngine `0x18ce26`), which writes `HideSharing` from `0x80` and
`WebHideSharing` from `0x1000`. All 66 shipped starter files have
`0x00000443`, both bits clear. FileMaker's XML omits the `Text` element
for the default `0x403` combination. The login account, password and
sign-in options are not here: the account catalog's header holds them
(local-files.md 3.1) [O].

### 3.3 Corpus [O]

746 files: OnFirstWindowOpen in 97, OnLastWindowClose 22, OnWindowClose 8,
OnWindowOpen 2, OnWindowTransaction 1 (with field name `AuditJSON`), none
in 642; options `0x0403` 611, `0x0443` 85, `0x040b` 36, `0x8403` 8, others
1-2. The probe file (section 9) sets all six file events, including
OnFileAVPlayerChange.

---

## 4. Relationships `[3, fb]`

### 4.1 Definition `[3, fb, 5, R, 02]` [O code, 364 + 9 relationships]

`DBRelation::Definition::Read`:

| # | size | member |
|---|---|---|
| 1 | key | `mLeftTable`: the left table occurrence |
| 2 | key | `mRightTable`: the right table occurrence |
| 3 | sort spec | `mLeftToRightOrder`: the order of the **right** occurrence's records seen from the left |
| 4 | sort spec | `mRightToLeftOrder`: the order of the **left** occurrence's records seen from the right |
| 5 | u32 | `mOptions` |

`Definition::Options` (each "X to Y" names the direction of travel: the
action happens to Y's records when working from X):

| bit | name | Edit Relationship dialog / XML |
|---|---|---|
| `0x01` | `kCascadeDeleteLeftToRight` | right side "Delete related records in this table when a record is deleted in the other table" (`RightTable cascadeDelete`) |
| `0x02` | `kCascadeDeleteRightToLeft` | left side delete (`LeftTable cascadeDelete`) |
| `0x04`, `0x08` | `kCascadeUpdateLeftToRight`, `RightToLeft` | never set; no UI or XML [O] |
| `0x10` | `kCreateRelatedLeftToRight` | right side "Allow creation of records in this table via this relationship" (`RightTable cascadeCreate`) |
| `0x20` | `kCreateRelatedRightToLeft` | left side creation |
| `0x40` | `kSortRelatedLeftToRight` | right side "Sort records": apply `mLeftToRightOrder` |
| `0x80` | `kSortRelatedRightToLeft` | left side sort: apply `mRightToLeftOrder` |

Each spec is the Sort Records spec of script-steps.md 11.1.1 (`DBSortSpec`);
the design probe's relationship 100 (both sides sorted, a value-list order
on the left, two keys on the right) is reproduced byte for byte from that
encoding [O-run]. A sort spec is kept when its sort bit is off (FileMaker keeps the last
order the dialog had); only the bit decides [O: one corpus relationship
keeps a spec with the bit clear, and FileMaker's XML shows no sort for
it]. Example (fmSyntaxColorizer relationship 19):

```
0403d0001e 0403d0001b                left Palette.Color, right _Palette
0000 0000                            left-to-right order: none
0000 0002                            right-to-left order, 2 entries:
  0403d0001e 02010e 0001 020100 020100 0001 020108 00 00000000   PaletteType by value list 8
  0403d0001e 020102 0001 020100 020100 0001 00     00 00000000   ColorName ascending
000000a2                             sort left, create left, delete left
```

### 4.2 Predicates `[3, fb, 5, R, 03, n]` [O]

One leaf per predicate, `n` = 1, 2, ... in definition order;
`DBJoinPred::Read`: u8 operator, key left field, key right field. The
fields are field ids of the occurrences' base tables. Operators
(`DBJoinPred::Type`), each meaning **left field op right field**:

| code | operator | XML `JoinPredicate type` | corpus | probe |
|---|---|---|---|---|
| 0 | = | `Equal` | 380 | 4 |
| 1 | ≠ | `NotEqual` | 2 | 1 |
| 2 | < | `Less` | 1 | 1 |
| 3 | ≤ | `LessOrEqual` | 0 | 1 |
| 4 | > | `Greater` | 0 | 2 |
| 5 | ≥ | `GreaterOrEqual` | 1 | 2 |
| 6 | × | `CartesianProduct` | 17 | 1 |

A code above 6 fails the read (7 `kInvalidOp` and 8 `kBetweenOp` are
in-memory only). A cartesian predicate still stores the two fields the
dialog showed. All predicates of a relationship must hold (AND).
`[3, fb, 1]` holds only the common catalog index keys (schema-path.md
5.1); `[3, 11, 5, O, fb, <other O>]` and `[3, 11, 1, 08]` list
relationships per occurrence (schema-path.md 5.3).

### 4.3 Related records [O: script engine on the probe]

From a record of occurrence X to occurrence Y over relationship R:

1. If X is R's left occurrence, a related record is one of Y's base table
   where every predicate `X.left op Y.right` holds; if X is the right
   occurrence, the predicates read `Y.left op X.right` (the stored left
   field always belongs to the left occurrence) [I for the reverse
   direction: the probe only travelled left to right].
2. Values compare with the calculation comparison of the field types
   (calc-values.md 6.2); an empty match value never matches, also for ≠
   and the inequalities (a record with an empty N was in no inequality
   result) [O]; a cartesian predicate relates every record (Claris; the
   probe's results are consistent with it) [I].
3. Order: if the direction's sort bit is set (`0x40` from left, `0x80`
   from right), sort by the direction's sort spec with the rules of
   script-steps.md 11.1.3; otherwise creation order (record id) [O]. Over
   several relationships the order is that of the last relationship on
   the path (Claris documentation, [I]).
4. The first related record (`Rel::field`, `GetNthRecord ( …; 1 )`) is the
   first in that order [O].

Probe vectors (records in creation order: `e` N=5 W=1 Pending 1/5/2020,
`a` 1 2 Pending 1/1, `d` 4 3 Done 1/4, `b` 2 1 Done 1/2, an empty-N record
W=1 Pending, `c` 3 2 Pending 1/3; from Tasks to P_x):

| relationship (stored) | from `c` | from `e` |
|---|---|---|
| `N ≥ N` (5), right sorted N descending, Task | `3,2,1` | `5,4,3,2,1` |
| `N < N` (2), unsorted | `5,4` | (none) |
| `N ≠ N` (1) and `Status = Status` and `Due Date > Due Date` (4) | `1` | `1,3` |
| `Task × Task` (6) | `5,1,4,2,3` (List skips the empty N) | |
| `N ≤ N` (3), right sorted Task ascending, blanks last, not maintained | Task `c,d,e` | `e` |
| `N > N` (4) and `W ≥ W` (5) | `1,2` | `2` |

`Count ( P_E::Task )` from `c` is 5 (six records, the one with an empty
Task not counted); `P_B::N` is 3 (first in the sort).

### 4.4 FileMaker's XML importer mirrors inequalities [O]

`FMUpgradeTool --generateDBFile` (and so the XML importer behind it)
stores `LessOrEqual` as code 5, `Greater` as 2, `Less` as 4 and
`GreaterOrEqual` as 3, keeping the fields on their sides; FileMaker's XML
export of the result shows the stored codes with the names of the table
above, and the script engine evaluates the stored codes as the table says
(4.3). So the stored code and the export are authoritative; a tool that
writes relationships from FileMaker XML must not rely on the importer's
direction. (`=`, `≠` and `×` are unaffected.)

---

## 5. Summary fields `[T, 3, 5, F, 09]`

### 5.1 The definition [O code, 38 + 14 fields]

A field whose definition byte 0 is 3 (schema-path.md 5.5) has a 30-byte
`DBField::SummaryDef` at `09`, stored as the raw packed structure
(`GetSummaryDef` copies it; a shorter value is zero-padded; a missing
value reads as Total of nothing):

| offset | size | member | |
|---|---|---|---|
| 0 | 1 | `mSummaryType` | `SummaryDef::SummaryType`, below |
| 1 | 1 | `mFlags` | below |
| 2 | 6 | `mFieldSpec.mTableKey` | `HBKey<5>`: length byte + up to 5 bytes, the rest unused (uninitialized bytes are common) |
| 8 | 6 | `mFieldSpec.mFieldKey` | the summarized field, `02 01 <id>` |
| 14 | 2 | `mFieldSpec.mRepetition` | big-endian, 1 |
| 16 | 6 | `mAuxFieldSpec.mTableKey` | the additional field's occurrence |
| 22 | 6 | `mAuxFieldSpec.mFieldKey` | the additional field, `02 01 00` when none |
| 28 | 2 | `mAuxFieldSpec.mRepetition` | |

The summarized field's table key is empty (this table, `02 01 00`); the
additional field's is the occurrence chosen in the dialog (see 5.4).

| `mSummaryType` | kind | flag `0x01` | XML `operation` |
|---|---|---|---|
| 1 | Total of | | `Total` / with `0x04` `RunningTotal` |
| 2 | Average of | weighted by the additional field | `Average` / `WeightedAverage` |
| 3 | Count of | | `Count` / with `0x04` `RunningCount` |
| 4 | Minimum | | `Minimum` |
| 5 | Maximum | | `Maximum` |
| 6 | Standard Deviation of | by population | `StdDeviation` / `StdDeviationByPopulation` |
| 7 | Fraction of Total of | subtotaled when sorted by the additional field | `Fractional` / `FractionalSubtotal` |
| 8 | (`kSumCountValidNumOnly`) | | never written [O] |
| 9 | List of | | `List` |

| `mFlags` bit | meaning | XML |
|---|---|---|
| `0x01` | the kind's option above (weighted, by population, subtotaled) | operation name |
| `0x02` | summarize repetitions individually (else all together) | `summarizeRepetition` |
| `0x04` | running total / running count | operation name |
| `0x10` | restart the running summary for each sorted group of the additional field | `restartEachGroup` |

The additional field (`AdditionalField` in the XML) is the weight
(weighted average), the break field (fraction subtotaled, restart). The
engine's subtotal mapping (`GetSubTotalFunctionFromSummaryInfo`) ignores
`0x02` and accepts only the combinations above.

### 5.2 Corpus and probe [O]

The corpus has 38 summary fields: Total 12, Count 25, Maximum 1, all flags
0. The probe has one field per row of the tables above (14), all equal to
FileMaker's XML (section 9).

### 5.3 Values [O: script engine; Claris documentation for the rest]

Summaries are computed over the found set. Probe (records as 4.3; N = 5,
1, 4, 2, empty, 3; W = 1, 2, 3, 1, 1, 2), unsorted, on the last record:
Total 15, Average 3, Weighted average 3 (Σ N·W / Σ W over records with an
N: 27 / 9), Count 5 (empty values are not counted), Minimum 1, Maximum 5,
Standard deviation 1.5811388300841897 (sample, n − 1; FileMaker's decimal
square root, calc-values.md 14), by population 1.414213562373095,
Fraction .2 (3 / 15), List `e¶a¶d¶b¶c` (found-set order, empty values
skipped), running total 15, running count 5. Per record (Task, N, running
total with restart by Status, running count, fraction, fraction subtotaled
by Category, `GetSummary ( S_Total ; Category )`, `GetSummary ( S_Total ;
Status )`, `GetSummary ( S_Total ; S_Total )`, `GetSummary ( S_RunTotal ;
Status )`):

```
unsorted (creation order)          sorted by Category, Task              sorted by Status, N
e 5  5 1 .3333 -     -  - 15 -      ∅ ∅  - 0 0     0     3  - 15 -       b 2 2 1 .1333 - - 6 15 6
a 1  6 2 .0667 -     -  - 15 -      a 1  1 1 .0667 .3333 3  - 15 -       d 4 6 2 .2667 - - 6 15 6
d 4 10 3 .2667 -     -  - 15 -      b 2  3 2 .1333 .6667 3  - 15 -       ∅ ∅ - 2 -     - - 9 15 9
b 2 12 4 .1333 -     -  - 15 -      c 3  6 3 .2    .25   12 - 15 -       a 1 1 3 .0667 - - 9 15 9
∅ ∅ 12 4 -     -     -  - 15 -      d 4 10 4 .2667 .3333 12 - 15 -       c 3 4 4 .2    - - 9 15 9
c 3 15 5 .2    -     -  - 15 -      e 5 15 5 .3333 .4167 12 - 15 -       e 5 9 5 .3333 - - 9 15 9
```

(`∅` is the record with no Task and no N, `-` an empty result; fractions
shortened here, the engine returns 16 digits.) Rules this shows:

- A running total accumulates in found-set order; with `0x10` it restarts
  at each change of the break field's value, but only when the found set
  is sorted by that field (sorted by Category it does not restart).
  `GetSummary ( running summary ; break )` is the running value at the end
  of the group.
- A fraction subtotaled by a break field is empty unless the found set is
  sorted by that field; then it is the value over its group's total.
- `GetSummary ( summary ; break )` is the summary over the record's group
  when the found set is sorted by the break field, empty otherwise;
  `GetSummary ( s ; s )` is the grand summary. For a running total or
  count it is the found set's total or count, also with restart; for a
  fraction of total 1, and for a subtotaled one 1 when the found set is
  sorted by the break field, empty otherwise [O, x05 below].
- On a record whose summarized value is empty, the running total is empty
  until something has accumulated in its group, else the value so far; the
  running count shows the count so far; the fraction is empty, except on
  the first record of a sorted found set, where it was 0, and a
  subtotaled fraction also on the first record of its group [O; the
  difference is FileMaker's].
- A summary FileMaker cannot compute is `?`: the standard deviation of a
  group with one value (`GetSummary ( SSD ; B )` in x05; by population it
  is 0). Over a group without values (x05, record 5 alone) the total,
  averages, minimum, maximum, deviations, fractions and list are empty.
- Repetitions summarized individually: a summary field with repetitions
  of its own (FileMaker's XML importer takes them from a `Storage
  maxRepetitions` element on the summary field, as
  `tools/fms-transfer-oracle/probe_schema.py` writes it) holds per
  repetition the summary over that repetition of the summarized field.
  Summarized together it is one value over every repetition [O, x05].

The transfer oracle's battery x05 (`tools/fms-transfer-oracle`, run in the
script harness; script-steps.md, "Implementation notes (import/export)",
Exports) checks these on six other records. Their K, break field B, N and
the 3-repetition NR are: 1 b 5 `1|2|3`; 2 a 1 `4||6`; 3 b 4 `|5|`; 4 a 2
`7|8|9`; 5 c (empty) `||`; 6 c 3 `10||`. Its per-record values, in four
orders (unsorted, by B, by B descending, by K), match the rules above. A
Total of NR summarized individually is 22, 15, 18 on every record, and 55
summarized together. `GetSummary ( s ; s )` of a running total restarting
by B is 15 and of a running count restarting by B 5 (sorted by B, where
the last group alone holds 3 and 1). A fraction subtotaled by B is 0 on
record 5, the empty first record of group c, when sorted by B.

### 5.4 FileMaker's XML importer and the additional field [O]

When the patch gave the additional field with a `BaseTableReference`, the
importer stored the base table's key (`80 02`) as `mAuxFieldSpec`'s table
key, and the engine then ignored the option (no restart, no subtotal);
with a `TableOccurrenceReference` it stored the occurrence and the options
worked. FileMaker Pro's own files have no additional fields in the corpus
[U for what FileMaker Pro writes; the occurrence is the working form].

---

## 6. Value lists `[21]`

### 6.1 Storage [O]

`[21, 1]` is the common catalog index, with two keys of its own [O: the
66 starter files, file-format.md 3.7]: `06`, the name list by type, one
leaf per list keyed by the list's type as two bytes (`00 00` custom,
`00 01` from a field) and then the first two levels of the key its name
has in the name list `01` (schema-path.md 5.6): the primary weights,
`00 00`, and the secondary level where the name has one. That key's
closing `00` and third level are not in it. The leaf holds the list's
key string. And `07`, an empty leaf per member. Members `[21, 5, V]`:

| key | value |
|---|---|
| `02` | the definition (6.2); absent in 110 corpus lists, which are custom lists |
| `03` | custom values: text, values separated by CR; `-` alone is a divider |
| `05` | a value-list calculation (`KItemsCalc`, read by `DBValueList::GetValuesCalculation`); see type 3 |
| `10` | name |
| `d8`, `d9`, `fc`… | as for every catalog member |

### 6.2 Definition [O code, 343 corpus + 4 probe definitions]

`DBValueList::Definition::Read`:

| # | size | member |
|---|---|---|
| 1 | u8 | `mValueListType`: 0 custom values, 1 values from field, 2 external (another file's value list), 3 from calculation |
| 2 | u16 | `mOptionSet` (`DBValueList::Options`) |
| 3 | field spec | `mPrimaryFieldSpec`: occurrence, field, repetition |
| 4 | field spec | `mSecondaryFieldSpec`: "also display values from second field" (`0:0` when none) |
| 5 | key | `mStartTable`: "include only related values starting from" occurrence |
| 6 | key | `mValueListFile`: external list's file reference (type 2) |
| 7 | key | `mValueListKey`: external list's id in that file (type 2) |
| 8 | string | `mSeparator` (a space in every file) |
| 9 | sort spec, optional | `mSortSpec`: present only for "re-sort values based on <language>": one entry with empty field specs and the language in `mLocaleOverride` (corpus: 21 English, 25 German) |

| option | name | XML |
|---|---|---|
| `0x1` | `kShowRelatedValues` | `ShowRelated value="True"` + the start occurrence |
| `0x2` | `kSortBySecondaryField` | `PrimaryField sort="False"`, `SecondaryField sort="True"` |
| `0x4` | `kShowSecondFieldOnly` | `PrimaryField show="False"`, `SecondaryField show="True"` |
| `0x8` | `kShadowValueList` | never set [O] |

Type 2 (`External`) and type 3 (`FromCalculation`) do not occur in the
corpus; type 3 is enabled only behind the debugger bit `0x7f`
(`IsCalculationBased` tests `Debug::HasBit(gDebugger, 0x7f)`) in 26.0.3
[O code], so files of 26.0 have types 0-2. FileMaker Pro 26.1.1 ships it
(6.6): its `IsCalculationBased` (DBEngine 0x1c2b9f4) is only "type is 3",
and `Definition::Read` and `Write` (0x1c2bf9e, 0x1c2c0c0) are unchanged,
so a calculated list is stored as 26.0.3 already defined it [O code].

### 6.3 Corpus [O]

453 value lists in 746 files: custom 269 (159 with a definition, 110
without), from field 184 (options 0 301, `0x1` 15, `0x2` 1, `0x6` 16, `0x7`
10 across all types). A custom-values text can be a chunked value whose
header is 12 bytes (`bytes, characters, 4`); `om-hbam`'s
`Node::assembled` accepts only 4- and 8-byte headers (1 corpus list; the
same 12-byte form is common in record data) [O].

### 6.4 `ValueListItems` [O: script engine]

- Custom values: the lines in order, dividers included
  (`Alpha¶-¶Beta¶Gamma`).
- From a field: the first field's values, unique, sorted by the first
  field (index order); numbers in numeric order (`1¶2¶3¶4¶5`).
- With a second field and "sort by second field": the first field's
  values, one per distinct second-field value, the first record in source
  order winning, ordered by the second field (`d¶e` for Status
  Done/Pending over all records).
- With "include only related values starting from" X: the source is the
  related set from the current record of X, in the relationship's order
  (`b¶c` where the related set was c, b, a sorted N descending).
- `ValueListIDs` lists the catalog's display order (section 1).
- A field's values may come from an unstored calculation. FileMaker Pro's
  value list dialog offers such fields, and a related-values list over one
  is in production use [O: a hosted solution's definition, relayed
  sanitized, #1035, #1058]. Such a field has no index, so the list holds
  its value on each record of the source as evaluated [I]. A text value
  gives one item per line, as a stored field's index does [I].

In FileMaker Pro 26.0.3, a field-backed drop-down list did not include a
new value still in the current record's draft [O: local Contacts copy,
[#148](https://github.com/OpenMaker-Dev/openmaker/issues/148)]. Two objects of the same field displayed the typed text, but the
second object's list held only the three committed values. "Revert Record"
was still available and closing asked whether to save the record. After
saving and reopening, the list included the new value [O]. That the list
reads the committed field index, rather than the draft, is consistent with
this result but was not checked by reading the index [I]. Pop-up menu,
checkbox set and radio button set controls also omitted distinct values
typed into a new, unsaved record while showing the committed choices [O:
three disposable copies of the same local Contacts file, [#148](https://github.com/OpenMaker-Dev/openmaker/issues/148)].

A related list behaved differently in a local Tasks copy [O: FileMaker Pro
26.0.3, [#148](https://github.com/OpenMaker-Dev/openmaker/issues/148)]. Its pop-up drew from a self-related occurrence, with "include
only related values starting from Tasks" enabled and automatic record
saving off. With three saved records, the current record's saved numeric
relationship key 2 offered `Completed, Pending`; changing that key to 3
without saving also offered `In Progress`. After discarding that change,
editing the current record's `Status` from `Completed` to `DraftOnly`
without saving replaced `Completed` with `DraftOnly` in the list. The
current record was itself in the related source set. Pro later asked
whether to save each edit when navigating away, and discarding it restored
the saved key or status. Thus the related filter used the draft key, and
the related choices reflected a draft edit to an existing source record
in this self-relationship [O]. Other relationship types, hosted sessions
and refresh after another user's edit remain unmeasured [U].

### 6.5 Writing custom values: Edit Value List [O: before/after twin, #869]

FileMaker Pro 26.0.3, a local copy of the English Content Management
starter with one record. On Content Details, Type > Edit… (Edit Value
List "Type") added `OM869 New Value` and a `-` line after the list's last
`-` line; OK. The closed copies before and after that one edit differ in
`[21]` only here:

| key | before | after |
|---|---|---|
| `[21, 5, 1, 03]` | `Photo¶Image¶PDF¶Audio¶Video¶-¶Office¶Web¶-` (42 bytes) | the same, then `¶OM869 New Value¶-` (60 bytes) |
| `[21, 5, 1, fc]` | modification count `01 01` | `01 02` |
| `[21, 5, 1, fc01]`, `fc03` | the modifying user and time | the editing user and time |
| `[21, 1, fc]` | the catalog index's count `01 02` | `01 03` |
| `[21, 1, fc01]`, `fc03` | user and time | the editing user and time |

- **Kept byte for byte:** the definition `02`, the name `10`, the UUIDs
  `d8` and `d9`, and every other entry of the index (`00`, `01`, `03`,
  `04`, `06`, `07`, `20`, `d8`). Each stamp's account `fc02` was `Admin`
  before and after.
- **The values** are the editor's lines joined by CR, `-` lines included,
  in the same SCSU text form as before (6.1).
- **Outside `[21]`:** two records of the file's node `[2, 8]` and `[2, 9]`
  differ by one counter, `01` → `02`. The after copy was opened once more
  than the before copy; that the counter counts opens, not the edit, is
  inferred.
- **Cancel** in the dialog left the list as it was (the reopened menu);
  the bytes of a cancelled session were not compared.
- **The record:** choosing Edit… committed the record first, a new
  uncommitted record included; Cancel did not undo that commit.

OpenMaker writes the same change (`om_data::value_lists::plan_values`,
authoring operation 24): the values `03`, and the host's stamps on the
member and on the index (schema-path.md 8.1). A file written so, its Type
list given one more value and divider, differs from the starter in `[21]`
in exactly the rows above.

**Unknown.** Lists without a definition, values too long for one stored
piece (a chunked value, 6.3), non-ASCII values, an empty list (OpenMaker
removes `03`), and what FileMaker Server or a hosted file does. Assets'
Type drop-down list stores the Edit flag (`0x8`) but FileMaker Pro 26.0.3
showed no Edit… in it, empty or holding values (#869); its list is one of
a field's values.

---

### 6.6 Calculated value lists (FileMaker 26.1.1)

Edit Value List's "Use calculated value list" (Claris release notes,
FileMaker Pro 26.1.1) is definition type 3 (6.2) with the formula at
`[21, 5, V, 05]` (`KItemsCalc`, read by `DBValueList::GetValuesCalculation`)
[O code; no 26.1.1-made list read yet]. The formula's result is either:

- text: each return-separated value is one item; or
- JSON with `ValueList.values`, an array of objects in display order, each
  with `display` and at most one second key (Claris): none (shows and
  stores `display`), `displaySecond` (shows both, stores `display`),
  `enterSecond` (shows `display`, stores the second value) or
  `displayAndEnterSecond` (shows both, stores the second value). The
  elements should use the same second key; when they differ, the last
  element that has one decides for the whole list.

[O, DBEngine 26.1.1] The key names are new strings; the JSON reader is
`DBCalculatedValueListData::GenerateFromCalc_json` (0x1d94576, larger than
26.0.3's), lookups go through the new `DBFromCalcValueListIter`, and
FMEngine's `GetCalcValueListDisplayText` (0x5fbb9e) builds the text a
field shows. A FileMaker 26.0 client takes a type 3 list as not
calculated (its gate) [O code]; what it then shows is [U].

Unknowns: the context (occurrence and record) the formula is evaluated in;
when FileMaker re-evaluates it; what an empty, invalid or mixed result
shows; how Save as XML writes a type 3 list (FMEngine's
`ObjectTransferValueLists` handles types 0-2 only [O code]).

## 7. Layout and object script triggers

### 7.1 Where [O]

A trigger cache (`ScriptTriggerCache::Read`) is a `KScriptTriggers` (`30`)
node under its owner:

| owner | node |
|---|---|
| layout L | `[4, 5, L, 30]` |
| object O of layout L | `[4, 5, L, 5, O, 30]` (`LayoutObject::LoadScriptTriggerCache`: the object's key path + `30`) |
| object O of layout L's table view | `[4, 5, L, 0d, 5, O, 30]` |

`[4, 5, L, 0d]` is the layout's separate table-view layout
(`FMLayout::HasTableLayout`, `GrabLayoutPos(…, table)`), with its own
object list; FileMaker's XML does not export its triggers (26 corpus
records) [O].

Cache: `04` = u16 count of triggers, `05, <event>` = one member per event,
the member key being the event id as u16 (`00 65` = 101). An empty cache
(count 0, no members) is common (106 corpus caches).

### 7.2 The trigger record `05, <event>, 01` [O code, 403 corpus + 18 probe records]

`ScriptTrigger::Read`:

| # | size | member | |
|---|---|---|---|
| 1 | u16 | `mTriggerID` | event id (script-runtime.md 7.1); a stored 108 is read as 110 (the old view-change id) |
| 2 | key | `mScriptKey` | the script id (section 1) |
| 3 | key | `mFileRefKey` | `00` = this file; else a file reference id (`[2, …]` file references catalog) |
| 4 | u32 | `mFlags` | modes: `0x1` Browse, `0x2` Find, `0x4` Preview (`kEnableBrowseMode`…) |
| 5 | u32 | `mOption1` | 0 in every record |
| 6 | u16 + bytes | `mExtraBytesCount`, `mExtraBytesPtr` | 0 in every record; the reader refuses 1000 or more |

The record is variable length (keys are length-prefixed): the common
16-byte record is `0065 020103 00 00000001 00000000 0000` (OnLayoutEnter,
script 3, this file, Browse); a script id of 128 or more makes it 17 bytes.

The parameter calculation is a formula node at `05, <event>, 31`
(`KParameter`) with the children `04` (context occurrence), `05`
(formatted) and `06` (Polish) of calc-encoding.md 1, read with
`DBCalcFormula::Get` when the trigger is loaded for running; it is absent
when there is no parameter [O: 11 corpus and 3 probe parameters, all
matching FileMaker's XML `Calculation`].

### 7.3 Events seen [O]

Corpus (746 files, 403 records): 1 OnObjectEnter 14, 2 OnObjectExit 16, 3 OnObjectModify
112, 4 OnObjectSave 42, 5 OnObjectKeystroke 17, 6 OnObjectValidate 10,
7 OnPanelSwitch 1, 101 OnLayoutEnter 123, 103 OnRecordLoad 40,
104 OnRecordCommit 5, 105 OnRecordRevert 1, 106 OnModeEnter 3,
107 OnModeExit 1, 109 OnLayoutExit 16, 111 OnLayoutSizeChange 2; modes 1
(355), 3 (44), 7 (4). The probe adds 8 OnObjectAVPlayerChange, 102
OnLayoutKeystroke, 110 OnViewChange, 112 OnGestureTap and 113
OnExternalCommandReceived, every one stored with the same record (FileMaker
stored every probe trigger as given, including the object event 8 on an
edit box; which events fire for which owner is script-runtime.md 7).

### 7.4 Correction to the desktop client's notes

script-runtime.md "Implementation notes (desktop client)" read the record
as fixed offsets (modes as a u16 at bytes 6-7, file reference after them)
and the script key as a raw big-endian key; the record is as 7.2 (the
modes are a u32 after the length-prefixed file reference, the script key
is a serial id), the parameter is at `31`, and the file triggers are in
the document preferences (section 3).

---

## 8. For implementers (summary)

1. Read ids as serial keys everywhere (section 1).
2. Read `[2, 0d]` at open and evaluate every calculation with it (section
   2); `Get ( FileLocaleElements )` is a pure function of the record plus
   the derived values of 2.4.
3. Read `[2, 18]` for the default layout (only if `0x40`), the six file
   triggers and the OnWindowTransaction field name (section 3).
4. Relationships: predicates `left op right`, AND, empty never matches
   (cartesian excepted), order by the direction's sort spec when its bit is
   set, else creation order (section 4).
5. Summaries: the 30-byte definition and the rules of 5.3; the break field
   comes from the additional field and only acts when the found set is
   sorted by it.
6. Value lists: section 6.
7. Triggers: the cache, record and parameter of 7; file triggers from 3.
8. A window opening on a layout starts on its occurrence's saved found
   set, sort and current record (section 11).

---

## 9. Verification

Tools (read-only):

- `crates/om-hbam/examples/file_metadata.rs`: decodes sections 1-7 of
  local files (TSV rows, or `--summary` counts).
- `tools/file_metadata_xml.py`: compares that output with FileMaker's
  `--saveAsXML` exports record by record.
- `tools/file_metadata_probe.py`: writes the probe FMSaveAsXML document
  (Tasks template plus the structures the corpus lacks) for
  `FMUpgradeTool --generateDBFile`.
- `tools/file_metadata_locale_json.py`: rebuilds `Get ( FileLocaleElements )`
  from a decoded locale record (2.4) and compares it with an engine run.
- `crates/om-hbam/examples/dump_keys.rs`: a key-tree dump of any subtree.

```sh
cargo run --release -p om-hbam --example file_metadata -- <files> > decoded.tsv
python3 -I tools/file_metadata_xml.py decoded.tsv <xml dir>
```

Results (2026-10-29): 3,068 checks equal and none different on the 143
exported corpus files, 294 equal and none different on the two probe
files; per check:

| check | records equal / compared |
|---|---|
| decode without error | 746 / 746 files (66 templates, 673 MBS examples, 6 FileMaker Server samples, test project) |
| catalog ids and names (tables, occurrences, relationships, layouts, scripts, value lists, custom functions, accounts, fields) | all, in 143 exported files (incl. 91 script and 18 field ids ≥ 128) |
| relationships: occurrences, options, predicates | 171 / 171; sorted sides 6 / 6 (+ probe 18 / 18, sorts 6 / 6) |
| summary kind, flags, fields | 3 / 3 (+ probe 28 / 28) |
| value lists: source, custom values, fields, show/sort, related, language | 221 / 221 (+ probe 18 / 18) |
| file triggers, default layout, use default fields | 143 / 143 files (+ probe 2 / 2, all six events) |
| layout and object triggers (event, script, modes, parameter) | 71 layout and 20 object caches (+ probe 7 and 1) |
| `Get ( FileLocaleElements )` rebuilt from the record vs the engine | 14 / 14 files, every field but `Name` and `Active` (11 Contacts languages, ja FMS sample, an MBS file, a generated file) |

Scratch (git-ignored, `re/work/filemeta/`): `fm_all.tsv` (corpus decode),
`fm_xmlset.tsv`, `harness/out/` (locale runs), `probe/` (probe XML, the
generated files, their exports, `sem.txt` and `work/sem4.out.tsv`, the
script-engine runs of 4.3, 5.3 and 6.4).

## 10. Unknowns

- U1. Where FileMaker Pro substitutes the system formats when Data entry
  is "always use current system settings" (`0x400`), whether that changes
  `Get ( FileLocaleElements )` or `GetAsText ( Date ( 2 ; 17 ; 2014 ) )`,
  and what "ask whenever settings are different" does. The date-entry
  effect of the two "always" choices is observed in 2.2.
- U2. What DocumentPrefs bits `0x0004`, `0x0010`, `0x0100` and `0x2000`
  do (only their DWARF names are known, 3.2); `Misc.Active` in the
  locale JSON; `LocaleID.Name`'s string resources for every language.
- U3. JSON element codes for date elements 2, 5, 7 and 9-17 other than 13
  (no file uses them).
- U4. What FileMaker Pro (not the XML importer) stores as the additional
  field's table key in a summary definition (5.4), and where a summary
  field's own repetition count is stored (5.3 has it only through the XML
  importer).
- U5. Related-record order and predicates when travelling right to left,
  and over multi-hop paths (4.3 step 1 and 3 are inference).
- U6. External value lists (type 2): the file reference catalog and how
  the other file's list is resolved.
- U7. `kCascadeUpdate*` bits (never set) and `kSumCountValidNumOnly`
  (never written).
- U8. What FileMaker Pro's Advanced Options dialog (not the XML importer)
  writes for an annotation and display names (1.2), and the display-names
  formula's answer inside a portal row or a related field's context.
- U9. Saved found sets (11): what `f0` is; what FileMaker Pro does with a
  saved found set whose `fe` is not the table's data version any more;
  what exactly makes it write the state, and which window's state it keeps
  when several windows show one occurrence (one sequence observed); the
  full form of `08` (sort keys and the buckets of a multi-field sort);
  whether a file hosted by FileMaker Server keeps or applies it. For the
  last find, criteria with ranges, operators other than the wildcard, and
  related fields remain unmeasured.
- U10. Whether the layout a window was on is saved anywhere for a file
  whose "Switch to layout" (3.2) is off (the starters all have it on).
- U11. Four data sources used by occurrences of a hosted multi-file
  solution have neither a readable name `10` nor path list `82` in
  OpenMaker ([#233](https://github.com/OpenMaker-Dev/openmaker/issues/233)). The current host's complete `[32]` replies lack the
  four source members themselves, and the occurrence source keys decode
  cleanly (1.1). Three further occurrences reference raw base-table key
  `80 01` (serial id 129) absent from the respective related files'
  complete host-served base-table catalogs, which contain only `80 02`
  (serial id 130). These are observations of replies to one authenticated
  account, not a comparison with current native Pro UI or on-disk files.
  Whether privilege filtering removed members, whether older or ODBC
  storage explains their absence, and how Pro displays the current
  references remain unknown. An offline extraction of those files' exact
  catalogs and a controlled native Pro comparison would distinguish
  these causes. OpenMaker's alert describes the absent members and
  tables as seen in its downloaded catalogs.

  Local synthetic checks [O, FileMaker Pro 26.0.3, [#233](https://github.com/OpenMaker-Dev/openmaker/issues/233)]: in separate
  copies of a two-file pair, deleting only a source's `10` name leaf,
  only its `82` path-list leaf, or both changed no other decoded key-tree
  leaf. Each file opened on a layout that did not use the related file
  without an alert. Manage External Data Sources kept the source row:
  without `10` its Name cell and edit field were blank, while its file
  path remained visible; without `82` its name remained visible, Details
  showed an empty File Path, and the edit dialog showed no path or
  selected source type. With both leaves absent, the row had a blank
  name and path. When Pro opened Manage Database, a present path led to
  the related file's sign-in dialog; without `82`, Pro asked the user to
  locate the file. After Cancel, the relationships graph kept the
  occurrence by its own name and showed `<Table Missing>`. These cases
  establish Pro's response to missing leaves, not which cause applies to
  the hosted solution.
- U12. Whether a draft value is omitted in pop-up menus, checkbox and
  radio sets, or lists limited to related values. The local check in 6.4
  covered only a field-backed drop-down list without a related-value filter.

## 11. Saved found sets `[TO, 07]` [O: native FileMaker Pro saves, 6 starters; meanings partly inferred]

When FileMaker Pro closes a local file it keeps, under each table
occurrence's key (`[TO]`, the occurrence id's segment, e.g. `d0 00 02`),
the found set its window had there, and opens the file on it again: Task
Details reopens at found 2 of 3 on the record that was current, and a
second table's layout, when the window first goes there, at its own found
set, sort and record [O: walkthrough on [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), FileMaker Pro 26.0.3].

`[TO, 07, 01]`:

| key | value | meaning |
|---|---|---|
| `02` | u8 | 1 when the found set is sorted by `0a`, 0 when it is not but a sort is kept; absent, with `0a` and `f0`, when the table was never sorted [O, 3 sorted and 1 unsorted entry with a sort, 2 never sorted] |
| `03` | a key `[u8 n][n bytes]`: the record id's serial key | the current record; `01 00` (id 0) none [O, 3 cases match the record Pro showed] |
| `05` | a RID list (query-program.md 5.1): bitmap words or singletons | the found set, when not every record [O] |
| `08` | `00`: u64 count; `01, <sort key>`: the records (a leaf per record, or a RID list's bitmap words as in `05`), or for a multi-field sort a bucket `01 n` whose `08, n` holds the next field's keys | the sorted order: the sort fields' index keys (the bytes of their value index entries, `[T, 01, F, 01, …]`) in key order [O: two ascending one-field sorts, one two-field descending sort; the bitmap form in a Tasks copy FileMaker Pro 26.0.3 closed sorted, records 1-3 under one key as the word `0000000e` (#642); the key forms inferred] |
| `0a` | `DBSortSpec::Write` (script-steps.md 11.1.1) | the last sort, also kept when `02` is 0 [O] |
| `f0` | u32 | 0 in every case, present only beside a sort spec `0a`; unknown |
| `fd` | none: the key alone, no value and no children | present only beside a sort spec `0a`, in sorted entries (`02` = 1) and in unsorted ones that keep a sort (`02` = 0); unknown [O: in FileMaker-written sample files (Claris's FileMaker Server sample and third-party examples, versions not known), 11 of 12 sorted entries and 4 of 9 kept-sort entries; the Tasks saves below list no `fd`] |
| other members | | `fc` (`01 n`, a key's form), `09`, and `fe` naming two or three tables occur in some of those files' entries, unsorted ones too; unknown [O, same files] |
| `fe, <table key>` | u32 | the table's data version `[3, 16, 5, T, fb]` (schema-path.md 5.8) when the state was saved [O: equal in all 7 entries of 5 saves and the 6 starters, unrelated to record ids; schema-path.md 5.8's E3 raised both together] |
| `ff` | empty leaf | every record of the table is found; `05` is then empty [inferred: present for every Show All case, absent for every subset] |

`[TO, 07, 00]` is `01 02`; `[TO, 07, 02]` holds the last find's requests.
On a disposable three-record Tasks copy, Pro saved a two-request find:
request `01` included `Task = Alpha*`, and request `02` omitted
`Task = Alpha Cedar`. The `06, 00` leaf was `01 02`; each request's
`06, <request>, <field spec>` leaf held its criterion as XOR text.
Request `01` had `06, 01, fc` = `01 01`; request `02` had an empty
`06, 02, 00` leaf and no `fc`. After close and reopen, Pro restored the
one-of-three found set, and Modify Last Find showed both criteria and
their Include/Omit settings [O, [#247](https://github.com/OpenMaker-Dev/openmaker/issues/247)].

In a second save, changing only request `02` from Omit to Include left
its criterion leaf unchanged, removed its empty `00` child, and wrote
`06, 02, fc` = `01 01`. This identifies the empty `00` child as Omit
and `fc` = `01 01` as Include in these Pro-authored requests [O, [#247](https://github.com/OpenMaker-Dev/openmaker/issues/247)].
The second save also committed a disposable record to ensure the file
wrote its window state after the value index was already present; this
does not establish that a find alone causes a save.

Evidence: two copies of one native-only Tasks file, closed by FileMaker Pro
26.0.3, the first after finding one of two Assignees (sorted by Initial,
`[d0 00 04, 07, 01]`: `02` = 1, `03` = record 2, `05` = {2}, `0a` field
`0a` ascending), the second after Show All on Tasks only. They differ in
`[d0 00 04, 07, 01]`, in the index entries and field flags the Assignee
find built, and in one step of `[2, 08]`/`[2, 09]` (the file's history),
and nowhere else: the second session's Show All was not written, and its
Tasks entry (`05` = {1, 3} as the bitmap word `0x0a`, `03` = record 3) is
the one both copies started from. Each of the six English starters ships
with one entry, for its main occurrence: `ff`, `03` = `01 00`, `05` empty,
`fe` = 1, no `0a`.

Three further Tasks copies, each with records 1-3 made in FileMaker Pro
26.0.3 and closed by it [O]:
- edited, then sorted by Task and Description, both descending: every
  record (`ff`), `02` = 1, `03` = record 3, a two-entry `0a`, `08` with
  three buckets; it reopened sorted on record 3;
- found Alpha (records 1 and 3), a record 4 created and committed: `05` =
  {1, 3, 4} (the word `0x1a`), `03` = record 4, no `02`, `0a` or `f0`; it
  reopened at 3 of 4 on record 4. A record made in the window joins the
  saved found set;
- two windows, the first found Alpha on record 3, the second found Beta on
  record 2 and edited it, the second closed first and the first last
  without an edit: `05` = {2}, `03` = record 2, the second window's state.

When it is written [O: FileMaker Pro 26.0.3, local Tasks copies, #642's
table, 2026-11-14]:
- **Written:** a first Quick Find `Alpha` (2 of 3) on a fresh copy,
  closed by the window's close button or by File > Close, reopened at 2 of
  3 on the same record; an edit and then that find, closed by the close
  button, reopened at 2 of 3 with the edited value.
- **Not written:** three byte-identical copies that already stored 2 of 3
  and had their indexes, in sessions that only changed the found set:
  Show All (3 of 3) closed by the close button, Quick Find `Beta` (1 of 3)
  closed by the close button, and Show All closed by File > Close. Each
  file's SHA-256 was the same before opening and after closing, and each
  reopened at the old 2 of 3.

So a session that only changes the found set writes nothing, by either
close route. What makes the first Quick Find's session write, the indexes
that find built or something else, is unmeasured (U9); OpenMaker takes it
to be the index build. That a window which changed nothing leaves the state
another window wrote is the two-window copy above.

Each occurrence has its own entry [O: keyed by occurrence], so two layouts
of one occurrence share it; that two occurrences of one table keep separate
found sets is inferred from the keying, not observed.

### 11.1 What a write of an unsorted found set holds

What FileMaker Pro wrote for a window that closed on an unsorted found set
(the two-window and find-new copies, and the found2 copy's Tasks entry
with a sort kept), key by key; the rows marked [inferred] are values the
copies show without the rule behind them being observed:

| key | value written | evidence |
|---|---|---|
| `fe, <table key>` | the table's data version `[3, 16, 5, T, fb]` when the state is written | [O: equal in all 7 entries; not derived from record ids or counts] |
| `03` | the current record's id as a key, `01 00` for none | [O: 5 entries] |
| `05` | the found set as a RID list, in FileMaker Server's run form (query-program.md 5.1): `{2}` the singleton `02`, `{1, 3, 4}` the word `0x1a` at `00` | [O: 3 entries; the run rule beyond one word, inferred] |
| `ff` | an empty leaf, with `05` an empty list, when every record is found | [O: the starters and every Show All entry] |
| `02` | `00` when a sort `0a` is kept, absent when the table was never sorted | [O: found2's Tasks entry and the two never-sorted copies] |
| `0a`, `f0` | the last sort and its `f0`, as they were, absent when never sorted | [O: the same entries; that FileMaker Pro keeps the old spec unchanged is inferred] |
| `08` | absent when not sorted | [O: every unsorted entry] |
| `[TO, 07, 00]` | `01 02` | [O: every entry] |
| `[TO, 07, 02]` | the last find, as it was; with none, `03` = `01 00` and an empty `05` | [O: every entry; its contents are not interpreted] |

A sorted found set's `02` = 1, `08` (sort keys) and new `0a` are not
covered: the form of `08` beyond these cases is U9.

---

## Implementation notes (2026-10-29)

Decoders, one for every host:

- `om_calc::Locale::from_record` / `to_record` read and write `[2, 0d]`
  (the records of all 745 local corpus files re-encode byte for byte) and
  `Locale::elements_json` is `Get ( FileLocaleElements )` (and, with
  `Misc.Active` false, `Get ( SystemLocaleElements )`), equal to FileMaker
  Server's own text for the en, de, ja and ko Contacts templates. The
  decoder lives in om-calc (with om-scsu for the strings) so om-data,
  OpenMaker Server and om-sql share it without depending on each other; the
  `Locale` constructors are the shipped Contacts records decoded. Every
  `Locale::Type` of `catalogs/locale-types.csv` decodes (`Language::Other`
  for the ones without a variant); `LocaleID` names come from the catalog;
  `Name` is the 11 observed names, else the language (U2). Long-date codes
  other than the six observed show as empty in `NameList` (U3); stored date
  orders 3-5 read as the order with the same first element. Korean times
  show the marker before the time; parsing that form is not supported.
  Calculations still collate a language without its own `Language` variant
  as English (`Collation::of`, calc-values.md 13); sorts and value lists use
  the language's named collation (below).
- `om_data::meta`: `FileOptions` (3), `Relationship` with both orders and
  `order_of` (4), `SummaryDef` (5), `ValueList` (6), `display_order`
  (`[cat, 1, 03]`) and `scripts` (section 1). Catalog ids stay the keys'
  big-endian reading inside the workspace (tables, fields, occurrences,
  layouts, value lists, as every catalog reader numbers them);
  `om_hbam::serial::id_of_raw` and `raw_of_id` convert to and from the ids
  FileMaker shows, and the Design functions and ExecuteSQL's system tables
  show the converted ids. Script ids are the serial decode throughout.
- `om_layout::triggers`: the record of 7.2 with `encode_record` (all 402
  corpus records re-encode), the parameter only at `31`; table-view caches
  are decoded into `Layout::table_view_triggers`. `om_scripts::specs::
  SortSpec::read` reads a sort spec inside a longer stream (4.1).
- Fixed: `Node::assembled` takes 4-, 8- and 12-byte chunk headers.
  `Script::id` is the serial decode and `Script::encode` writes the serial
  key; script references (Perform Script, Install OnTimer Script, a
  button's step, the Script Workspace's target) read through
  `StoredKey::serial_id` (`ScriptStep::script_id`, `target_script_id`) and
  are written with `StoredKey::from_serial_id`; trigger records and the
  document preferences' file triggers decode their script keys the same
  way; Perform Script on Server's op30 key is the serial key on both sides
  (`om_protocol::script::script_key`/`script_id`). Ids below 128 are
  unchanged; all 3,635 script definitions of the 745 local corpus files
  still re-encode exactly.
- `crates/om-data/tests/metadata_corpus.rs` (ignored; `OM_METADATA_CORPUS`
  adds the MBS examples) decodes every structure of the templates, the FMS
  samples and the 673 MBS files: 361 relationships (17 sorted sides), 38
  summaries, 447 value lists, 340 layout and object and 62 table-view
  trigger records (10 parameters), 130 file triggers, 91 script ids from
  128 on (the test project, absent here, holds the rest of section 9's
  counts).

Hosts:

- `rules::schema::calc_schema` builds the locale, file options,
  relationships with their orders, summaries, value lists, scripts, display
  orders, field comments, options and index language, and the layouts'
  field objects (field, control style, value list, repetitions shown). The
  desktop provider also downloads `[2, 0d]`, `[2, 18]`, `[17]` and `[21]`.
  OpenMaker Server's own decoders are gone: its store reads `[2, 0d]` with
  `Locale::from_record` (en-US with the index-detected date order when
  missing), a sorted relationship's order is the calculation schema's
  (`Hop::sort`), Sort Records by a custom value list reads the schema's
  `ValueList` (the old reader looked under key `21` decimal, so value-list
  orders never applied), and server scripts answer
  `Get ( FileLocaleElements )`. ExecuteSQL's `SqlCatalog::read` takes its
  value lists from `meta::value_lists` (custom lines without empty ones);
  om-sql has `FileHost::open_file` with the file's own locale.
- Related records: 4.3 with the last hop's order. The shared sort
  (`om_data::host::sort`) follows the rules OpenMaker Server's Sort Records
  was built on (script-steps.md 11.1.3): empty first ascending and last
  descending or last with blanks last; with a custom value list its values
  in order, then other values, then empty ones (S11); text by the entry's
  language override, else the field's index language, else the file's,
  each through its named collation; ties to the next field, then the
  record id. A sort field of an occurrence of another table sorts by its
  first related record's value. Stored values only: an unstored
  calculation or a summary as a sort field sorts as empty; a value list
  from a field orders by value.
- Summaries: every kind and option of 5.1 with the rules of 5.3, in the
  desktop's record pages and scripts. The found set's sort comes from
  the window (scripts) or the provider's found set (record pages); when
  unknown it reads as unsorted. A group is a run of records equal in every
  sort field up to the break field [I: the probe only broke on the first
  sort field]; `GetSummary` of a fraction is the group's share of the
  total [I]. Every host computes them with `om_data::summary` (the
  desktop's calculation host and ExecuteSQL since 2026-10-29, server
  scripts before), with summarized values read as calculations read them
  (an unstored calculation is computed).
- Value lists follow 6.4; values are unique by `=` and ordered by the
  field's index language or the list's re-sort language. The related-values
  vector `b¶c` of 6.4 is not reproduced here (its list definition is not
  recorded); external lists answer empty, calculation lists 1225.
- Design functions from the schema, as FileMaker Server answers them
  (verified 2026-10-29, calc-functions.md 5.8): every list, field, layout
  field object, relationship and value list function, `LayoutObjectNames`
  and `GetLayoutObjectAttribute`, through the desktop host and server
  scripts. Superseded by that section: the display order (1.1), field ids
  and names on layouts, `RelationInfo`'s format, `FieldStyle`'s
  `Scrolling`, `FieldRepetitions` (no longer 1225).
- Desktop triggers: layout and object triggers come from the corrected
  record (serial script ids, modes after the keys, parameter at `31`).
  OnFirstWindowOpen then OnWindowOpen fire once the first layout has
  loaded, before its OnLayoutEnter and OnRecordLoad, each waiting for the
  one before [I: the order]; OnWindowClose then OnLastWindowClose hold the
  user's Close File and cancel it on a false result. File triggers run in
  every mode. A script's Close File does not fire them. OnWindowTransaction
  fires after every commit that writes records, with its field-name option
  (script-runtime.md 13.26 and the desktop notes, 2026-10-29);
  OnFileAVPlayerChange has no desktop event yet. A hosted file's `[2, 0d]`
  and `[2, 18]` are values: the desktop reads them with the file's node
  `[2]`, since a download of their own key answers inline. Table-view
  object triggers are decoded but not fired: the client's table view draws
  the record's fields, and which table-view object shows which field is not
  specified. The window opens on the default layout when `0x40` is set.
  Data entry with system formats (`0x400`, U1) is not offered.
