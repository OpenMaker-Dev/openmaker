# Manage Database in OpenMaker's client

File > Manage > Database shows the provider's tables, fields, table
occurrences and relationship predicates, decoded from the file's catalogs
([schema-path.md](re/schema-path.md) 5, [file-metadata.md](re/file-metadata.md)),
and saves changes to them the way FileMaker Pro does.

## The dialog

Code: `crates/om-gui/src/database.rs`. It follows FileMaker Pro 26's
dialog (reference screenshots taken 2026-11-11 from a copy of the Contacts
starter solution):

- **Tables:** "N tables defined in this file", View by (creation order,
  table name), and a list of Table Name, Source ("FileMaker"), "Details /
  Comments (Click to toggle)" (the field count, or the comment) and
  "Occurrences in Graph". Table Name and Comment; Create, Change, Delete.
- **Fields:** the Table menu, "N fields", View by (creation order, field
  name, field type), and a list of Field Name, Type and "Options / Comments
  / Advanced   (Click to toggle)". A click on that title steps the column
  from each field's options to its comment, to its advanced options and
  back; the options read as FileMaker Pro lists them ("Indexed,
  Auto-enter Calculation replaces existing value, Can't Modify Auto,
  Required Value, Unique", "= Count * 2", "= Total of Price";
  `DatabaseSchema::field_column_text`, client-ui.md). Field Name, the Type menu
  (Text, Number, Date, Time, Timestamp, Container, Calculation, Summary)
  and Comment; Create, Change, Delete; Options… for the selected field.
- **Specify Calculation** (`script_workspace::calculation` in field mode):
  Create with the type Calculation, or Options… on a calculation field,
  opens it on the field's formula: the context occurrence (the field's
  table's occurrences), the fields and functions panes, "Calculation
  result is", "Number of repetitions", "Do not evaluate if all referenced
  fields are empty" and Storage Options (global storage, "Do not store
  calculation results -- recalculate when needed", indexing, automatic
  indexes; its layout follows Options for Field's Storage tab and was not
  compared with FileMaker Pro). The names it compiles against are the
  schema as edited, with the file's functions. OK stages the field; a
  change the draft refuses keeps the dialog open with the reason.
- **Options for Summary Field** (`database_summary`): Create with the type
  Summary, or Options… on a summary field: the kind (Total of, Average
  of, Count of, Minimum, Maximum, Standard Deviation of, Fraction of Total
  of, List of), the field it summarizes (fields the kind cannot take are
  unavailable), the kind's option (running total, weighted average,
  running count, by population, subtotaled), "Restart summary for each
  sorted group" with its field, and "Summarize repetitions". The layout
  follows Claris's help and was not compared with FileMaker Pro.
- **Options for Field** (`crates/om-gui/src/database_options.rs`):
  - FileMaker Pro 26's Auto-Enter, Validation, Storage and Furigana tabs,
    with Cancel and OK. OK stages the field's options.
  - The auto-entered values are one choice, as in FileMaker Pro:
    Creation or Modification (date, time, timestamp, name, account
    name), a serial number (on creation or on commit, next value,
    increment), the value from the last visited record, data, a
    calculated value with "Do not replace existing value of field (if
    any)", or a looked-up value. Prohibit modification goes with them.
  - Checking "Looked-up value" opens Lookup for Field
    (`crates/om-gui/src/database_lookup.rs`), whose OK checks the box;
    Specify… opens it again. It offers the starting occurrence (one of
    the field's table), the occurrences the graph relates to it, the
    field to copy, "If no exact match, then" (do not copy, next lower,
    next higher, use a constant) and "Don't copy contents if empty". Its
    layout follows Claris's help and was not compared with FileMaker Pro
    26.
  - Checking "Calculated value" or "Validated by calculation" opens
    Specify Calculation, whose OK checks the box; Specify… opens it
    again. There the result type and repetitions are the field's and
    unavailable, and the checkbox under the formula is "Do not evaluate
    if all referenced fields are empty", or for a validation calculation
    "Validate only if field has been modified" [I: FileMaker Pro's
    layout of these two dialogs was not captured]. The custom message's
    Specify… opens it for a message calculation, with the result Text and
    no checkbox [I: likewise not captured].
  - Validation: Always or only during data entry, allow override,
    strict data type, Not empty, Unique, Existing, member of one of the
    file's value lists, in range, validated by calculation, maximum
    characters, and a custom message, typed as text or a calculation;
    the calculation shows in the message box until text replaces it.
  - Storage: global storage, repetitions, indexing None, Minimal (text
    only) or All, automatic indexes, and the default language
    (`mIndexLang`, definition bytes 4-7, schema-path.md 5.5).
    - The language list is Sort Records' list (script-steps.md 11.1.2),
      in its order, with Unicode (Unicode raw, 2) first.
    - Only the languages whose index keys OpenMaker builds as FileMaker
      does can be chosen (`om_data::rules::written_text_keys`): Unicode,
      English and English (international). The others are listed
      unavailable ([#299](https://github.com/OpenMaker-Dev/openmaker/issues/299), [#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)).
    - A field that has a language the list does not show (none, System,
      Unicode standard, catalog) shows it by its catalog name.
    - Whether FileMaker Pro's own list also has a "Default" entry, and
      what it writes, is unmeasured.
  - Furigana, on a text field (schema-path.md 5.9, "Furigana"):
    - "Use Furigana Field";
    - the field that receives the reading, one of the table's stored text
      fields other than this one;
    - the translation. Only Full-Width Katakana and Half-Width Roman, the
      two FileMaker Pro 26.0.3 was seen to write, can be chosen. The others
      are listed unavailable, and a field's own is kept
      ([#326](https://github.com/OpenMaker-Dev/openmaker/issues/326)).

    They are written as Pro writes them: bit `0x40`, and member `07`'s
    second part, its other bytes kept. Turned off, the bit alone goes. A
    value list set on the field keeps the furigana part, and the furigana
    keeps the value list. OpenMaker's record rules refuse changing such a
    field's value, as OpenMaker does not write the reading (#1302, [#322](https://github.com/OpenMaker-Dev/openmaker/issues/322)).
  - What the field has but OpenMaker does not change yet is shown
    unavailable: a lookup whose definition does not read.
- **Relationships:** the graph and its tools. Add a table occurrence opens
  Specify Table (this file's tables, the name); Add a relationship opens
  Edit Relationship (both occurrences and their fields, the operator,
  Add, Change, Duplicate and Delete of match pairs, and each side's "Allow
  creation", "Delete related records" and "Sort records" with Specify…,
  which opens Sort Records on that side's occurrence; checking "Sort
  records" without an order opens it too). A double click edits a box
  (Specify Table, its name) or a relationship (Edit Relationship, its
  occurrences fixed). Edit and Delete act on the selection, and the colour
  tool colours the selected box. A box is moved by dragging its title or
  background. Dragging a visible match-field row onto one in another
  occurrence stages an equality match through the same schema operations.
  An additional drag appends a predicate to an existing relationship,
  preserving its side options and sort orders. Repeated identical pairs
  leave it unchanged. Cancel discards the edit; OK commits the schema
  transaction. Drags are bound to the starting schema and refuse a changed
  catalog. Containers, summaries and unstored fields cannot start or accept
  a link here. A same-box drop explicitly asks for a duplicate occurrence;
  automatic self-join occurrence creation is not implemented by this slice.
  Shift-click adds a box to the selection or takes it out. Duplicate copies
  the selected occurrences and the relationships among them in one change
  (`SchemaChange::DuplicateOccurrences`):
  - each copy keeps its occurrence's stored definition: data source, table,
    colour and box, moved 20 points right and down;
  - an empty box stays empty, for the graph to place, and a definition with
    no box gets a new occurrence's;
  - each copy is named as a new occurrence of its table is: "name 2",
    "name 3" and so on, the first free;
  - each relationship joining two selected occurrences is made between
    their copies, with its match pairs, option bits and sort orders kept
    byte for byte.

  Edit, Delete and the colour tool act on one object, so they are off while
  more than one box is selected. The Arrange tools work on two or more
  selected boxes, as one change of their stored boxes (`om_data::arrange`,
  `SchemaChange::ArrangeOccurrences`):
  - **align:** left, center or right edges; top, center or bottom edges;
  - **distribute:** horizontally or vertically;
  - **resize:** to the smallest or largest width, height, or both.

  Edges align to the outermost box. Centers align to the center of the
  selection's bounds. Distributing keeps the two outermost boxes and spaces
  the others evenly between them. Resizing keeps each box's top left.
  Positions are rounded to whole points.
- Cancel and OK at the bottom right.

Deleting asks first, in FileMaker Pro's words: for a field "Permanently
delete the selected 1 field(s) and ALL of their field data?"; for a table
"Permanently delete the selected 1 FileMaker table(s) and all of their
fields and record data?" with "Also remove occurrences of these tables in
the graph" (unchecked by default, Cancel the default button).

Shown but disabled until OpenMaker has them: Print, Copy, Paste,
Import, and the custom order of View by.

## Editing and saving

Each operation is the provider's (`om_data::DataProvider`):

1. Opening the dialog calls `begin_schema_edit`.
2. Create, Change, Duplicate and Delete call `apply_schema_change` with
   an `om_data::SchemaChange`. The change is staged and the dialog shows the
   result; nothing is sent to the host.
3. OK calls `commit_schema_changes`, Cancel `cancel_schema_changes`.
   After a saved OK, every window of the file reloads its layout and asks
   for the calculation schema again (`calc_schema`). So Layout mode's
   Field tool lists a new field without the file being reopened (#726).

The sample provider stages the same changes on its sample catalog.

On a hosted file or one opened from this computer (which opens through a
private embedded OpenMaker host), `om-provider` (`schema.rs`) does what
FileMaker Pro does (schema-path.md 2):

- **Opening** locks `[08]` and the three catalogs `[3, 16]`, `[3, 17]`,
  `[3, fb]`, then reads them and every table's field catalog under those
  locks. A second user then gets FileMaker's error 303, "The database
  schema is in use by <name>", and the dialog does not open for them.
- **Each change** is applied to OpenMaker's copy of the catalogs by
  `om_data::schema_draft`, whole or not at all. The copy is edited as
  FileMaker Pro edits its own, as compared key by key with FileMaker Pro's
  saves (schema-path.md 5.8):
  - **New table:** the next table id, an occurrence of the same name
    (FileMaker's placement-pending empty box and grey colour), an empty
    field catalog and the table's dependency index entry.
  - **Changed table:** a renamed table takes the occurrences that had its
    name with it. Comments are kept with the table.
  - **Deleted table:** keeps its occurrences unless they are removed too;
    removing them removes their relationships as for a deleted occurrence.
  - **New field:** the 26-byte definition with FileMaker's defaults, the
    index language most of the table's fields have, the name lists, the
    display order and the count. A container field is also listed in the
    dependency index.
  - **Changed field:** the name lists follow the name and type, and the
    host is asked to note the change (`0x200`). What a type change does to
    stored values (schema-path.md 7.2.1):
    - **Kept:** among text, number and time, from a date or a timestamp to
      any of those, and between date and timestamp. Every stored text stays.
      Made on any field.
    - **Converted:** to a date or a timestamp from text, number or time, or
      from a container. The host rewrites the values (or deletes them, with
      their container data). A global field's value is not rewritten to a
      date or a timestamp, as FileMaker Server leaves it, but is deleted
      from a container. Made on any field of a file on this computer,
      whose host is OpenMaker's own and converts them. In a hosted file it
      is made only on a field that holds no value: the editor cannot make
      sure the host converts
      ([#292](https://github.com/OpenMaker-Dev/openmaker/issues/292)).
    - **To a container:** the text stays, and what it reads as there is not
      established
      ([#293](https://github.com/OpenMaker-Dev/openmaker/issues/293)).
      Made only on a field that holds no value.

    A field whose type changes more than once in one edit must be allowed
    each change on its own, while the host's work and the save's check
    that a table is still empty follow the type the file holds and the
    final type: Text to Date and back to Text converts nothing.

    A change of repetitions is made on any field of a file on this
    computer, whose host deletes the repetitions above a smaller number
    from every record, or from the global value of a global field
    (schema-path.md 7.2). In a hosted file it is made only on a field that
    holds no value, as the editor cannot make sure the host deletes them
    ([#292](https://github.com/OpenMaker-Dev/openmaker/issues/292)). A field that holds no value is:
    - one made in the same edit;
    - in a file on this computer, a field that is not global (in the file
      or in the edit) of a table that had no record when Manage Database
      opened. A global field keeps its value in the table's global record
      (schema-path.md 7.2.1). At OK, holding the lock on the records of
      each table whose fields change, which no other session's record lock
      (a new record's included) can share and under which OpenMaker Server
      commits no other session's record, OpenMaker reads the table's
      record list and what is stored under `[T, 05]` again, along with any
      new record of its own open transaction, and refuses the save,
      changing nothing, if a table such a change relies on now has one. A
      table whose records cannot be read counts as having some. A hosted
      file's other clients could add a record meanwhile, so there every
      field not made in the same edit may hold values.
  - **Duplicated field** (schema-path.md 5.14): the selected field's copy
    at the end of the list, named as FileMaker names it ("Name Copy",
    then "Name Copy2"...), and selected. OpenMaker makes it through the
    paths of Create and the options dialogs, so the catalog's name lists
    and evaluation lists are written as for a new field, then gives it the
    original's tags (rewritten as FileMaker rewrites them), next serial
    value, summary and lookup padding (the definition `08` as it is when
    it reads as the same lookup, schema-path.md 5.5) and definition byte
    2. The copy must then hold
    what FileMaker's member copy holds, byte for byte but for its name,
    identity and bookkeeping; otherwise the duplicate is refused with the
    member that would differ. A name calculations write as `${…}` is
    allowed for a copy whose original has one, as FileMaker keeps it.
  - **Deleted field:** the display order is renumbered without gaps, and
    the host's record work (`0x1e1`, plus global and container bits) is
    listed. A field with dependency data of its own (a calculation, a
    summary, an auto-entered or validation formula, a lookup, an entry
    option or a validation) also leaves its `fff0`, `fff1` and `fb`
    entries and the table's `fffc` (also `fffc, 00`) and `fffb` lists,
    and the table's evaluation lists are rebuilt without it, as FileMaker's engine does
    (schema-path.md 5.15). That needs the table's dependency entry to be
    one the rebuild reproduces; otherwise the deletion is refused.
  - **Field options** (schema-path.md 5.9, 5.13):
    - The definition's option and validation words, serial increment,
      byte limit and repetitions; the next serial value `0c` and the data
      `0d`.
    - A calculated value's formula in `05`, compiled from its context as
      a calculation field's is, with a `fb, F, <context>, <occurrence>`
      node for each related occurrence it reads (schema-path.md 5.13); the value list reference `07` in the
      14-byte form of FileMaker Pro's files; the range's ends as text in
      `21` and `22`; a validation calculation in `19`; a custom message
      as text in `18`, or as a calculation as FileMaker Pro 26's dialog
      writes one (`0x8000` with `0x80`, the formula in `20`, no dependency
      entries, and in `18` its text for older versions, 5.9 E34); a lookup
      in `08` in FileMaker Pro 26's own byte
      form (schema-path.md 5.5, which OpenMaker's record rules read), with
      its constant in `0b`. Turning an option off keeps its member, as
      FileMaker's files do.
    - The field's place in the table's evaluation order in the dependency
      index, rebuilt from the fields' triggers (schema-path.md 5.10):
      fields are ordered by how many triggers they reach, then by id, and
      later positions are renumbered. The field's events, or a calculated
      value's inputs and creation, go in the field catalog's `fff0`; a
      validation calculation's inputs in `fff1`; a lookup's starting and
      looked-up occurrences in `fb, F`, from which the rebuild derives
      its match fields as triggers, as it does for the tables' existing
      lookups and related auto-entered formulas (Assets' Assignments, for
      one, now takes option and calculation edits).
    - Its entry in the list of fields validated Not empty, Unique,
      Existing or by calculation (`fffc`), and `fffc, 00` when such a
      field is validated Always.
    - Unique and Existing turn the value index on, as FileMaker Pro does.
    - A changed index raises the table's data version and asks the host
      to build or drop the index (`0x10`, `0x20`). OpenMaker Server does
      both in op73.
    - Repetitions changed on a field not made in the same edit (see
      Changed field) ask for the host's work by 7.2's rules, as a
      calculation's do: `0x40`, and for fewer repetitions than the field
      had when Manage Database opened `0x4 | 0x1000` (`0x8` for a
      container, `0x2` for a global), without `0x4` while the values are
      deleted anyway (`0x1`) or a container's type changes too (`0x8`
      deletes every repetition); `0x10` on an indexed field. The deleting
      bits follow the definition the file holds and the one the field ends
      with, whatever the edit did in between. The field's data mod count
      and the table's data version go up. OpenMaker Server deletes the
      repetitions above the new number at op73 (the cells `F` then the
      byte n from the new number + 1, up to 255), releasing a container's
      data and counting a modification on each record it changes, then
      rewrites what is left of a field changed to a date or timestamp.
    - Global storage also changes the field's key in the name list by
      type. Turned on or off on a field not made in the same edit, it
      asks the host to delete every record's value (`0x1`), or the global
      value when the field was global (`0x1 | 0x2`), with `0x40`, and
      raises the field's data mod count `fb` (schema-path.md 7.2, E38).
      A field the file holds as a container has its data released with
      them (`0x8`), whatever type the edit gave it first. The work is
      reckoned from the storage the field had when Manage
      Database opened, which holds its values: storage turned back before
      OK asks for nothing, and a field deleted after a change is deleted
      where its values are. OpenMaker Server does that work at op73,
      deleting the global value at `[T, 08, F]` as FileMaker Server's
      `DeleteGlobalRecordData` does, or the records' values, every
      repetition with them (the cells `F` then the byte n, calc-values.md);
      a field made global starts with no value.
  - **New, renamed and deleted occurrences:** a deleted occurrence takes
    its relationships, the other occurrences' relationship lists, and the
    routes and key-field reach that no longer connect.
  - **Calculations over related fields and aggregates** (schema-path.md
    5.12): a formula that reads a related field or an unstored
    calculation is made unstored, as FileMaker makes it; related inputs
    and aggregates (`Count ( Phone Numbers::Number )`, `Sum ( Title )`)
    have their own trigger forms and relational entries in the dependency
    index, which relationship edits keep. Every calculation's `0x80000`
    is recomputed at each rebuild, as the engine recomputes it.
  - **New and changed calculation fields** (schema-path.md 5.10): the
    formula compiled from the field's context occurrence; the definition
    (kind, result, repetitions, "Do not evaluate…" as option `0x20`,
    storage and index bits, `0x80000` for a formula that reads no field);
    the member `05` with the context and both streams, `fb` = `01 01`, the
    name lists; the field's direct triggers in `fff0` and the table's
    evaluation lists rebuilt from them, as the engine rebuilds them. A
    change asks the host for the record work of schema-path.md 7.2
    (`0x40` and `0x80` for a new formula, `0x1` for values no longer
    stored, index bits); OpenMaker Server recomputes stored calculations
    whose formula or storage changed at op73.
  - **Calculations over a global or summary field of their own table**
    (schema-path.md 5.16): made unstored whatever storage was chosen, as
    FileMaker's engine makes them, with byte 2 `00`; the global or summary
    field is a direct trigger of type `01`; a calculation that reads a
    summary field joins the table's `fffb` list beside it, and leaves it
    when it no longer does; the rebuilt dependency entries carry `0x50`
    under a global input and `0x40` under a summary input.
  - **Auto-entered and validation formulas over a global field of their
    own table** (schema-path.md 5.17): written by 5.13's rules, the global
    an ordinary input of type `01`, as FileMaker's engine writes them.
  - **New and changed summary fields** (schema-path.md 5.11): the
    definition (kind 3 and the result type), `09`, `fb`, the name lists;
    the trigger on the summarized field in `fff0`, the table's `fffb`
    list and the rebuilt evaluation lists.
  - Options for Field rebuilds the evaluation lists by the same rule.
  - Every other key, known or not, is kept as it was.
- **OK** saves in FileMaker Pro's order:
  - op82 for the host's clock, so the new and changed members and each
    uploaded catalog's index get UUIDs and modification stamps (user name,
    account, host time; the count up by one);
  - the changed field catalogs are locked;
  - each catalog to be replaced is downloaded again and compared with the
    copy the edit began from, leaving out stamps. If another client
    changed it, the save is refused with 306 and nothing is sent;
  - the tables whose fields change are locked (`[T, 05]`, 2 s), so a
    record being edited there refuses the save with 303;
  - each catalog's lock is upgraded (op61), the catalog is uploaded whole
    (op13) and released with attention 2, which tells the other clients to
    read it again. Base tables go first, then relationships, occurrences
    and field catalogs, each field catalog followed by op73;
  - deleted tables are removed with DeleteTable under their own lock.
    OpenMaker sends this after the table catalog no longer lists them
    (FileMaker Pro sends it before), so a failure in between leaves
    unreachable records rather than a table without fields;
  - finally the schema lock is released with attention 3 and the table
    locks with `0x201`.
- **Verifying.** The schema is then read again over the same connection,
  and the save is reported only when the tables, fields, comments,
  occurrences and relationships the host has are the draft's. A host that
  stored nothing (OpenMaker Server ignores catalog uploads from an account
  without [Full Access]) or something else is reported as not saved.
- **Failures.**
  - A save refused before anything was sent keeps the edit open, with its
    locks and changes, so the user can try OK again or Cancel.
  - A failure after the first upload says which catalogs were stored, and
    the edit ends.
  - Cancel and closing the file release the locks.

**What may be saved.** Tables, fields and occurrences may be saved only on
OpenMaker Server, or a file opened from this computer, and only when the
host lets the account change the design. That is asked through OpenMaker
Server's authoring extension, whose field-authoring grant needs the same
[Full Access] that catalog uploads need. On FileMaker Server the dialog is
read only, with the reason shown: the transaction itself was replayed there
by the RE probe (schema-path.md 10), but catalogs OpenMaker writes have not
been opened by FileMaker clients of a FileMaker Server, and a mistake would
reach every client of a production file.

## The relationships graph

The Relationships tab draws the graph as FileMaker Pro 26 draws it
(schema-path.md 5.3, "How FileMaker Pro 26 draws it"), in `om-gui`'s
`database_graph` module. Its tools stage the edits listed under "The
dialog". It draws:

- Each occurrence at its stored box: every field for display word 0, the
  title bar and key fields for 1, as observed; the title bar alone for 2,
  which is inferred and not observed; every field for any other value.
  The box anatomy, text positions and the default grey's tints are
  measured at 100%. Other colours are tinted the same way, which is
  OpenMaker's assumption. Text is Inter at 10.5 points, 0.35 points
  tighter per letter, which has FileMaker's cap height and its field
  names' widths in the Contacts starter solution within about 2 points.
- The key-field area lists the fields the occurrence's relationships
  match on in the table's field order (FileMaker's order for several key
  fields is not compared), each with a ring on the side a line leaves by.
  It shows at most 200 rows; past that, the last row says how many more
  key fields there are and their lines meet it (OpenMaker's choice, so
  that a hostile file cannot size a box or the canvas). Only the rows in
  view are painted.
  The field list shows every field in creation order, key fields in
  italic, fields that cannot be match fields in grey, clipped at the box.
- A relationship's line runs from its first predicate's key row on one
  side to the other's, with 14-point stubs, its operator framed at the
  middle and a crow's foot at each "many" end. A relationship of several
  predicates draws a ring, a stub and, on a "many" side, a crow's foot at
  every key row it uses; the stubs converge on the first row's line
  (OpenMaker's choice, not compared). An end is "one" when an
  `=` predicate uses a field there with unique-value validation (observed)
  or an auto-entered serial number (inferred). Boxes that overlap
  horizontally are joined on their right sides, and a relationship whose
  predicates use different operators shows each distinct operator in its
  box: both OpenMaker's choices, not compared. Resting the pointer on the
  operator box lists every predicate, one per line (OpenMaker's choice:
  FileMaker's tooltip is not captured); the operator box is also an
  accessibility label reading them, such as `Contacts::PrimaryKey =
  Addresses::ForeignKey`.
- Occurrences with no readable box (files OpenMaker's tests build) are
  placed by OpenMaker: each connected group in columns by its distance
  from its first occurrence, 131 x 118 boxes, under any stored boxes.
  The sample provider gives a new occurrence a 131 x 118 box right of the
  graph's top row.
- Under the graph are FileMaker's Tables / Relationships, Arrange, Tools
  and Pages groups. The selection tool, zoom in, zoom out and the zoom
  percentage work (10 to 400%; the buttons step through 25, 50, 75, 100,
  150, 200, 300 and 400%, the document zoom's steps, which is OpenMaker's
  choice). The Tables / Relationships tools work where the schema can be
  saved, and so do Duplicate and the Arrange tools (above); text notes,
  table display and Pages are shown unavailable
  ([#310](https://github.com/OpenMaker-Dev/openmaker/issues/310)).
- Clicking a box or a relationship's operator selects it; the selection
  outlines are OpenMaker's, as FileMaker's are not captured.

Not drawn or not compared: text notes on the graph (not decoded), page
breaks, resizing boxes, the display button, and FileMaker Pro's drawing at
zooms other than 100%. Field-drag behavior follows public Claris Help,
without a new native drag/save timing comparison.

## What it refuses (error 3, nothing staged)

Each of these is refused because what FileMaker Pro writes for it is not
established:

- deleting a field another definition uses: one a calculation, an
  auto-entered or validation formula, a summary or a lookup reads (also
  a validation of another table through any path of relationships), one
  a relationship matches on or sorts its records by (either side's order,
  also when "Sort records" is off), or one a value list takes its values
  from (checked when the file can be read for calculations). FileMaker's engine leaves a field another
  definition reads in place, and a deleted match field leaves the
  relationship naming it (schema-path.md 5.15); what FileMaker Pro writes
  for the definitions it leaves reading a missing field is not
  established;
- changing the type of a field that may hold values (see Changed field)
  to a container, whose stored text's reading is not established ([#293](https://github.com/OpenMaker-Dev/openmaker/issues/293)),
  or, in a hosted file, to one its values would be converted to: the
  editor cannot make sure the host converts them ([#292](https://github.com/OpenMaker-Dev/openmaker/issues/292));
- deleting a field or an occurrence while the file has a relationship
  OpenMaker cannot read (a definition, a predicate list, or a predicate
  with an operator above 6 that does not decode), which might use it;
- turning a field into a calculation or a summary field or back (names
  and comments can be changed);
- deleting a field a summary field summarizes, weights, subtotals or
  restarts by, or changing its type (the summary's type follows it);
- a summary's second field (weight, subtotal or restart) of another table
  than the summary's own; a weight that is not a number field, or a
  subtotal or weight that is a summary or container field, as the dialog
  offers none [I: FileMaker Pro's own refusals are unmeasured];
- deleting a field, an occurrence or a table that a calculation reads
  through a relationship;
- calculation fields whose dependency entries are not established
  (schema-path.md 5.10):
  - a formula that reads a summary or global field of another table, or
    one of its own table's: beside a related field, an unstored
    calculation, a `Get` function, `IsEmpty` or a function of its kind;
    as a field reference or in an aggregate (`Sum ( S )`); with "Do not
    evaluate if all referenced fields are empty" off; or in a calculation
    chosen unstored or global (5.16 measured calculations asked to be
    stored, reading every field as a value);
  - a calculation that reads such a calculation, here or through a
    relationship; making a calculation another definition reads read a
    global or summary field; leaving unstored a calculation that a
    calculation over a global or summary field reads; an auto-entered or
    validation formula over a summary field, over another table's global
    field, or over a container or repeating global (5.17 measured its own
    table's text globals of one repetition);
  - a formula that uses `$` variables, `Self`, `Evaluate`, `GetField`,
    or custom, design or plug-in functions;
  - any calculation in a table whose evaluation lists the rebuild does
    not reproduce (a related or summary input it does not model);
- calculations OpenMaker could not keep computed. OpenMaker Server
  recomputes a stored calculation only when record rules can evaluate it,
  and a table with one it cannot compute is read-only to OpenMaker:
  - global calculations, whose value nothing computes yet;
  - repeating calculations;
  - a stored calculation with a container result, over a container field,
    or with a formula record rules do not evaluate (a `Get` selector
    that needs a window, for example);
  - a change that leaves another stored calculation reading a field that
    is no longer a stored value (made unstored, global or a container);
- in Options for Field:
  - changing a lookup whose definition does not read, or a field that
    enters more than one kind of value;
  - a lookup OpenMaker's record rules could not perform, so the table
    would become read-only to OpenMaker: one into another type than
    text, number or date, with another choice than "do not copy" without
    an exact match, with Prohibit modification, from the field's own
    table, copying anything but a stored text, number or date field of
    one repetition (a date field only from a date), or following more
    than one
    relationship, one sorted by anything but the related table's own
    stored fields, by a value list's order or by Japanese text, or one
    with other than `=` predicates on stored text fields whose start side
    has no auto-entry other than a `Get ( UUID )` key (record-writes.md,
    Lookups);
  - deleting a relationship, occurrence, table or copied field a lookup
    reads, and changing the match fields or sort order of a relationship
    a lookup or related auto-entered formula follows (their triggers are
    derived from it, schema-path.md 5.13); Edit Relationship OK'd with
    its match fields and orders as stored is accepted and keeps the
    definition's bytes;
  - a calculated value or validation calculation OpenMaker's record
    rules could not evaluate, so the table would become read-only to
    OpenMaker: one over unstored calculations, repetitions, `Evaluate`,
    plug-in functions or a `Get` selector that needs a window; one over
    related fields other than another table's stored values through one
    relationship of `=` on plain stored text match fields, sorted as a
    lookup's may be (two relationships, other sort orders, match fields
    with auto-entry; an aggregate of such a field is accepted, #1036); a
    calculated value for a container or repeating field, a serial number
    in a date, time, timestamp or container field (the record rules
    number only text and number fields; which types FileMaker Pro numbers
    is not measured), and a serial number or a lookup in a repeating
    field;
  - member of a value list the record rules do not check values against
    (one with a second field or related values only; Options for Field
    shows these unavailable), or of any value list when the file cannot
    be read for calculations;
  - a message calculation the record rules cannot evaluate over the
    record alone: one reading related fields, a summary, an unstored
    calculation, a container or a repeating global, or a `Get` answer only
    the write gives (`Get ( RecordNumber )`, the layout number, the
    privilege set name, the host's clock);
  - a furigana translation FileMaker Pro was not seen to write ([#326](https://github.com/OpenMaker-Dev/openmaker/issues/326)), a
    furigana field that is not a stored text field of the table, and
    furigana on a field that is not text;
  - an index language whose keys OpenMaker does not build (any but
    Unicode raw, English, English international, System and Unicode
    standard; [#299](https://github.com/OpenMaker-Dev/openmaker/issues/299), [#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)). In a hosted file, any change of the index
    language of a field that may hold values: the editor cannot make sure
    the host rebuilds the index in the new language, as an OpenMaker
    Server older than 2026-11-14 keyed every index in English ([#292](https://github.com/OpenMaker-Dev/openmaker/issues/292)). In
    a file on this computer the change asks the host to rebuild the index
    (`0x10`, schema-path.md 7.2), and OpenMaker's host keys it in the new
    language;
  - changing the repetitions of a field that may hold values in a hosted
    file (see Changed field), as the editor cannot make sure the host
    deletes those above a smaller number ([#292](https://github.com/OpenMaker-Dev/openmaker/issues/292));
  - changing the global storage of a field a calculation, summary,
    relationship or option uses (FileMaker Pro makes the stored
    calculations that read a field made global unstored, 5.9, E38, which
    OpenMaker does not yet), also of a field made in the same edit, or of
    one with auto-entered or calculated options;
  - global storage with auto-entered values, required values or an
    index;
  - the options of a summary field (Options… opens Options for Summary
    Field instead).

  These are refused too, but as FileMaker Pro's dialog does not offer
  them, not error 3:
  - an index on a container field;
  - Minimal indexing on another type than text;
  - Prohibit modification without an auto-entered value;
  - a serial increment of 0;
  - an empty next serial value;
  - more than 1000 characters of next value or data;
  - no value list chosen or one the file does not have, an empty end of
    a range or more than 1000
    characters in one, an empty custom message or one of more than
    30000 characters;
  - repetitions outside 1-32000;
  - a maximum of 0 characters.

Names must be ones a calculation can use as they are, the rule `om-author`
applies (`field_catalog::check_name`), and must differ from the other
tables', fields' or occurrences' names by their primary weights, so neither
case nor accents tell names apart. FileMaker Pro accepts some other names
after a warning, which OpenMaker does not. FileMaker's own keys tell
accents apart; whether Pro takes `Grosse` beside `Größe` is not
established, so OpenMaker refuses it. A name is listed in the name lists
under the key FileMaker Pro lists it under, with its accent and kana levels
(re/schema-path.md 5.6, `om_collate::name_key`), in every file language.
OpenMaker refuses a name whose key it cannot build: one with `‚`, `‛`,
`‟` or a CJK compatibility ideograph, one that mixes an accent or kana with
a character FileMaker gives its own weights other than a CJK unified
ideograph (Hangul, CJK extension A, private use), and one whose key would
pass FileMaker's 210 bytes. A deleted or renamed member's entries go
whatever key they were listed under, so an entry an earlier OpenMaker
wrote under the primary weights alone does not stay behind.

## Decoding

- **Occurrences.** A definition must begin `02 01 00`, followed by the base
  table as a key string (schema-path.md 5.3). The decoder checks that
  prefix and that the base table exists; names and low id bits are never
  used to guess ownership. The graph box is the 28 bytes after the two
  key strings (`om_hbam::TableOccurrence::graph_box`): four big-endian
  f32 (x, y, width, height), the field-list height, the display word and
  the RGBA colour. Anything but exactly 28 bytes, or a coordinate that is
  not finite, reads as no box. An occurrence of a deleted table shows no
  table.
- **Field traits.** A field is drawn as the "one" end of a relationship
  when it has unique-value validation or an auto-entered serial number,
  and grey when it is a container, a summary or an unstored calculation
  (option bit `0x20000`), from its 26-byte options (schema-path.md 5.5).
- **Relationships.** A key inside a relationship definition is an outer
  length followed by the normal HBAM key encoding (an inner numeric length,
  then the big-endian number). The decoder checks both bounds and every
  referenced table and field. An invalid or unrecognized definition sets
  `relationships_decoded = false`, which the dialog shows differently from
  an empty graph.
- **Operators.** Each predicate's operator is shown by its symbol: 0 `=`,
  1 `≠`, 2 `<`, 3 `≤`, 4 `>`, 5 `≥` and 6 `×` (cartesian), read through
  the same `JoinOp` the query path uses (file-metadata.md 4.2). A stored
  code above 6 marks the graph undecoded, as FileMaker's own read fails on
  it (7 and 8 exist only in memory, file-metadata.md 4.2).
- **Comments.** A table's `[3, 16, 5, T, 03]` and a field's
  `[T, 3, 5, F, 03]`.

## Limits

- **Relationships.**
  - Relationships are saved as FileMaker Pro saved them in schema-path.md
    5.8 (E20, E21): the definition, the match pairs, the occurrences'
    relationship lists, the routes, and the key fields' reach and markers.
    Multi-pair relationships and cartesian pairs are written the same way,
    which is OpenMaker's reading, not compared.
  - A side's records are sorted only by that occurrence's own fields, not
    by related fields or a summary field (Sort Records offers them; the
    draft refuses them). A stored order of another form is kept as long
    as that side's order is not specified again. Option bits outside the
    side checkboxes are kept as stored.
  - Specify Table renames an occurrence but cannot point it at another
    table, and offers no other data source.
  - Two occurrences already connected cannot be related, as FileMaker
    allows one path; FileMaker Pro offers to add an occurrence instead.
  - A new occurrence gets FileMaker's empty box (FileMaker Pro places it
    when it next shows the graph; OpenMaker places it in its own column
    layout).
  - Duplicate's default name and offset are OpenMaker's choice (FileMaker
    Pro's are not documented), and the originals stay selected after it.
  - What the Arrange tools align centers to, and how they distribute, is
    OpenMaker's reading of FileMaker's layout tools, not measured in the
    graph. They work on the stored boxes, so a collapsed box's full height
    counts. A box whose definition has no stored box (not one FileMaker
    writes) refuses, as moving it does. Moving several selected boxes
    together, deleting several at once, and duplicating notes are not built.
    Field dragging currently links existing occurrences; a same-box self-join that automatically
    creates another occurrence is not implemented. Indirectly connected
    occurrences retain the existing one-path refusal.
- **Field options in part.** These cannot be set yet
  (#431):
  - calculated values and validation calculations over related fields
    beyond what the record rules evaluate (above);
  - lookups beyond what the record rules perform (above);
  - index languages without keys ([#299](https://github.com/OpenMaker-Dev/openmaker/issues/299), [#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)), and furigana translations
    FileMaker Pro was not seen to write ([#326](https://github.com/OpenMaker-Dev/openmaker/issues/326));
  - repetitions of fields that may hold values, and global storage of
    fields that other definitions use (above). A field with more than one
    stored auto-enter source also refuses any Options edit, since decoding
    and re-encoding a single source would otherwise lose the others.

  Summary fields of every kind are defined and changed, the weighted
  average and the subtotaled fraction with their second field of the
  same table (#432). What the draft writes was compared with FileMaker
  Server's engine (schema-path.md 5.11, runs s01-s16), not with FileMaker
  Pro's own dialog.
- **Calculation fields in part.** The formulas listed under "What it
  refuses" are refused. What the draft writes for a calculation was
  compared with FileMaker Server's engine (its importer), not with
  FileMaker Pro's dialog: byte 2 of the definition, `26` and the deferred
  work bits FileMaker Pro sends to a host for a new or changed calculation
  are OpenMaker's reading (schema-path.md 5.10, 7.2). FileMaker Server's
  engine opens a file with calculation fields OpenMaker made, exports it
  as XML and computes them; FileMaker Pro itself has not opened one.
- **Tables OpenMaker then opens read-only.** A table with a value from
  the last visited record opens read-only in OpenMaker's own browsing, as
  it does for files FileMaker Pro made
  (#186).
  FileMaker Pro clients enter records in it as usual.
- **Duplicate.** One field at a time (FileMaker duplicates every
  selected field). Refused: any field with a member OpenMaker does not
  write (`29`-`31`...) or an index language other than the one
  a new field of the table takes; a field whose definition, options or
  formula OpenMaker would write differently. Every field of the six
  starter solutions duplicates, in every language, the Assets lookup
  included. The copy's tags when none is left, its `fb`, and the lookup
  padding kept from the original follow OpenMaker's reading of
  FileMaker Pro (schema-path.md 5.14), not a capture of its Duplicate.
- **Missing list features.** Custom order, Copy, Paste, Import and Print
  are not built, and the record counts of "Details" are not read
  ([#152](https://github.com/OpenMaker-Dev/openmaker/issues/152)).
- **The Fields tab's third column.** Advanced shows nothing: OpenMaker
  does not read a field's advanced members (`29`-`31`). The options text
  says "Use Furigana" without the furigana field, and a summary's break or
  weight field by its name alone. Only the field name is exposed to
  accessibility, as for every list in the dialog. The text follows
  FileMaker Pro's code and strings; it was not compared with its screen.
- **No layout for a new table.** FileMaker Pro makes one when the new
  table has fields; OpenMaker does not.
- **Unverified parity.** Only the edits listed in schema-path.md 5.8 and
  5.9 were compared with FileMaker Pro's own saves. The rest is inferred
  from them, in particular:
  - the name given to a new table's occurrence when an occurrence already
    has the table's name;
  - for field options:
    - removing a serial number, data or a validation option other than
      Not empty;
    - global storage and repetitions on a field made in the same edit;
    - a type or repetitions change of an existing field of an empty
      table, written as for any existing field, with the host's work of
      7.2 (from bin), but not compared with a FileMaker Pro save;
    - the deferred work bits sent to the host: OpenMaker sends `0x200`,
      plus `0x10` to build indexes, `0x20` to drop them, or both to discard
      an old index before rebuilding a reduced set of index kinds.
  - What FileMaker Pro itself does with the values of a field whose global
    storage changes, beyond the host's work of 7.2 (from bin): whether it
    sets the new global from the current record is not measured
    (schema-path.md 12, item 11).
  - FileMaker Pro writes the next serial value `0c` when its Auto-Enter
    tab was shown, and `26` when its Storage tab was. OpenMaker always
    writes `0c` and never writes `26`.
  - Calculated values, value lists, ranges, validation calculations and
    custom messages were compared with FileMaker Server's engine (its
    importer, schema-path.md 5.13), not with FileMaker Pro's dialog.
    FileMaker Pro 26's dialog writes every custom message as a calculation
    (`0x8080`, `20`, and `18` with a text for older versions, E34).
    OpenMaker writes a typed message as text (`0x80`, `18`), the form the
    engine's importer writes, and a message calculation in that dialog's
    form; neither OpenMaker form was compared with a file FileMaker Pro
    wrote for the same message.
  - A lookup's dependency entries were compared with the engine's (l01);
    its `08` takes the padding of FileMaker Pro's own two samples rather
    than the engine's, and FileMaker Pro's dialog was not run for it.

  Whether FileMaker Pro opens files OpenMaker changed this way has not been
  checked.

## Validation

- `cargo test -p om-data --lib schema_draft`: each edit against the
  catalogs FileMaker Pro wrote for it (schema-path.md 5.8), refusals that
  leave the draft unchanged, and a table made and deleted in one session;
  and the calculation fields of engine runs c01-c20 made through the
  draft, compared key by key with the field catalog and dependency entry
  the engine wrote (`tests/fixtures/contacts-calculations.txt`).
- `cargo test -p om-gui --test database`: staging, Cancel and OK on the
  sample provider; and on an OpenMaker Server on loopback a new table and
  field, a new, a renamed and a deleted field saved by OK, the schema lock
  refusing a second editor meanwhile, and a fresh connection reading the
  changes and the records back; and Options for Field (data, Not empty and
  full indexing on Tasks::Name; a serial, a range, a validation
  calculation and a custom message on a new number field; a calculated
  value written in Specify Calculation on a new text field; a message
  calculation on Tasks::Name) saved the same way and read back, and a
  write emptying Name refused with that calculation's text.
- `cargo test -p om-data --lib field_options`:
  - the definitions FileMaker Pro wrote in E11-E43 decode to the options
    set and encode back byte for byte;
  - the draft writes the dependency entry, `fff0`, the definition and
    `0c`/`0d` FileMaker Pro wrote in E16, E30, E33, E41 and E43
    (`tests/fixtures/contacts-field-options.txt`), and refuses what it
    cannot save.
- `cargo test -p om-data --lib option_formulas_are_written`: the
  calculated values and validation options of engine runs a01-a11 and
  v01-v24 made through Create and Options for Field, compared key by key
  with the field catalog and dependency entry the engine wrote
  (`tests/fixtures/contacts-option-formulas.txt`), and the formulas
  OpenMaker's record rules could not evaluate refused.
- `cargo test -p om-data --lib related_option_triggers`: after the
  engine's runs a04, a05, a12-a14 and l01-l04, rebuilding the Contacts and
  Phone Numbers entries with the triggers derived from `fb` nodes gives
  back what the engine wrote (and without them it does not).
- `cargo test -p om-data --lib lookups_are_written`: run l01's lookup made
  through Create and Options for Field, compared key by key with the
  engine's (with `08` in FileMaker Pro's form and `0c`); l02-l04 refused;
  the relationship, occurrence and copied field kept from deletion.
- `cargo test -p om-gui --test database a_lookup_is_set_on_an_openmaker_server`:
  a lookup set through Lookup for Field and saved on an OpenMaker Server
  on loopback, read back, and performed by a write of its match field.
- `cargo test -p om-data --lib related_formulas_are_written`: runs a15
  (an auto-entered `Contacts::Company & "!"` in Phone Numbers) and v28 (a
  validation by `Contacts::Company ≠ ""`) made through Create and Options
  for Field, compared key by key with the engine's; an auto-entered
  `Count ( Contacts::Company )` compared with a15 as a05 differs from a04
  (its input typed `00`); reads through two relationships refused.
- `cargo test -p om-gui --test database
  a_related_calculated_value_is_set_on_an_openmaker_server`: a calculated
  value `Notes::Body` written in Specify Calculation and saved on an
  OpenMaker Server on loopback, read back, and evaluated by a write of
  its match field; a validation by `Notes::Body ≠ ""` refusing a write
  with nothing related (507).
- `OM_RELATED_FORMULA_FILE=<copy of the Contacts starter solution> cargo
  test -p om-provider --test local_files -- --ignored related_formulas`
  gives Phone Numbers both formulas over Contacts::Company for
  FileMaker's tools to check.
- `OM_LOOKUP_FILE=<copy of the Contacts starter solution>
  OM_ASSETS_FILE=<copy of the Assets starter solution> cargo test -p
  om-provider --test local_files -- --ignored lookups_are_made` adds a
  lookup to Phone Numbers and a creation value beside Assets'
  Assignments lookup; FileMaker Server's `FMDeveloperTool
  --checkConsistency` finds no bad block in either copy.
- `OM_OPTIONS_FILE=<copy of the Contacts starter solution> cargo test -p
  om-provider --test local_files -- --ignored field_options` gives a local
  copy a calculated value, a value list, a validation calculation with a
  message and an Always range; FileMaker Server's `FMDeveloperTool
  --checkConsistency` finds no bad block in it and its Save as XML reads
  each option back.
- `cargo test -p om-data --lib global_storage_changes`: Website made
  global asks for `0x241` and raises its `fb` to `01 01`; a global made
  stored asks for `0x243`, turned back asks for nothing, and deleted then
  asks for its global value's deletion (`0x1e3`); First Name, which
  Initial reads, is refused.
- `cargo test -p om-gui --test database global_storage_is_changed`: on an
  OpenMaker Server on loopback, Setting (global, "on", with a second
  repetition) made stored and Name made global in the Storage tab; after
  the save both global cells and Name's record values are gone.
- `cargo test -p om-data --lib summary_fields_are_written`: the summary
  fields of engine runs s01-s16 made through the draft, compared key by
  key with what the engine wrote; s03 and s10 refused.
- `cargo test -p om-gui --test database
  a_summary_field_is_made_on_an_openmaker_server`: a running total made
  through Options for Summary Field on an OpenMaker Server on loopback,
  read back by a fresh connection.
- `cargo test -p om-gui --test database
  a_calculation_field_is_made_and_changed_on_an_openmaker_server`: a
  calculation field made and its formula changed through Specify
  Calculation on an OpenMaker Server on loopback, read back by a fresh
  connection, with its values computed in every record.
- `OM_CALCULATION_FILE=<copy of the Contacts starter solution> cargo test
  -p om-provider --test local_files -- --ignored calculation_fields` makes
  three calculation fields in a local copy, for FileMaker's tools to open
  (the script harness's Save as XML and a probe script computed them).
- `cargo test -p om-data --lib relationship_sort`: a relationship's two
  sort orders written from their meaning equal the bytes FileMaker's
  engine stored for the same orders (file-metadata.md 4.1), and changing
  one side's order keeps the other's.
- `OM_RELATIONSHIP_FILE=<copy of the Contacts starter solution> cargo test
  -p om-provider --test local_files -- --ignored relationships_are_made`
  makes an occurrence, a two-pair relationship with a sort, changes and
  deletes relationships in a local copy; FileMaker's `FMDeveloperTool
  --checkConsistency` found no problems in it and its Save as XML reads the
  relationships, options and sorts back (likewise for the calculation
  fields of `calculation_fields_are_made`).
- `cargo test -p om-data --lib deletions_are_written`: from FileMaker's own
  files, a calculation, an entry option with a validation, a summary, a
  validation calculation, two auto-entered calculations, a lookup and a
  field validated Always deleted through the draft leave the field catalog and the dependency
  entry key for key as FileMaker's engine left them (schema-path.md 5.15),
  and the fields the engine kept are refused.
- `OM_DELETE_FIELDS_FILE=<copy of the Contacts starter solution> cargo
  test -p om-provider --test local_files -- --ignored
  fields_with_options_are_deleted` writes a record, deletes Initial and
  CreatedBy, makes a calculation and a summary field, and in a second
  session deletes them; FileMaker's `FMDeveloperTool --checkConsistency`
  found no problems in it, its Save as XML lists the table without the
  four fields, and the record keeps its other values.
- `cargo test -p om-author --test fields`: a staged field saved by OK, the
  lock held until then, refused names leaving the file unchanged.
- `cargo test -p om-provider --lib schema::tests`: bounded relationship
  keys and relationship identity checks.
- `cargo test -p om-hbam --lib catalog`: occurrence base tables, and graph
  boxes decoded from the Contacts starter solution's bytes with malformed
  tails refused.
- `cargo test -p om-gui --lib database_graph`: the key-field rows a box
  draws stay bounded with thousands of key fields.
- `OM_RELATIONSHIPS_FILE=<copy.fmp12> OM_SCREENSHOTS=<dir> xvfb-run -a
  cargo test -p om-gui --test database -- --ignored
  render_relationships_of_a_file` hosts a copy of a file and renders its
  Relationships tab, to compare with FileMaker Pro's graph of the same
  file.
- `cargo run -p om-provider --example schema_catalog -- <file.fmp12>`
  prints a local file's tables, fields, occurrences and relationships
  through the same decoder as native downloads.
