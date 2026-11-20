# Agent authoring of FileMaker solutions

`om-author` lets a program — typically an AI coding agent — develop a
FileMaker solution through structured commands instead of driving the
desktop UI: inspect a hosted file's metadata, fetch a script as editable
text, validate and plan an edit against the file's names, and apply it with
conflict protection, reading the result back over a new connection; plan
and add a field to a table, add a layout, and place a field on a layout,
the same way;
and run a script on the host to see what an edit does. The same service
(`om_author::Service`) answers a command-line tool and an MCP server; both
print the same versioned JSON.

In files hosted by **OpenMaker Server**, this version edits **script
steps**; adds **stored fields** (text, number, date, time or timestamp) to
existing tables; adds, duplicates and deletes **Form layouts** and changes
their Layout Setup, width and parts; **places fields** (an edit box, with
an optional text label), texts and portals, moves, resizes and deletes
objects, and changes a field's control and a text's words; changes a
**custom value list's values**; and **runs scripts** on the host with
Perform Script on Server. `create-file` makes a blank file on this
computer without a connection. Everything else is read-only; see
[Limits](#limits).

## Commands (`openmaker.authoring/v1`)

A request is JSON:

```json
{"version": "openmaker.authoring/v1", "command": "plan",
 "params": {"script": "script:5@Tasks.fmp12", "source": "...", "expected_revision": "sha256:..."}}
```

and every response is one JSON object:

```json
{"version": "openmaker.authoring/v1", "command": "plan", "ok": true, "result": {...}}
{"version": "openmaker.authoring/v1", "command": "apply", "ok": false,
 "error": {"kind": "conflict", "message": "...", "details": {"current_revision": "sha256:..."}}}
```

| command | params | result |
|---------|--------|--------|
| `commands` | none | every command: `id`, `title`, `description`, `writes`, its `effect` (`reads`, `authors`, `runs`, `edits` or `creates`), `needs` (`nothing`, `file`, `grant`, with the `grant`: `script_steps`, `fields`, `layouts`, `new_layouts`, `layout_objects`, `object_deletion` or `texts`, `layout_copies` or `layout_deletion`, or `window`), the `operation` in `capabilities` that says whether it is available, `cli` and `mcp_tool` names, and `params` as JSON Schema. Needs no connection |
| `capabilities` | none | host product, the account, each operation with `available` and, when not, `kind` (`unsupported`, `permission_denied`) and `reason`; size limits |
| `inspect` | `include` (optional: `scripts`, `tables`, `occurrences`, `layouts`, `value_lists`, `data_sources`) | the file's metadata, each object with a file-qualified `ref`, and with `layouts` the `layouts_revision` `create_layout` names. Never reads records |
| `get_script` | `script` or `script_name` | the script's summary and `revision`, its `source` text, each step (line, depth, step id and name, `fidelity`), what is carried opaquely, whether `apply` is available |
| `validate` | `script`/`script_name`, `source`, optional `expected_revision` | `valid`, step counts (kept, new, deleted) or `diagnostics` with line, step and text |
| `plan` | as `validate` | `base_revision`, `after_revision`, `changed`, `already_applied`, a unified `diff` of the canonical text, `after_source`, step counts. Writes nothing |
| `apply` | `script`/`script_name`, `source`, `expected_revision` (required), optional `expected_after_revision` | `status` (`applied`, `already_applied`, `unchanged`), `previous_revision`, `revision`, and `verified` from a new connection |
| `plan_field` | `table` or `table_name`, `name`, `type` (`text`, `number`, `date`, `time`, `timestamp`), optional `comment` and `expected_revision` | the table, `base_revision`, `already_applied`, the `field` (the reference and id the host would give it now, its definition and defaults), the catalog `changes`, whether `create` is available. Writes nothing |
| `create_field` | as `plan_field`, `expected_revision` required | `status` (`created`, `already_applied`), the `field` and its `ref`, `previous_revision`, `revision`, and `verified` from a new connection |
| `get_layout` | `layout` or `layout_name` | the layout's `revision`, occurrence and table, `width`, `parts`, each object (`ref`, key, kind, `frame`, place in the root `stacking`, parent, name, UUID; a field object's `field` and `control`; a label's `text`), the `root_stack` back to front, whether `place_field` is available |
| `plan_field_placement` | `layout`/`layout_name`, `field` or `field_name` (a field of the layout's table), `frame` (`left`, `top`, `width`, `height` in points), optional `label` (`text`, `frame`) and `expected_revision` | `base_revision`, `already_applied`, the `objects` with the keys the host would give them, the `part`, `stacking`, the objects it `overlaps`, the catalog `changes`, whether `place` is available. Writes nothing |
| `place_field` | as `plan_field_placement`, `expected_revision` required | `status` (`placed`, `already_applied`), the `objects` and their refs, `previous_revision`, `revision`, and `verified` from a new connection |
| `place_text` | `layout`/`layout_name`, `text` (one line), `frame` (`left`, `top`, `width`, `height` in points), `expected_revision` (get_layout's, required) | `status` (`placed`, `already_applied`), the `layout`, the text's `object` reference, `previous_revision`, `revision`, and `verified` from a new connection. [Texts on layouts](#texts-on-layouts) |
| `set_object_frames` | `layout`/`layout_name`, `objects` (each a `layout_object` reference and its new `frame`: `left`, `top`, `width`, `height` in points), `expected_revision` (get_layout's, required) | `status` (`moved`, `already_applied`), the `layout`, the `objects`, `previous_revision`, `revision`, and `verified` from a new connection. [Moving objects](#moving-objects) |
| `arrange_objects` | `layout`/`layout_name`, `objects` (`layout_object` references: two or more, three to distribute), `arrangement` (`align_left_edges`, `align_vertical_centers`, `align_right_edges`, `align_top_edges`, `align_horizontal_centers`, `align_bottom_edges`, `distribute_horizontally`, `distribute_vertically`, `resize_to_smallest_width`, `resize_to_smallest_height`, `resize_to_smallest_width_and_height`, `resize_to_largest_width`, `resize_to_largest_height`, `resize_to_largest_width_and_height`), `expected_revision` (get_layout's, required) | `status` (`moved`, `already_applied`, `unchanged`) and the `arrangement`. A write returns `set_object_frames`'s result fields; an arrangement that changes nothing returns the `layout` and its `revision`. [Arranging objects](#arranging-objects) |
| `delete_objects` | `layout`/`layout_name`, `objects` (`layout_object` references), `expected_revision` (get_layout's, required) | `status` (`deleted`, `already_applied`), the `layout`, the `objects`, `previous_revision`, `revision`, and `verified` from a new connection. [Deleting objects](#deleting-objects) |
| `create_layout` | `name`, `occurrence` or `occurrence_name` (the table occurrence it shows records of), optional `layout`/`layout_name` (whose theme and width it takes), `expected_revision` (inspect's `layouts_revision`, required) | `status` (`created`, `already_applied`), the `layout` (`ref`, `id`, `name`), the `occurrence`, `previous_revision`, `revision`, and `verified` from a new connection. [New layouts](#new-layouts) |
| `duplicate_layout` | `layout`/`layout_name`, `expected_revision` (inspect's `layouts_revision`, required) | `status` (`created`, `already_applied`), the copy's `layout` (`ref`, `id`, `name`), the `source`, `previous_revision`, `revision`, and `verified` from a new connection. [Duplicating layouts](#duplicating-layouts) |
| `place_portal` | `layout`/`layout_name`, `occurrence`/`occurrence_name` (related to the layout's), `field_names` (1 to 20 of that occurrence's table), `rows` (1 to 100), `frame` (`left`, `top`, `width`, `height` in points), `expected_revision` (get_layout's, required) | `status` (`placed`, `already_applied`), the `layout`, the `portal`'s and its `fields`' references, `previous_revision`, `revision`, and `verified` from a new connection. [Portals on layouts](#portals-on-layouts) |
| `change_parts` | `layout`/`layout_name`, `expected_revision` (get_layout's, required), `action` (`add`, `delete`, `resize`), `part` (get_layout's part `id`, for `delete` and `resize`), `kind` (for `add`: `title_header`, `header`, `leading_grand_summary`, `body`, `trailing_grand_summary`, `footer`, `title_footer`, `top_navigation`, `bottom_navigation`), `height` (whole points, for `add` and `resize`) | `status` (`changed`, `already_applied`), the `layout`, its `parts` read back, `previous_revision`, `revision`, and `verified` from a new connection. [Parts](#parts) |
| `set_text` | `object` (get_layout's reference of a text or a field's label), `expected_revision` (get_layout's `revision`, required), `text` (one line of plain text) | `status` (`set`, `already_applied`), the `layout`, the `object`, the `text`, `previous_revision`, `revision`, and `verified` from a new connection. [Texts on layouts](#texts-on-layouts) |
| `set_part_options` | `layout` or `layout_name`, `expected_revision` (get_layout's `revision`, required), `part` (a part's `id` from get_layout: a body or a sub-summary), `options` (`page_break_before`, `restart_page_numbers`, `break_across_pages`, `discard_remainder`, each `true` or `false`; others keep their state), `page_break_after` (1 to 32,767, 0 for none), `sorted_by` (a leading sub-summary's field, of the layout's own table, as `field:TABLE.FIELD@File`) | `status` (`set`, `already_applied`), the `layout`, the `part`, its `options` and `page_break_after`, `previous_revision`, `revision`, and `verified` from a new connection. [Part options](#part-options) |
| `set_field_control` | `object` (get_layout's field object reference), `expected_revision` (get_layout's `revision`, required), `control` (`edit_box`, `drop_down_list`, `pop_up_menu`, `checkbox_set`, `radio_button_set`, `drop_down_calendar`), `value_list` (get_value_lists' reference; required for the four list controls, left out to keep the object's) | `status` (`set`, `already_applied`), the `layout`, the `object`, the `control`, `previous_revision`, `revision`, and `verified` from a new connection. [Field controls](#field-controls) |
| `set_layout_width` | `layout`/`layout_name`, `expected_revision` (get_layout's `revision`, required), `width` (points) | `status` (`set`, `already_applied`), the `layout`, its `width`, `previous_revision`, `revision`, and `verified` from a new connection. [Layout width](#layout-width) |
| `layout_setup` | `layout`/`layout_name`, `expected_revision` (get_layout's `revision`, required), and any of `name`, `views` (some of `form`, `list`, `table`), `default_view`, `include_in_layout_menus` | `status` (`set_up`, `already_applied`), the `layout`, its `name`, `views`, `default_view` and `include_in_layout_menus`, `previous_revision`, `revision`, and `verified` from a new connection. [Layout Setup](#layout-setup) |
| `delete_layout` | `layout`/`layout_name`, `expected_revision` (inspect's `layouts_revision`, required) | `status` (`deleted`, `already_applied`), the `layout`, `previous_revision`, `revision`, and `verified` from a new connection. [Deleting layouts](#deleting-layouts) |
| `run_script` | `script`/`script_name`, optional `parameter` (text) and `expected_revision` | `status` (`completed`), the `script` and its `name`, the `revision` it ran at, `last_error` (the host's answer: the script's final Get ( LastError ), or its refusal) and `result` (the Exit Script text). [Running scripts](#running-scripts) |
| `list_choices` | `object` (a `layout_object` reference), `record` (the record's id) | the `object`, the `record`, the `value_list` and each item's `value` (what choosing it stores) and `label` (what the control shows), with dividers marked; or the reason the list cannot be read. Reads the records the list's values come from. [Value-list choices](#value-list-choices) |
| `choose_value_list_item` | as `list_choices`, and the item's `value` | always the typed error `unsupported` with `details.unavailable` = `no_window` in this version. [Value-list choices](#value-list-choices) |
| `enter_other_value` | as `list_choices`, and the `text` to enter | always the typed error `unsupported` with `details.unavailable` = `no_window` in this version: a pop-up menu's Other… (#869) needs a desktop window. [Value-list choices](#value-list-choices) |
| `get_value_list_values` | `value_list` (a reference from `get_value_lists`) | the list's `name`, its `values` in order (`-` a divider) and its `revision`; a list whose values come from a field is refused (`unsupported`). [Value lists](#value-lists) |
| `set_value_list_values` | `value_list`, `values` (one item each, `-` a divider, no item holding a return), `expected_revision` (get_value_list_values' `revision`, required) | `status` (`set`, `already_applied`), the `value_list`, its `values`, `previous_revision`, `revision`, and `verified` from a new connection. [Value lists](#value-lists) |
| `get_security` | none | the file's `accounts` (record `key` in hex, `name`, `guest`, `type`, `active`, `must_change_password`, `privilege_set` and its name), `privilege_sets` (`key`, `name`, `record_flags` in hex, `record_access` for the observed flags only, `password_days`, its `accounts` and `extended_privileges`) and `extended_privileges` (`key`, `keyword`, its `privilege_sets`); `null` for anything the host could not read; `editable` false with `not_editable_reason`. Never a password, hash or other credential. [Reading security](#reading-security) |
| `get_value_lists` | none | the file's value lists in display order (`om_data::value_lists`, the catalog File > Manage > Value Lists shows): each list's `ref`, `id`, `filemaker_id` (the id FileMaker shows), `name` and `source`: `custom` with its `values`; `field` with the `field`, `second_field`, `related_from` occurrence, `sort_by_second_field` and `show_second_field_only`; `external` with the stored `file_reference` and `list_key`, unresolved; `calculation`; or `unreadable` with the reason. Decoded lists add `sort_language` and `unknown_options`. `editable` is `false` and `read_only` gives the reason. Never reads records |
| `complete_calculation` | `text` (formula text), optional `cursor` (characters; default the end), `occurrence` or `occurrence_name` (the context; default the first occurrence), `limit` (default 50, at most 1000) | the `prefix` typed at the cursor, the characters it `replace`s, the `candidates` best first (`kind`, `name`, `label`, the text to `insert`, `selection_after`, a function's `category`, a field's `field` and `occurrence` refs), `total`, `truncated`, `refused` when nothing is completed, and each name source's availability. Reads metadata only. [Calculation name completion](#calculation-name-completion) |
| `create_file` | `path` (on this computer; `.fmp12` added when missing) | the new file's `path`, its `name` (the file name without the extension, also its table's and layout's), and the `account` (`Admin`) and `password` (empty) that open it. Makes a blank file as File > Create New... does ([docs/new-files.md](new-files.md)); never replaces a file (`duplicate_name`); a name the table or layout cannot have is `invalid_name`, a failed write `save_failed`, with nothing left. Needs no connection |

**References** are `kind:id@file` with the file's own ids:
`script:5@Tasks.fmp12`, `table:32769@Tasks.fmp12`,
`field:32769.2@Tasks.fmp12`, `occurrence:13631489@Tasks.fmp12`,
`layout:1@Tasks.fmp12`, `value_list:3@Tasks.fmp12`, and a layout's
objects `layout_object:1.4@Tasks.fmp12` (layout 1, object key 4). A
reference naming
another file than the one connected to is refused (`wrong_file`); it is
never resolved to an object of this file with the same id. A name
(`script_name`) must name exactly one script: exact match first, then
ignoring case, as FileMaker matches names; a shared name is
`ambiguous_reference` with the candidates' references; `table_name` names
a table the same way, `layout_name` a layout and `field_name` a field of
the layout's table. Occurrences of another file's tables have no `table`
and name their `data_source`.

**Revisions** are `sha256:` of a script's stored definition `[17, 5, S]`
in a canonical form, leaving out its modification stamp (`fc`, `fc 01`
to `fc 03`): a save that stores the same content keeps the revision. A
table's `fields_revision` (`inspect`) is the same over its whole field
catalog `[T, 3]`, leaving out every stamp in it: any field added, renamed,
changed or deleted, by anyone, changes it. Each table's listed fields and
revision come from the same downloaded catalog. A concurrent edit leaves
that observation stale together, so its token detects the change. This
is not a transaction covering all metadata in the inspection. A layout's
`revision` (`get_layout`) is the same over its whole catalog member
`[4, 5, L]` (definition, parts, objects, object attributes and the
layout's indexes), leaving out every stamp in it.

**Errors** (`error.kind`): `invalid_request`, `connection`, `not_found`,
`wrong_file`, `ambiguous_reference`, `unsupported`, `permission_denied`,
`conflict`, `locked`, `invalid_source`, `invalid_name`, `duplicate_name`,
`outcome_unknown`, `save_failed`, `verification_failed`. All but the last
two mean nothing was written. `outcome_unknown` means the edit was sent and
no answer came back: read the script or table again — a script is at
`after_revision` if the edit was applied, at `previous_revision` if not —
and do not resend blindly. A `plan` or `apply` of the same text then
reports `already_applied` without writing, and so does a `create_field` of
the same field from the same revision.

### Reading security

`get_security`, and File > Manage > Security on the desktop, read the
file's security catalog `[23]` through OpenMaker Server's Describe
Security (operation 5, below). The host decodes it and sends only these
facts:
- for each account (`[23, 1, 5, A]`), its name (key 16), kind `02`, status
  `0a` (active, and "must change password") and privilege set `0b`;
- for each privilege set (`[23, 2, 5, P]`), its name (key 16), record flags
  `02` and password lifetime `0c`;
- for each extended privilege (`[23, 3, 5, E]`), its keyword (key 16) and
  the sets that have it (`05`).

It never reads an account's password verifier `06` or its undecoded `04`,
so neither can reach a client, a result or a log. What the host cannot read
is `null`, never a default. Record access is named only for the observed
first flag bytes: `55` create, edit and delete; `54` create and edit; `10`
view only (docs/re/session-identity.md 2). Other values give the flags in
hex with no reading.

Only an account of the file itself, in the file's verified built-in [Full
Access] set (docs/re/local-files.md 2.3), may read it. Any other account is
`permission_denied` (status 9), and a session without the file open is
status 100. Another host, FileMaker Server included, is sent nothing:
`unsupported`. Accounts and privilege sets cannot be created, changed or
deleted here. How FileMaker writes an account is specified
(docs/re/local-files.md 2.4; whether FileMaker accepts OpenMaker's writes
is [#12](https://github.com/OpenMaker-Dev/openmaker/issues/12)), but a
set's checksum is not (#474). The rest of a set's members are specified
(docs/re/session-identity.md 2) but Describe Security does not read them
yet: set descriptions, per-table record and field access, layout, script
and value list access, the other privileges and menu commands.
`capabilities` lists `security` as unavailable for that reason.

### One description per command

Each command is described once, in `om_author::catalog`, and every way in
reads that description: the request parser, the CLI, the MCP tool list
(each tool's title, description and input schema), `capabilities` and the
service's checks before a write. Each command's `effect` says what it can
change: `reads` nothing, `authors` the file's design, `runs` a script, or
`edits` a record's field as a user's choice in a window does.
A command that authors needs the host's grant (`script_steps`, `fields`,
`layouts`, `new_layouts`, `layout_objects`, `object_deletion`, `texts`, `layout_copies` or `layout_deletion`): only OpenMaker Server's authoring extension gives one, to
an account allowed to author, so on any other host, FileMaker Server
included, `capabilities` reports the operation unavailable and the
command answers `unsupported` without sending anything. MCP tools are
marked read-only only for `reads`; `run_script`'s tool is marked
destructive, not idempotent and open-world, so a client does not retry it
on its own. `commands` returns the descriptions with each command's
parameters as JSON Schema, without a connection, so a program using the
CLI or `request` can learn the commands the way an MCP client learns them
from the tool list. Results are not described there yet; this page
describes them.

### Script source

The text is the Script Workspace's copy form: one step per line, a tab per
block level, `// ` before a disabled step, `# ` before a comment. A step
whose visible text leaves something out carries it in `Stored …` options
(`fidelity: "extras"`), or whole as `Stored step` (`"raw"`), so the text
reads back to the same steps. Leave those options as they are when the
step is not being changed.

Compiling lines the edited text up with the script's own step texts. A
line that is unchanged keeps its step exactly as stored (data key, data,
item UUIDs); any other line is read as a new step with the file's names
and locale, through the parsers the Script Workspace uses
(`om_scripts::display`, `om-formula`). Text that is this file's own may
carry its `Stored …` ids; other text may only name objects, so a step
copied from another file never binds to whatever has the same ids here.
Steps must nest (If/End If, Loop/End Loop, transactions). Problems come
back as `diagnostics` with the line each is on. Step XML is not read in
this version. Limits: 8 MiB of text and 20,000 steps, as for a paste.

### New fields

`create_field` adds a **normal field with stored values** of a scalar
type, with FileMaker's defaults: no auto-enter or validation, indexed as
needed ("Automatically create indexes as needed"), one repetition, the
index language most of the table's fields have (English in a table without
fields), and the optional comment. The name must be one a calculation can
name without `${…}`: at most 100 characters, no operators or other
delimiters, no leading digit, period or `$`, no space at either end, no
`and`/`or`/`xor`/`not` word, not a function's name, no control characters
(`invalid_name`), and one whose name list key OpenMaker can build
(re/schema-path.md 5.6; the names it cannot are in schema-manager.md,
"What it refuses"). It must not have the primary weights of another of the
table's fields, so neither case nor accents tell them apart, nor what
FileMaker's catalog language equates (`ı` and `i`)
(`duplicate_name`, naming the field that has it): FileMaker's keys tell
accents apart, and whether FileMaker Pro takes `Grosse` beside `Größe` is
not established.

The host does what Manage Database's save of one new field stores
(re/schema-path.md 2.3, 5.1, 5.5, 5.6, 8.1), in the table's field catalog
`[T, 3]` only:

- the member `[T, 3, 5, F]`: the 26-byte definition `02`, the comment
  `03` if any, the name `10`, two fresh UUIDs `d8`, `d9`, and `fc 01` to
  `fc 03` (a new member has no `fc`);
- its id `F`: the serial key after the larger of the index's last issued
  id `[T, 3, 1, 00]` and every member's (file-metadata.md 1; references
  give it as the key read big-endian, as for every field);
- in the index `[T, 3, 1]`: the last issued id, the name in the name list
  `01` and in the name list by type `06` (when the index has one), the
  next display position `03`, the member count `04` plus one, and the
  index's stamp with `fc` counted up.

Every other key of the catalog, known or not, is kept byte for byte;
`create_field` checks the other fields again on the read-back. A catalog
without an index, or whose index lacks its name list, display order or
count while it has fields, is refused (`unsupported`): every file FileMaker
made has them, and OpenMaker does not rebuild them. A new field holds no
values, so no record work is listed for op73 (re/schema-path.md 7.2), and
calculations, layouts and scripts are not changed.

`plan_field` runs the same planning code
(`om_data::field_catalog`) against the catalog as it is: the id it shows is
the one the host gives unless the table's fields change first. The host
plans again under its locks and answers with the id it gave.

### What an apply changes

Only the script's step list `04` and step data `05`, plus the
modification stamp the host writes. The script's id, name, folder,
"Include in menu", "Run script with full access privileges", every other
stored key (known or not) and step data no step uses are kept byte for
byte; `apply` checks them again on the read-back. New steps get fresh
step data keys above every key the script held.

### Fields on layouts

`place_field` puts an **edit box** showing a field on an existing layout,
on top of its objects, with an optional **text label**: what FileMaker
Pro's Layout mode stores when a field is dropped on a layout's body
(re/layout-model.md 5, 10.5). The field must be one of the table the
layout shows records of (not a related field) and not a container; the
layout must be a layout (not a folder or separator) with a definition,
not responsive, with a body part, and objects OpenMaker reads. Both frames
must lie inside the body part and the layout's width (Browse shows nothing
beyond it), not overlap each other, and not overlap a container — a group,
portal, tab or slide control, popover or button bar — whose contents
FileMaker Pro would make them. They may overlap simple objects (fields,
text, shapes), which stay beneath; the plan lists them (`overlaps`).

The host plans with `om_data::layout_placement`, as the client does, and
writes into the layout member `[4, 5, L]` only:

- the object store `07`: the new objects' `LayoutObject` encodings and
  root keys appended to the stored encoding, so every stored byte, unknown
  fields included, is kept and the new objects are on top of the root
  stack. The field object is type 1 with the edit box style type (61),
  the field's first repetition, one repetition shown, anchored left and
  top, and the other settings the templates' edit boxes have; the label
  is type 2 with the text box style type (70). Neither has a local style
  but the rotation FileMaker Pro writes, so the layout's theme styles
  them;
- an attribute record `[4, 5, L, 05, O]` for each object: an empty name
  `10`, fresh UUIDs `d8` and `d9`, and for the label its text `01` (a
  plain SCSU value, as the templates store a plain label) and a text
  height of 0 `0d`;
- the field reference index entry `[4, 5, L, 01, <occurrence>, <field>,
  <object>]` (an empty value, as every field object in the templates has);
- both objects at the end of the body part's list of its objects `[4, 5,
  L, 03, <body>, 07]`, its count raised and every stored entry kept. Every
  top-level object FileMaker saved is in its part's list, and FileMaker
  Pro's Browse did not show an edit box missing from it (issue #166,
  re/layout-model.md 10.5);
- the edit box's entry in the layout's object index `[4, 5, L, 0a, <top,
  left, n>]`: its frame's top and left, the number of objects before it in
  the object tree, and its key. Every field object FileMaker saved has
  one. The index holds whole points, so the field's frame must start at
  whole points (status 3 otherwise);
- the last object key issued `[4, 5, L, 05, 00]`, raised to the new
  objects' keys;
- where the layout stores a Table View layout `[4, 5, L, 0d]` (every
  layout OpenMaker makes does, #825, #839), the field's column there, as
  FileMaker Pro's Field tool writes it (re/layout-model.md 10.14): the
  empty column taken out, the field's column added with the next key and
  a new empty column after it, with their field references, body list,
  object index and tab order entries, and each with the 213-byte data
  format record Pro gives every column (the field object gets it too);
  the field's column repeats -1, as Pro's. Without the record, Pro empties
  the view when it saves the layout, then quits in Table View (#886).
  FileMaker Pro shows that table, so
  without the column the field is no column of it, and a layout with no
  stored table view shows a placed field as two columns (#815). A table
  view OpenMaker does not add to (an object that is not a top-level
  column, no body, a position between whole points) is kept as stored;
- the layout's stamp (`fc` counted up, `fc 01` to `fc 03`).

Object keys are the next after every key the layout uses (its objects,
root stack, containers' children, attribute records, index and part
lists) and after the last key it issued. A layout whose object tree does
not reach each of its objects once, whose object index already holds the
new edit box's position in that tree, or whose body part has no list of
its objects that OpenMaker reads, is refused (status 3). Nothing else is written: no
tab order, object name, data formatting, script trigger, conditional
formatting or hide condition, and no other layout index
(re/layout-model.md 10.5). Every other key of the layout, known or not,
every stored object's attribute record and every index entry are kept;
`place_field` checks them again on the read-back, decodes the layout as
the desktop client does (`om_layout::Layout::decode_from_tree`), and
checks that the client lists the new object among the layout's fields,
that the object index lists the edit box, and that the parts are as they
were but for the new objects at the end of the body's list.

A request repeated after a lost answer finds its objects on top of the
stack (the same field at the same frame, and the same label) and reports
`already_applied` without writing; any other placement from an old
revision is a `conflict`.

The desktop's Layout mode places a field the same way: its Field tool and
Save Layout call `DataProvider::place_layout_field`, which the native
provider implements with the same plan and the same `place_field` request,
at the revision of the layout the window showed ([desktop
client](desktop-client.md), "Layout mode and Manage Layouts"). Fields
placed from the window and from `place_field` read back the same way
(`get_layout`).

### Texts on layouts

`place_text` puts a **text object** holding one line of text on a layout's
body on its own, on top of its objects, as FileMaker Pro's Text tool
places one: the same text object, attribute record (its text, a text
height of 0, an empty name and fresh UUIDs) and entry at the end of the
body part's list that `place_field` writes for a field's label, the last
object key issued raised to its key, and the layout's stamp. A text has no
object index entry and shows no field, so nothing else changes
(re/layout-model.md 10.5). Its frame follows `place_field`'s rules: inside
the body part and the layout's width, over no container. A request
repeated after a lost answer finds the same text at the same frame on top
of the stack and reports `already_applied`; any other request at an old
revision is a `conflict`. The desktop's Text tool places a text the same
way at Save Layout (`DataProvider::place_layout_text`, the same request).
What FileMaker Pro's Text tool itself writes is not measured.

`set_text` changes a stored text's words, a field's label's included, as
typing into it in FileMaker Pro's Layout mode does (operation 20, planned
with `om_data::layout_placement::plan_words` and saved as an object edit
is: behind the layout's exclusive lock, its member stamped). Its attribute
record's `01` is written again as a placed text's is, every other key of
the record and the member as stored (re/layout-model.md 10.16). Refused:
an object that is not a text; words that are styled, hold a merge field
or are a calculation; new words that are empty, longer than 1,000
characters, or hold a line break, a merge field's `<<` or another control
character. A request the text already holds reports `already_applied`;
any other at an old revision is a `conflict`.

### Portals on layouts

`place_portal` puts a **portal** on a layout's body, on top of its
objects, as FileMaker Pro's Portal tool, Portal Setup (its defaults but
the number of rows) and Add Fields to Portal make one (operation 21,
planned with `om_data::layout_placement::plan_portal`; re/layout-model.md
10.17): the portal showing `rows` rows of a table occurrence the
relationship graph joins to the layout's own, directly or through others;
the fields named, of that occurrence's table, side by side in its first
row as its contents (4 points in and apart, at most 249 by 31 points
each, narrower and lower where the portal is); the portal's and each
field's references, attribute records with fresh UUIDs, the portal's
object index entry with its fields nested at the places they are drawn
(inside the portal's borders, from the layout's theme), the fields in the
tab order, the portal at the end of the body's list, a column per field
in the stored table view as for a placed field, the last key issued, and
the layout's stamp. Its frame follows `place_field`'s rules. Refused: the
layout's own occurrence or one not related to it, no field or more than
20, a container field, rows that leave no room for a field. A request
repeated after a lost answer finds the same portal on top and reports
`already_applied`; any other at an old revision is a `conflict`. Portal
Setup's other options (sorting, filtering, deletion, scroll bar,
alternate rows) stay at Pro's defaults, and the desktop has no Portal
tool yet.

### Moving objects

`set_object_frames` moves and resizes objects of a layout: each to its
new frame, all in one edit. It changes only what holds an object's position
(re/layout-model.md 10.7), planned with `om_data::layout_objects` as the
client plans it and written by the host under an exclusive lock on the
layout, in one journaled commit:

- the object's `bounds` in the object store `07`, rewritten where they are
  stored (`om_layout::encode::with_object_bounds`): the four edges change,
  and every other byte of the store, other objects and unknown fields
  included, stays as stored;
- the object's entry in the object index `0a`, keyed again at its new top
  and left, its place in the object tree and its value kept (an object the
  index does not list, such as a text, has none);
- the layout's stamp.

An object must be at the top level and hold no other objects (a field
showing one repetition, a text, a shape, a line, a button or a picture; not
a group, portal, tab or slide control, popover or button bar, or anything
inside one). Its new frame starts at whole points, stays inside the part
that lists it and the layout's width, has a size (a line keeps its empty
side), and does not overlap a container, which FileMaker Pro would make it
part of. A request repeated after a lost answer finds every object at its
frame and reports `already_applied`; any other request at an old revision
is a `conflict`. `set_object_frames` reads the layout back over a new
connection: at the revision the host reported, each object at its frame.

The desktop's Layout mode moves and resizes objects the same way
(`DataProvider::set_layout_object_frames`, the same request) at Save
Layout ([desktop client](desktop-client.md), "Layout mode and Manage
Layouts").

### Arranging objects

`arrange_objects` is Layout mode's Arrange > Align, Distribute and Resize
To:
- It reads the objects' stored frames.
- It gives them new frames with `om_data::layout_arrange`, as the
  desktop's Arrange menu does (re/client-ui.md, "Layout mode: Align,
  Distribute and Resize To").
- It writes them with `set_object_frames`, so the objects it accepts, the
  refusals, the `conflict` at an old revision, `already_applied` and the
  read-back are the same.

Every object named must be one `set_object_frames` can move, the ones that
stay where they are included, so an object that cannot be moved never
anchors the others. A frame with no size, a negative size or an edge that
is not a number is refused. A malformed `expected_revision` is an
`invalid_request`.

An arrangement that changes no frame (the objects are arranged so already)
writes nothing. It reports `unchanged` at the expected revision, and
`already_applied` at another one, as after a request whose answer was lost.
An arrangement it does not know, or too few objects, is an
`invalid_request`. On the command line, repeat
`--object` for each object and name the arrangement with
`--arrangement`.

### Deleting objects

`delete_objects` deletes objects of a layout, all in one edit. Each object
goes with every record the layout member holds for it
(re/layout-model.md 10.8), planned with `om_data::layout_objects` as the
client plans it and written by the host under an exclusive lock on the
layout, in one journaled commit:

- its definition and its root key in the object store `07`
  (`om_layout::encode::without_objects`); every other byte of the store,
  other objects and unknown fields included, stays as stored;
- its attribute record `05, <object>` (name, UUIDs, text, data formatting,
  tab position, script triggers and calculations);
- its entries in the field reference index `01`, with a field's node left
  without objects, and an occurrence's left without fields unless it is
  the layout's own;
- its entries in the named objects `08` and the tab order `15` (the other
  entries keep their numbers);
- its entry in its part's list `03, <part>, 07`, the count lowered;
- its entry in the object index `0a`, and every entry after it keyed again
  at its object's new place in the object tree, nested entries included;
- in the stored Table View layout `0d`, the columns of the fields no
  remaining field object shows, taken out with the empty column renewed,
  as FileMaker Pro does; a layout left with no field object and no `0d`
  gets the one a new layout stores, without which FileMaker Pro quits when
  it is shown as a table (#814, re/layout-model.md 10.14);
- the layout's stamp.

The last object key issued (`05, 00`) stays, so a deleted key is never
issued again. The objects are those `set_object_frames` moves (top-level,
without contents) that show no image from the file's library: FileMaker
counts each use of a library item and removes the item with its last use
(re/layout-model.md 10.8), which this version does not write, so a
button with an icon, a picture or an object with a library image in its
own styles is refused (status 3), and so are an object another one
names by key (a field's label, which the field names with `labelKey`,
unless the field is deleted in the same request) and a popover button,
whose popover would stay without one. A layout whose object index does not follow its object
tree, or one of whose parts' lists does not read, is refused (status 3):
every layout FileMaker saved has both. A request repeated after a lost
answer finds none of the objects and reports `already_applied`; any other
request at an old revision is a `conflict`. `delete_objects` reads the
layout back over a new connection: at the revision the host reported,
without the objects.

The desktop's Layout mode deletes objects the same way
(`DataProvider::delete_layout_objects`, the same request) at Save Layout.

### New layouts

`create_layout` adds a **Form layout for a computer** with one empty body
part, no objects and a stored Table View layout with an empty column,
showing records of the table occurrence named: what
FileMaker Pro's New Layout/Report makes for a computer with Form chosen
shows only in Form view (Claris, "New Layout/Report"). It takes the theme
and width of the layout named, or of the first layout in the file's order
that has a theme; its body is 600 points high. Layouts may share a name.
The name is listed under the key FileMaker Pro lists it under
(re/schema-path.md 5.6), and one whose key OpenMaker cannot build is
refused as an unusable name.
The part set, the body's height, the width and the theme are OpenMaker's
choices: what FileMaker Pro writes for them is not measured
(re/layout-model.md 10.6).

The host plans the layout with `om_data::layout_catalog`, as the client
does, and writes, in one journaled commit under the schema lock `[08]`
(write) and an exclusive lock on the layout catalog's index `[4, 1]`:

- the member `[4, 5, L]`: the `LayoutDef` and body `PartDef` FileMaker
  writes for a layout given only its occurrence, width and body height
  (`om_layout::encode::new_layout_def`, `new_body_part`), an empty object
  store and empty part, name, object and tab-order lists, the theme key,
  the name, the empty `17` and `21` every layout has, Form-only view flags
  `0x10f3080d`, the request's UUID as `d8` and a fresh `d9`, and the
  member's stamp `fc01`-`fc03` (no `fc`: it is new, re/schema-path.md
  8.1), and the stored Table View layout `0d` FileMaker Pro's Create New
  stores on its blank layout (`layout_catalog::table_view`,
  re/layout-model.md 10.14): without it, FileMaker Pro quits when a layout
  with no field object is shown as a table (#814);
- in the index `[4, 1]`: the last issued id, a name list entry (followed
  by the layout's id when the name is already listed, as FileMaker lists a
  second layout of a name), the next display position, the count, the
  layout's entry `07, L` (occurrence and menu set, name, `30`, kind and
  view flags) and the index's stamp counted up.

The id is the one after the index's last issued id and every member's; the
layout goes last in the display order, outside any folder. The name must
be 1 to 100 characters without line breaks or other control characters.
Each request carries a UUID, which becomes the layout's `d8`: a request
repeated after a lost answer finds the layout with its UUID and reports
`already_applied`, at any revision, and any other request at an old
revision is a `conflict`. `create_layout` derives the UUID from the call
and the revision it names, so repeating the same call is idempotent (two
agents making the same call from the same revision get one layout); the
desktop draws a new UUID for each Finish. `create_layout` reads the layout back over a
new connection: the index at the reported revision, the member decoded as
the desktop decodes it (the occurrence, one body part, no objects) and the
layout listed among the names calculations use.

### Duplicating layouts

`duplicate_layout` copies a layout as FileMaker Pro's Duplicate Layout
does (re/layout-model.md 10.9), planned with
`om_data::layout_catalog::plan_duplicate` and written by the host in one
journaled commit under the catalog's write lock, the index's exclusive
lock and an exclusive lock on each library item it counts:

- the copy's member `[4, 5, C]`: the source's, every key and byte as
  stored (definition, parts, object store, attribute records, indexes,
  theme, view flags, unknown keys), with the source's name followed by " Copy", the
  request's UUID as `d8`, no `d9`, each object's `d8` new (its `d9` kept),
  in the stored table view `0d` too, and the source's stamp with who copied it when (its count kept, as
  FileMaker Pro's copy keeps it);
- in the index `[4, 1]`: the last issued id, the name list entry, the next
  display position (last, outside any folder), the count, the copy's entry
  `07, C` (the source's, with the copy's name), and the index's stamp
  counted up;
- for each library item the copy's objects show (a button's, popover
  button's or graphic's `libraryKey`), its use count `[31, 5, N, 12]`
  raised by the number of objects showing it, and the item's stamp count
  `fc` raised by the same number (its user and time kept), as FileMaker
  Pro's copy raises them.

A folder or separator is refused, and so is a layout whose own styles,
parts' styles or objects' styles show a library image, or whose stored
table view's objects show any library item (status 3): how FileMaker
counts those uses is not known. Every use count and stamp is worked out,
overflow checked, before anything is written. The UUID is the request's
identity, as for `create_layout`: derived from the call and its revision,
so a repeat reports `already_applied`; any other request at an old
revision is a `conflict`. `duplicate_layout` reads the copy back over a
new connection: the index at the reported revision and the copy found by
the request's UUID, with the name it is stored under (a repeat after the
copy was renamed reports the new name). The desktop's Layouts > Duplicate Layout and Manage Layouts'
Duplicate make the same request (`DataProvider::duplicate_layout`).

### Deleting layouts

`delete_layout` deletes a layout as FileMaker Pro's Delete Layout does
(re/layout-model.md 10.10), planned with
`om_data::layout_catalog::plan_delete` and `om_data::library_uses::release`
and written by the host in one journaled commit under the catalog's write
lock, the index's and the layout's exclusive locks and each library
item's:

- the member `[4, 5, L]` removed;
- in the index `[4, 1]`: its name list entry, its display position (the
  later positions move up one), its entry `07, L`, the count lowered and
  the index's stamp counted up; the last issued id stays, so the id is not
  issued again;
- for each library item its objects showed, the use count lowered by the
  number of them and the item's stamp count raised by as many; an item
  left with no use is removed, with its library index entries (`06, N`,
  `07, N`, `08, <key>, N`), the library index's count lowered and its stamp
  counted up.

Scripts, buttons and the file's options that name the layout keep naming
it, as FileMaker Pro's deletion keeps them. A folder or separator, the
file's last layout, a layout whose styles show a library image and one
whose stored table view `0d` shows a library item are refused (status 3).
No client holds the layout's lock while it edits the layout: a Layout mode
draft in another client is not protected from the deletion ([#217](https://github.com/OpenMaker-Dev/openmaker/issues/217)). The request's identity is the layout: ids are not
issued again, so a repeat that names it by its reference after a lost
answer finds it gone at another revision and reports `already_applied`;
another layout's deletion at an old revision is a `conflict`.
`delete_layout` reads the layout list back over a new connection. The
desktop's Layouts > Delete Layout... and Manage Layouts' Delete make the
same request (`DataProvider::delete_layout`) after FileMaker Pro's alert.

The desktop's New Layout/Report adds a layout the same way
(`DataProvider::create_layout`, the same request) and opens it in Layout
mode ([desktop client](desktop-client.md), "Layout mode and Manage
Layouts"). Fields go on a new layout with `place_field` or the Field tool.

### Field controls

`set_field_control` shows a field object with another control, as the
Inspector's Control style does in FileMaker Pro (re/layout-model.md 5.4,
10.13), planned with `om_data::layout_objects::plan_control` and saved as
an object edit is: behind the layout's exclusive lock, its member stamped.
The host reads what the file says of the object's field (its type) and of
the value list (that the file has it) under the same store guard it plans
and saves the change under.

What changes, in place in the stored object store (every other byte as
stored):
- the field object's `displayType` and `valueListKey`, a missing one
  inserted in field number order; its flags stay as stored, as FileMaker
  Pro's Inspector keeps them. For the one change measured in Pro (an edit
  box to a pop-up menu) the object is Pro's, byte for byte;
- the style's object type, from 61 to 66 by control (65, Radio Button Set,
  from FMUpgradeTool's write alone: no starter holds one);
- the object's custom style names are dropped. They name styles of the old
  object type, so the object takes the theme's default for its new type
  from the file's own theme.

Refused (status 3 or invalid):
- containers, fields showing several repetitions, and controls other than
  these six (a concealed edit box);
- a calendar on a field that is not a date;
- a list control without a value list, and a value list the file lacks:
  the one named, or for a list control the one the object kept;
- a style whose type or custom style fields are not in the form expected.

A request the object already holds reports `already_applied`; any other at
an old revision is a `conflict`.

Where the file differs from Pro's for the same change: OpenMaker keeps the
objects in their stored order, where Pro writes them in an order of its
own, and does not count the change in the layout index's modification
count ([#241](https://github.com/OpenMaker-Dev/openmaker/issues/241)).

### Parts

`change_parts` adds a part, deletes one or sets one's height, as FileMaker
Pro's Part Setup (Create, Delete) and a dragged part boundary do
(operation 22, planned with `om_data::layout_placement::plan_parts`, saved
as an object edit is; re/layout-model.md 10.18). A new part goes where
FileMaker's order of parts puts it, with its type's styles and no objects.
The parts are keyed again from the top, each moving with its object list,
and the objects of every part that moves move with it, their frames and
object index entries included. Refused: adding a sub-summary (it needs a
break field; set one afterwards with `set_part_options`) or deleting one,
a second part of a kind
a layout has once, deleting a part that holds objects (Pro deletes them
with it, after asking) or the only part, and a height that is not whole
points or leaves an object below the part's end. A request the layout
already shows (the part of that kind and height there, the deleted kind
gone, the part at that height) reports `already_applied`; any other at an
old revision is a `conflict`.

### Part options

`set_part_options` sets what FileMaker Pro's Part Definition sets of an
existing part's page options (re/layout-model.md 4.3, 10.15), planned with
`om_data::layout_placement::plan_part_options` and saved as an object
edit is: behind the layout's exclusive lock, its member stamped. In the
part's `PartDef` `[4, 5, L, 03, P, 02]`, every other byte as stored:
- `flags`, as Pro's checkboxes set them: page break before each
  occurrence `0x1`, after every N occurrences `0x2`, restart page numbers
  `0x4`, allow the part to break across page boundaries `0x40`, discard
  the remainder before a new page `0x100`;
- `pageBreakAfter`: N, 1 to 32,767, with `0x2`;
- a leading sub-summary's `breakFieldKey`, the field it is sorted by: a
  field of the layout's own occurrence. The request names the table the
  field was chosen from, and the host checks, under the store guard it
  saves under, that the occurrence still shows that table and the table
  has the field.

For the one change Pro made of each, the `PartDef` is Pro's byte for byte.
Pro also writes the file's history, the layout index's count, the object
store and some library items' counts again on saving; OpenMaker keeps them
as stored ([#241](https://github.com/OpenMaker-Dev/openmaker/issues/241) for the index count).

Refused, as Part Definition offers it (re/layout-model.md 10.15): an
option the part's type does not offer, a trailing sub-summary's or a
navigation part's options, a break both before each occurrence and after
every N, a discard without breaking across pages (and on any part but a
body), after every N without an N from 1 to 32,767, a sort field on any
part but a leading sub-summary, a field the file lacks, any other bit of
`flags`, and a part the layout lacks. om-author's `page_break_after` turns
a break before each occurrence off, and the other way round, as the dialog
does. A request the part already holds reports `already_applied`; any
other at an old revision is a `conflict`. New, deleted or reordered parts,
part types, heights and the alternate and active row state are not
changed.

### Layout width

`set_layout_width` sets a layout's width, `LayoutDef.baseWidth`
(re/layout-model.md 10.12), as an object edit is saved: planned with
`om_data::layout_placement::plan_width` and written by the host behind
the layout's exclusive lock, its member stamped. Only that double of the
definition changes; every other byte stays as stored. The width is 1 to
32000 points (OpenMaker's bound, as for frames); objects beyond it stay,
off the layout, as FileMaker files keep them. A request the layout already
holds reports `already_applied`; any other at an old revision is a
`conflict`. FileMaker Pro's own width change also raises the layout
index's count and writes the object store again; OpenMaker keeps both as
stored (re/layout-model.md 10.12). The desktop's Layout mode sets it by
dragging the layout's right edge ([desktop client](desktop-client.md),
"Layout width").

### Layout Setup

`layout_setup` sets what FileMaker Pro's Layout Setup sets of a layout's
name, views, General options and "Include in layout menus"
(re/layout-model.md 10.11),
planned with `om_data::layout_catalog::plan_setup` and written by the host
in one journaled commit under the catalog's write lock and the index's
and the layout's exclusive locks:

- the name in the member `[4, 5, L, 10]` and its entry
  `[4, 1, 07, L, 10]`, and in the name list `[4, 1, 01]` its entry under
  the new name's key (with the id after it when another layout has the
  name);
- the views in the view flags `f1` of the member and the entry: `0x2`,
  `0x4`, `0x8` set for Form, List, Table view not available, and `0x200`
  for List or `0x4000` for Table as the default view (neither for Form);
- the General options in the same `f1`, as FileMaker Pro's checkboxes set
  them (`options`, by name): `0x10` set for "Save record changes
  automatically" off, `0x20` for "Show field frames when record is
  active" on, `0x1` for "Delineate fields on current record only" on,
  `0x10000000` for "Show current record indicator in List View" off, and
  `0x8000` for "Enable Quick Find" off;
- "Include in layout menus" cleared as bit `0x08` of the entry's `f0` and
  of the member's (a member keeps `f0` only when it is not 0);
- the member's and the index's stamps counted up, as FileMaker Pro's
  Layout Setup counts them.

Other bits of `f1` are refused, as are a set of views with none available
or without the default one, folders and separators, and an unusable name.
A request the layout already holds reports `already_applied`; any other at
an old revision is a `conflict`. Pro also writes the layout's object store
again and raises the stamp counts of the library items it shows; OpenMaker
keeps both as stored (re/layout-model.md 10.11).

### Buttons on layouts

The desktop's Layout mode places a **text button** with a label on the
body of an existing layout, with one of two actions (Wire format below):

- **Perform Script** with a script of the file chosen from the list and
  an optional parameter: operation 7;
- **Single Step**, one other script step with its options, as the Script
  Workspace stores a step: operation 8, which carries the step's
  descriptor (id, value, flags, the 14 `mReserved` bytes) and its data
  subtree.

Both are what FileMaker Pro stores for such buttons (re/layout-model.md
5.3.3); no `om-author` command sends them yet.

The host checks, in this order: the file is not read-only (status 810),
the label (some text, one line, at most 1,000 characters; 5), for
operation 7 that the parameter's Polish stream and, when it has one, its
formatted stream decode (5), and for operation 8 that the step is one a
button may hold (`check_button_step`: a step of the catalog that FileMaker
Pro allows on a button, not Perform Script, enabled, with nothing stored
that its schema does not explain, every calculation's streams decodable,
its data at most 64 KiB; 5, or 3 for options OpenMaker does not edit).
Then the layout exists (105), a script the step names is one of the
file's scripts, not a folder or separator (104 when there is none, 5
otherwise), and the layout's revision. It then plans with
`om_data::layout_placement::plan_button`, as the client does: the same
layouts, frames and overlaps as a field's (Fields on layouts), and writes
into the layout member `[4, 5, L]` only:

- the button's `LayoutObject` (type 10, the `button` style type 80 with
  the rotation only, anchored left and top) appended to the object store
  and the root stack, on top. Its `buttonObj` is the starter solutions'
  plain-label text button: text flags `0x2`, icon size 16, display type 0
  and the step: Perform Script (1), value 4, the script's key in
  `mReserved` (`kScriptKey`), and `kCalculation` with a parameter; or the
  single step's descriptor as sent;
- its attribute record `[4, 5, L, 05, O]`: the label `01` (a plain SCSU
  value), `0d` `00 00`, an empty name `10`, fresh UUIDs `d8` and `d9`, and
  the step's data `11`: Perform Script's only with a parameter, holding it
  as slot 0's calculation (`11, 00 00, 05`, context `01 00`); a single
  step's always, empty when the step has no options;
- the button at the end of the body part's list `[4, 5, L, 03, <body>,
  07]`, its `0a` entry (its frame's top and left in whole points, its
  pre-order index, its key, 0 and 1), `05, 00` raised, and the layout's
  stamp, as for a field.

No tab order entry or `07`, tooltip, name, icon, style or script trigger
is written. A repeated request after a lost answer finds a button with the
same frame, label and step descriptor, and step data the same byte for
byte, and reports it placed with `changed` false; any other placement
from an old revision is a conflict (306), a button that differs only in
its parameter or options included. The client sends operation 7 or 8 only
when Describe lists bit 7 or 8 (`NativeProvider::place_button`,
`place_button_step`), through `DataProvider::place_layout_button`, which
plans it first and refuses locally what the host would refuse.

### Running scripts

`run_script` asks the host to run a script with Perform Script on Server
(script-runtime.md 11), the request FileMaker Pro sends for the step with
"Wait for completion" on, and waits until the script ends. The host runs
it in a session of its own as the configured account, with that account's
privileges: OpenMaker Server runs an account's scripts unless the account
is not in the file's [Full Access] set and the script does not run with
full access privileges. There is no layout, found set or
record; the script finds its own (script-runtime.md 11.5). The
`parameter` is text, which the script reads with Get ( ScriptParameter ).

- **Completed.** The host answered: `last_error` is the script's final
  Get ( LastError ) (0 when it ended without one), or the host's refusal
  before it ran, which uses the same numbers (812 when the host runs too
  many scripts, 104 when it cannot find the script; script-runtime.md
  11.3). On OpenMaker Server a script halted by a server shutdown
  answers 1. The response is `ok` either way; the CLI exits 1 when
  `last_error` is not 0.
- **Refused before sending.** A script that is not found or is a folder,
  a `parameter` over 1,000,000 characters (FileMaker's 513) and a script
  not at `expected_revision` (`conflict`) run nothing. The revision is
  checked just before the request is sent, not atomically with the run.
- **Outcome unknown.** Once the request is sent, a failure (the
  connection lost, an answer that does not read) is `outcome_unknown`:
  the script may have run in part or whole, and whatever it changed stays
  changed. Nothing is retried; check what the script changes before
  running it again.

A waiting run lasts as long as the script does: no client timeout ends
it. OpenMaker Server aborts a script after its `--script-seconds` limit
(default 1800). Not waiting (the step's
"Wait for completion" off) and Perform Script on Server with Callback are
not offered: an agent wants the result.

### Value-list choices

`list_choices` and `choose_value_list_item` are the operations the
desktop's pop-up menus, drop-down lists and option sets use
(`om_data::choice`, issue #178; layout-model.md 5.4).
`choose_value_list_item` chooses in a pop-up menu or a drop-down list;
sets change their values their own way.

**`list_choices`** lists the items the field control `object` offers on
record `record`:
- Each item has its stored `value` and the `label` the control shows: the
  second field's value for a list set to show values only from the second
  field.
- A divider (`-` alone, file-metadata.md 6.1) is marked.
- The items are read as the desktop reads them, through the calculation
  host over this connection, with the account's privileges. This is the
  one command that reads records: the records the list's values come
  from, and for "include only related values" the records related to
  `record`.

The list is complete or the command fails:
- More than 100,000 items is `unsupported`, never the first ones.
- Another file's value list and a calculated list are `unsupported`.
- A table the account cannot read is an error, never a shorter list.

The target must exist and be a field control with a value list on a
layout of this file. `record` must exist in the layout's table.

These are refused rather than guessed: a control inside a portal, a
related field (one of another occurrence than the layout's), and a
control showing several repetitions.

**`choose_value_list_item`** has the parameters and checks of
`list_choices` and an item's `value`. Choosing an item is a user's edit in
a window:
- it enters the field and runs the left field's exit triggers and its own
  OnObjectModify;
- it waits in the record's draft;
- it is validated and committed under the record lock when the record is
  committed, with OnObjectValidate, OnObjectSave and OnRecordCommit able
  to refuse it.

This service has no window: no field focus, no draft and no trigger
engine. So the command answers `unsupported` with `details.unavailable`
= `no_window` (FileMaker's error 3) and changes nothing.
- `capabilities` reports it unavailable for the same reason.
- It never commits a value without the triggers and validation a window
  runs.
- No window-session protocol exists yet for an agent to name a window.

The edit, apply, run loop:

```sh
om-author get-script --script-name Greeting > greeting.json   # source and revision
# edit the source, then plan and apply it with that revision; apply returns the new one
om-author run-script --script-name Greeting --parameter agent --expected-revision sha256:...
```

### Value lists

`set_value_list_values` is what a pop-up menu's Edit… saves on the
desktop (Edit Value List, #869): a list of custom values gets the values
given, in order, written as FileMaker Pro 26.0.3 writes them
(re/file-metadata.md 6.5): the member's values `[21, 5, V, 03]`, and the
host's stamps on the member and on the catalog's index `[21, 1]`; its
definition, name and every other entry stay as stored. Only if the list
is still at `expected_revision`, the revision `get_value_list_values`
reads; then the list is read back over a new connection. A list whose
values come from a field, a value holding a return, or values over 1,000
stored bytes (one piece of a stored value) are refused. Manage Value
Lists' own changes (new lists, sources, options) are not in this version:
`get_value_lists` still reports the catalog read only.

### Calculation name completion

`complete_calculation` answers the query Specify Calculation's
completion list shows while typing (`om_data::formula_editor::completion`,
issue [#69](https://github.com/OpenMaker-Dev/openmaker/issues/69); docs/desktop-client.md), for agents writing formulas:

- The **prefix** is the name the formula scanner reads ending at `cursor`
  (a name runs over the spaces between its words, so `First Na` is tried
  whole before `Na`). `replace` is its range in characters (Unicode scalar
  values), ending at the cursor: writing `insert` there keeps all other
  text, and `selection_after` is the selection in the result (a
  function's first parameter, else after the name).
- **Candidates:** FileMaker's functions and Get functions (the functions
  pane's list, from calc-functions.csv and get-selectors.csv), Get
  selectors after `Get (`, the file's custom functions, the context
  occurrence's fields, and an occurrence's fields after `Occurrence::`.
  Fields are written qualified, as in script steps, with `${...}` where
  a name needs it. Names match from their start, ignoring case; an exact
  name comes first, then names alphabetically, then fields before custom
  functions before functions.
- **References:** fields and occurrences retain their owning file and native
  IDs, including members imported from related files. A root occurrence of a
  related table belongs to the root file; its field belongs to the related
  file. Client-only hydrated IDs are never serialized as references.
- **Refusals:** nothing is completed inside a text literal or a comment,
  inside a word, or where no name ends at the cursor; `refused` is
  `in_text`, `in_comment`, `inside_name` or `no_name`.
- **Sources:** fields and custom functions come only from the file's
  names as the host gives them. Without them `sources` reports
  `no_names`; without a context occurrence, unqualified fields report
  `no_context`. Plug-in functions, occurrence names, variables and
  `Let` names are not offered.

The matching and order are OpenMaker's choices; FileMaker's own list has
not been measured (re/client-ui.md, Unknowns).

## Command line

```sh
export OPENMAKER_HOST=127.0.0.1 OPENMAKER_FILE="Tasks.fmp12" OPENMAKER_ACCOUNT=Admin
export OPENMAKER_PASSWORD_FILE=~/.config/openmaker/password   # or OPENMAKER_PASSWORD
export OPENMAKER_CA_CERT=/path/to/server-ca.pem                 # a private CA, or the server's own certificate

om-author capabilities
om-author inspect --include scripts,tables
om-author get-script --script-name Greeting > greeting.json
# edit the "source" text into greeting.txt, then:
om-author plan  --script script:5@Tasks.fmp12 --source greeting.txt --expected-revision sha256:...
om-author apply --script script:5@Tasks.fmp12 --source greeting.txt --expected-revision sha256:...
om-author inspect --include tables          # each table's ref and fields_revision
om-author plan-field   --table table:32769@Tasks.fmp12 --name "Follow up" --type date --expected-revision sha256:...
om-author create-field --table table:32769@Tasks.fmp12 --name "Follow up" --type date --expected-revision sha256:...
om-author get-layout --layout-name Tasks    # objects, frames and the layout's revision
om-author plan-placement --layout layout:1@Tasks.fmp12 --field-name "Follow up" --frame 110,80,200,20 \
    --label "Follow up" --label-frame 20,80,80,20 --expected-revision sha256:...
om-author place-field --layout layout:1@Tasks.fmp12 --field-name "Follow up" --frame 110,80,200,20 \
    --label "Follow up" --label-frame 20,80,80,20 --expected-revision sha256:...
echo '{"version":"openmaker.authoring/v1","command":"capabilities"}' | om-author request
om-author run-script --script-name Greeting --parameter agent   # run it on the host, wait for the result
om-author complete-calculation --text 'Left ( Tasks::Ti' # names completing the one typed at the end
om-author commands                          # every command and its parameters; no connection
```

`--host`, `--port` (default 5003), `--file`, `--account` and
`--password-file` override the environment; `commands` needs none of
them. Passwords are never taken as arguments, which other users of the
machine can list, and are never printed. The exit status is 0 for a
successful response (a valid `validate`, a `run_script` whose last error
is 0), 1 for an error response or another `run_script` last error, and
2 for a usage error.

`cargo run -p om-author --example edit_script`, `--example create_field`
and `--example place_field` run the whole flows against an OpenMaker
Server in the same process, on a synthetic file. `edit_script` ends by
running the applied script with `run_script` at the revision it applied;
the last two restart the server and store a value in the new field
through the desktop client's record writes. `place_field` creates a text field, places it with a label
on a Form layout, and reads the layout back after the restart.

## MCP server

`om-author mcp` serves every command of the catalog as an MCP tool over
stdio with the official Rust SDK (`rmcp` 3.9), under the name `om-author
commands` gives it as `mcp_tool` (`openmaker_capabilities`,
`openmaker_place_field`, `openmaker_commands` and the others). A tool's
arguments are the command's params; its result is the JSON response as
structured content, marked as an error when `ok` is false. Protocol
revisions: **2026-07-28** (stateless `server/discover`, per-request
`_meta`) and **2025-11-25** (`initialize`); no others are offered. stdout
carries protocol messages only. Each tool call whose command needs the
file (`needs` in `commands`) opens it over a new connection with the
configured account, so the server holds no session state between calls.

Starting MCP, listing its tools and calling `openmaker_commands` need no
connection settings or credentials. They are loaded when the first tool
needs the file. Missing configuration is that tool's structured error;
the MCP session stays open so discovery still works.

Registering the server with an agent is the user's choice; for example a
client configuration entry running `om-author mcp` with the `OPENMAKER_*`
environment above. Nothing registers it automatically.

## How writes reach the file

Writes go through **OpenMaker Server's authoring extension**: one Perform
transaction, opcode 240. It is not a FileMaker transaction, and it is sent
only to a host that claims it: the client first asks the server object
`_is_a("IDL:OpenMaker/Authoring:1.0")`, CORBA's built-in type check, as for
OpenMaker's file list ([hosted-file-list.md](hosted-file-list.md)).
A host without the interface answers `false` (FileMaker Server 26.0.3 did
for the file list's interface) and is never sent the transaction; a host
that claims it but answers with a lone status 3 is treated as not having
it.

FileMaker Pro saves a script with op60, op82, op61, op13 and op62 on
`[17, 5, S]` and `[17, 1]` (re/scripts.md, "Saving scripts to a host").
That sequence was captured against OpenMaker Server only, and op13's reply
is the same empty page whether the host stored the upload, skipped it (a
protected index without `fc` and `KSerNum`, re/schema-path.md 3.3) or
failed to save it, so a client cannot tell an applied edit from a lost
one. The extension does the same catalog edit on the host and answers
with what happened:

1. The session must have the file open (status 100) and be logged in,
   and the account must be in the file's verified built-in [Full Access]
   set; every other account is refused (status 9). This is checked before
   the rest of the request is read (see Wire format).
2. The new definition must decode as a script of the same id whose steps
   nest, and differ from the stored one only in `04`, `05` and the stamp
   (status 5 otherwise).
3. Under the file's store lock, the stored definition's revision is
   compared: equal to the new one, nothing is written (`changed` false);
   not the expected one, status 306 with the current revision.
4. `[17, 5, S]` and `[17, 1]` are locked exclusively (type 10) for the
   session. A client holding either, as FileMaker Pro's Script Workspace
   holds an open script, refuses the edit (status 13, naming the holder).
5. The definition gets the stamp of re/schema-path.md 8.1 (`fc`
   incremented, `fc 01` the session's user name, `fc 02` the account,
   `fc 03` the host time); `[17, 1]` gets the same stamp when the file has
   one (none is created). Both are replaced in one journaled commit, and
   the `.fmp12` is rewritten and renamed over the old one before the
   reply. A failed save puts everything back (status 800, 809 or 810).
6. Other clients registered on the file get attention-2 notices for
   `[17, 5, S]` and `[17, 1]`, as the op62 releases would send; server
   script sessions read the scripts again.

A new field is Manage Database's save of one field (op60 `[08]` and
`[T, 5]`, op61 `[T, 3]` to exclusive, op13 `[T, 3]`, op62, op73;
re/schema-path.md 2.3), which op13 cannot confirm either, done on the host:

1. The account check of step 1 (status 9).
2. The type must be 1 to 5 and the name usable (status 5, 1300 "The
   specified name can't be used"); the table must exist (106).
3. Under the file's store lock, the field catalog's revision is compared.
   A field the request makes is already there under that name (an earlier
   request's answer was lost) and the revision is not the expected one:
   nothing is written (`changed` false, the field's id). Otherwise a
   revision other than the expected one is status 306 with the current
   revision, and a taken name status 12 ("Name already exists").
4. `[08]` and `[T, 5]` are locked for writing (type 9) and `[T, 3]`
   exclusively (10), the locks Manage Database holds while it saves. A
   client with Manage Database open, with the table's fields open, or
   editing a record of the table refuses the field (status 303, naming the
   holder).
5. The field is planned into the catalog as stored (above, "New
   fields"), with fresh UUIDs and the stamps; the catalog `[T, 3]` is
   replaced in one journaled commit, the schema the host serves is read
   again, and the `.fmp12` is rewritten and renamed over the old one
   before the reply. A failed save puts the catalog and the schema back
   (status 800, 809 or 810).
6. Other clients registered on the file get an attention-2 notice for
   `[T, 3]`, as the catalog's op62 release would send.

A field placed on a layout is Layout mode's save of the layout: FileMaker
Pro locks `[4, 5, L]` for writing with its modification count, upgrades it
to exclusive and uploads the whole layout with op13 (re/schema-path.md
2.3), which op13 cannot confirm either. Done on the host:

1. The account check of step 1 (status 9). A file hosted read-only is
   refused (810).
2. The layout must exist (105); the label must be one line of at most
   1,000 characters (5).
3. Under the file's store lock, the occurrence must still resolve to the request's expected base table (306 if it changed), including an already-applied retry. The layout member's revision is then compared.
   The request's objects already on top of the stack (an earlier
   request's answer was lost) and the revision not the expected one:
   nothing is written (`changed` false, their keys). Otherwise a revision
   other than the expected one is status 306 with the current revision.
4. The field must be one of the occurrence's table of this file (102) and
   not a container (5); the placement is planned as above, a frame or label
   that cannot be used is status 5 and a layout or position this version
   does not edit status 3.
5. `[4, 5, L]` is locked exclusively (10). A client holding any lock on
   it, as FileMaker Pro does while it saves the layout, refuses the
   placement (status 304, naming the holder).
6. The objects get fresh UUIDs and the layout its stamp; the member is
   replaced in one journaled commit, the schema the host serves (layouts
   included) is read again, and the `.fmp12` is rewritten and renamed over
   the old one before the reply. A failed save puts the layout and the
   schema back (status 800, 809 or 810).
7. Other clients registered on the file get an attention-2 notice for
   `[4, 5, L]`.

### Wire format

Requests and replies use the RCTransaction push order (the receiver pops
from the end). A request is the 20-byte header (opcode 240, the file id)
and, popped in this order: `u16` version (1), `u8` operation, then the
operation's fields.

| operation | fields (pop order) | OK reply fields (pop order) |
|-----------|--------------------|-----------------------------|
| 1 Describe | none | `u32` operations (bit 2: replace steps, bit 3: create field, bit 4: place field, bit 5: describe security, bit 6: create layout, bit 7: place button, bit 8: place button step, bit 9: save, bit 11: set object frames, bit 12: delete objects, bit 13: duplicate layout, bit 14: delete layout, bit 15: place text, bit 16: Layout Setup, bit 17: layout width, bit 18: field control, bit 19: part options, bit 20: set text, bit 21: place portal, bit 22: change parts), `u8` may author, `u32` largest definition |
| 2 Replace script steps | `u32` length, the new definition as a wire slot list (one child of a root), 32-byte expected revision, `u64` script id | `u8` changed, 32-byte stored revision |
| 3 Create field | `u64` table id (its key read big-endian), 32-byte expected fields revision, `u8` data type (1 text, 2 number, 3 date, 4 time, 5 timestamp), name, comment (empty for none), strings in the trailing-`u32` SCSU form | `u8` changed, `u64` field id (its key read big-endian), 32-byte stored fields revision |
| 4 Place field | `u64` layout id, 32-byte expected layout revision, `u64` expected base-table id, `u64` occurrence id, `u64` field id (keys read big-endian), the frame as four `u64` IEEE-754 bit patterns (top, left, bottom, right, points), `u8` label (0 none, 1), and with one the label's text and its frame | `u8` changed, `u64` field object key, `u64` label object key (0 for none), 32-byte stored layout revision |
| 5 Describe security | none | three lists, each a `u32` count: accounts (record key as `u8` length and bytes; then name, kind (`u8`), active, must-change-password and privilege set key, each a presence `u8` and the value), privilege sets (key; name, record flags as `u8` length and bytes, at most 16, password days `u16`, each optional) and extended privileges (key; optional keyword; a `u32` count of set keys). A count past the bytes left is malformed; a catalog whose reply would pass 1 MiB is refused with status 3 |
| 6 Create layout | 32-byte expected layout index revision, `u64` occurrence id (its key read big-endian), `u64` id of the layout whose theme and width it takes (0: the first in the order with a theme), the name, the 16-byte UUID the layout gets (the request's identity) | `u8` changed, `u64` layout id (its key read big-endian), 32-byte stored layout index revision |
| 13 Duplicate layout | 32-byte expected layout index revision, `u64` layout id (its key read big-endian), the 16-byte UUID the copy gets (the request's identity) | as 6 Create layout: `u8` changed, `u64` the copy's id, 32-byte stored layout index revision |
| 14 Delete layout | 32-byte expected layout index revision, `u64` layout id | as 12 Delete objects: `u8` changed, 32-byte stored layout index revision |
| 7 Place button | `u64` layout id, 32-byte expected layout revision, `u64` script id (the script's serial id), the frame as four `u64` IEEE-754 bit patterns (top, left, bottom, right, points), the label, `u8` parameter (0 none, 1), and with one the parameter's formatted and Polish streams, each a `u32` length and the bytes (at most 64 KiB each) | as Place field: `u8` changed, `u64` button object key, `u64` 0, 32-byte stored layout revision |
| 8 Place button step | `u64` layout id, 32-byte expected layout revision, the frame as four `u64` IEEE-754 bit patterns (top, left, bottom, right, points), the label, `u16` step id, `u16` value, `u32` flags, the 14 `mReserved` bytes, `u32` length and the step's data subtree as a wire slot list (one child of a root; length 0 for none, at most 64 KiB) | as Place field |
| 9 Save | none | none. The host writes the file now if it holds changes the disk lacks, and answers OK only when the disk holds them all; a failed write is status 800, the changes kept in its memory for the next save. A read-only file is status 3. The desktop's script catalog save asks for it after its op13 uploads, whose reply says nothing about the disk (re/schema-path.md 3.3) |
| 11 Set object frames | `u64` layout id, 32-byte expected layout revision, `u32` count (1 to 1,000), then each object's key (`u64`) and its frame as four `u64` IEEE-754 bit patterns (top, left, bottom, right, points) | `u8` changed, 32-byte stored layout revision |
| 12 Delete objects | `u64` layout id, 32-byte expected layout revision, `u32` count (1 to 1,000), then each object's key (`u64`) | `u8` changed, 32-byte stored layout revision |
| 15 Place text | `u64` layout id, 32-byte expected layout revision, the frame as four `u64` IEEE-754 bit patterns (top, left, bottom, right, points), the text | as 4 Place field: `u8` changed, `u64` the text object's key, `u64` 0, 32-byte stored layout revision |
| 16 Layout Setup | `u64` layout id, 32-byte expected layout revision, `u64` view flags `f1`, the name, `u8` hidden from the layout menus | as 11 Set object frames: `u8` changed, 32-byte stored layout revision |
| 17 Layout width | `u64` layout id, 32-byte expected layout revision, `u64` the width's IEEE-754 bit pattern (points) | as 11 Set object frames: `u8` changed, 32-byte stored layout revision |
| 18 Field control | `u64` layout id, 32-byte expected layout revision, `u64` object key, `u32` display type, `u64` value list key, `u8` whether the value list is given (0: keep the object's) | as 11 Set object frames: `u8` changed, 32-byte stored layout revision |
| 19 Part options | `u64` layout id, 32-byte expected layout revision, `u64` part key, `u64` `PartDef.flags`, `u32` page break after every N (0: none), `u64` break field's occurrence id (the layout's own), `u64` that occurrence's base table id, `u64` the field id, `u8` whether the break field is given (0: keep the part's) | as 11 Set object frames: `u8` changed, 32-byte stored layout revision |
| 20 Set text | `u64` layout id, 32-byte expected layout revision, `u64` object key, the new words (string) | as 11 Set object frames: `u8` changed, 32-byte stored layout revision |
| 21 Place portal | `u64` layout id, 32-byte expected layout revision, `u64` expected base-table id, `u64` occurrence id, the frame as four `u64` IEEE-754 bit patterns (top, left, bottom, right, points), `u32` rows, `u32` count (1 to 20), then each field id (`u64`) | as 4 Place field: `u8` changed, `u64` the portal's key (its fields have the keys after it, in order), `u64` 0, 32-byte stored layout revision |
| 22 Change parts | `u64` layout id, 32-byte expected layout revision, `u8` action (1 add, 2 delete, 3 resize), `u64` part key (0 for add), `u32` part type, `u64` the size's IEEE-754 bit pattern (points; 0 for delete) | as 11 Set object frames: `u8` changed, 32-byte stored layout revision |
| 24 Value list values | `u64` value list id, 32-byte expected member revision, `u32` count of values (at most 10,000), then each value (a string; at most 10,000 characters in all, none holding a return) | as 11 Set object frames: `u8` changed, 32-byte stored member revision |

Every reply pops `u16` status, `u16` version and the message string
first; a refusal then pops `u8` and, when it is 1, the 32-byte current
revision. A reply holding only a status is a host without the extension.
Statuses use FileMaker's numbers where one fits: 0 OK, 3 unsupported, 5
invalid, 9 insufficient privileges, 12 name already exists, 13 in use, 100
file not open, 102 no such field, 104 no such script, 105 no such layout,
106 no such table, 108 no such value list, 303 schema in use, 304 layout in use, 306 revision
mismatch, 800/809/810 save failed, 1300 name can't be used.
One request carries at most 1 MiB of definition, within the host's 2 MiB
request limit, and a field name of at most 100 characters and a comment of
at most 10,000, a label of at most 1,000 and a layout name of at most 100. OpenMaker Server answers a
request in this order. A version or operation it does not know is status 3.
Then, from the version and operation alone, before the operation's fields
are read, it checks the caller for what the operation needs: the file open
in this session (status 100) and a login (9), and for every operation but
Describe an account in the file's [Full Access] set (9). Only then are the
fields read, so it never reads a definition from a caller that may not send
one, and a malformed request from such a caller gets that refusal, not the
CORBA system exception `MARSHAL`. It reads a new definition into its tree
once, within what the client's address may still hold of the server's
memory for clients' uploads (`--upload-memory-mib`, record-writes.md): up
to 256 times the request's size plus 64 KiB while it reads. The tree stays
charged while the request is served, and what decoding it as a script takes
is charged before it is decoded: about twice the tree, a step for each
descriptor, and the step data the steps name twice again, once however
many steps name it, since decoding shares it between them
([#149](https://github.com/OpenMaker-Dev/openmaker/issues/149)). Past
either the request is
refused with the CORBA system exception `IMP_LIMIT` before anything is
applied. The order and the limit are OpenMaker's choice. A client sends an
operation only when Describe lists its bit, so a host that predates field
creation, placement, reading security, new layouts or moving objects is
never sent operation 3, 4, 5, 6, 7, 8 or 11.
Operation 5 is checked against the file's own account (see Reading
security), not the server's grant, before anything else is read. The
codec is `om_protocol::authoring`; the host side is `om-server`'s
`authoring.rs`, the client `om_provider::NativeProvider`'s
`script_authoring`, `field_authoring`, `layout_authoring`,
`layout_creation`, `object_editing`, `script_definitions`, `field_catalog`,
`layout_members`, `layout_member`, `layout_index`, `replace_script_steps`,
`create_field`, `place_field`, `create_layout_on_host`, `set_object_frames`,
`place_button` and `place_button_step`; the shared planners are `om_data::field_catalog`,
`om_data::layout_placement`, `om_data::layout_catalog` and
`om_data::layout_objects`, the encoders `om_layout::encode`.

## Limits

- **Hosts.** Every design change needs OpenMaker Server: the commands
  `commands` lists with the effect `authors`, and the desktop's placements
  through the same extension. On a host that does not claim the extension,
  FileMaker Server (Claris) included, those commands report `unsupported`
  and nothing is sent. `get_security` is refused there too, since reading
  security needs the extension ([Reading security](#reading-security)),
  and the choice commands need a window (Records, below); the other
  commands work.
  `run_script` sends the request FileMaker Pro sends for Perform Script
  on Server, which FileMaker Server answers too, but it has been run
  against OpenMaker Server only (in process, on a synthetic file).
  Native script, schema and layout saves to FileMaker Server stay
  unavailable until its replies to the captured sequences are verified;
  OpenMaker reading back what OpenMaker wrote does not prove FileMaker Pro
  compatibility either. One case has been checked in FileMaker Pro 26
  (issue #166): a stored Text field from `create_field` and its edit box
  from `place_field`, without a label, on a Form layout of a copy of a
  starter solution. The file passed Check Consistency and opened without a
  warning; Browse showed the edit box on first open, before any visit to
  Manage Database or Layout mode, and its value stayed after reopening;
  Manage Database and Layout mode showed the field and its binding; and
  after FileMaker Pro saved a field comment and moved the box, the file
  passed Check Consistency again and kept all of it. Earlier placements
  without the part list entry were not shown in Browse. The 14-point-high
  box clipped its unfocused text under the theme's padding, a limit of the
  frame chosen, not of the placement. Other field types, labels, layouts
  and script edits have not been opened in FileMaker Pro.
- **Records.** `list_choices` reads records, as above; no other command
  does. `choose_value_list_item` and `enter_other_value` are unavailable
  without a window.
- **Operations.** Only replacing an existing script's steps and adding a
  stored text, number, date, time or timestamp field with the default
  options to an existing table. Creating, renaming, deleting, moving and
  duplicating scripts and folders, script options, tables, changing,
  renaming or deleting fields, calculation, summary, container and global
  fields, field options (auto-enter, validation, indexing, repetitions,
  storage), occurrences, relationships, value lists, custom
  functions and accounts are read-only here (`capabilities` lists them as
  unavailable); of value lists, only a custom list's values change
  ([Value lists](#value-lists)). On layouts, only the operations above:
  new, duplicated and deleted Form layouts; placing edit boxes of the
  layout's table, texts, and portals of related occurrences with fields
  in their first row (buttons are placed by the desktop's Layout mode);
  moving, resizing and deleting top-level objects; a field's control (the
  styles in [Field controls](#field-controls)), a text's words, Layout
  Setup, the width, part options and adding, deleting and resizing
  parts. A field is placed as an edit box. No other object kinds, no
  related fields outside a new portal,
  repetitions, styles, data formatting, tab order, triggers, conditional
  formatting or hide conditions; nothing placed in headers, footers,
  summaries, stored portals, tab or slide panels, popovers or groups;
  moving and resizing only as above (not between parts, not containers or
  their contents); no sub-summary parts or themes, and of new layouts only the
  Form layout above (no List, Table or Report layouts, no fields placed
  when it is made, no folders; no renaming outside Layout Setup). No
  script catalog changes (new, renamed, moved or deleted scripts); field
  changes need op73's record work (re/schema-path.md 7.2) and calculation
  fields their formulas compiled against the file's names.
- **New fields are not placed on layouts by `create_field`**, as FileMaker
  Pro's "Add newly defined fields to current layout" option would; place
  one with `place_field`. A placed field is not in the layout's tab order,
  so Tab does not reach it (a click does), and New Record does not put the
  focus in it; a portal's fields are, as FileMaker Pro puts them there.
- **Layout indexes.** Besides the field reference and object indexes,
  FileMaker keeps an object name index `[4, 5, L, 08]` and the tab order
  `[4, 5, L, 15]` (re/layout-model.md 10.5); a placed object is unnamed
  and outside the tab order, so they are left as they are. FileMaker Pro's
  own save of a layout holding a placed edit box also added it to the tab
  order, set an undecoded flag bit on it and wrote a table view `0d` for
  the layout (issue #166); a placement does none of these. The layout
  catalog's index `[4, 1]` and its stamp are not changed. FileMaker Pro
  counted up its `fc` during a session that saved a field and a layout;
  which saves do so is not isolated.
- **One object per edit.** Edits to several scripts, and several new
  fields, are separate requests with their own revisions; there is no
  multi-object transaction.
- **Privileges.** Only accounts configured as [Full Access] on the server
  author. A file's finer privileges (script and layout access per
  privilege set, "Manage database, data sources, containers, and custom
  functions"; docs/re/session-identity.md 2) are not applied. FileMaker
  has no "Manage scripts" option: script access and "Allow creation of
  new scripts" govern scripts.
- **Running scripts** waits for the script: no Perform Script on Server
  without waiting or with a callback, no client timeout, and no way to
  stop a run from `om-author`. The result is text, as Perform Script on
  Server returns it. Runs cannot be undone, and a run whose answer is lost
  is not retried. The MCP server answers one tool call at a time, so the
  calls after a long run wait for it.
- **The desktop Script Workspace** saves a script's steps through the same
  step save as `apply` (`NativeProvider::save_script_steps`: the stored
  definition with the edited step list and step data, at the revision the
  scripts were read at). Its catalog changes (new, duplicated, renamed,
  moved and deleted scripts, script options) go through FileMaker Pro's
  own script save, not through this extension
  ([desktop-client.md](desktop-client.md), Script Workspace); `om-author`
  makes no catalog change.
- **Large files.** OpenMaker Server rewrites the whole `.fmp12` on each
  save; a save that takes longer than the client's 30-second read timeout
  is reported as `outcome_unknown`.
- **Not audited.** A dedicated security review of the extension and of
  OpenMaker Server's existing op13 handling (direct catalog uploads need
  the file's [Full Access] set; staged record uploads need only a session
  that may write the file, as below) is still to be done.

Structured authoring authenticates without requiring Browse layouts or tables.
Accepted source is aligned after parser normalization, so retrying the same
normalized edit retains its stored revision and reports already applied.
Direct native catalog uploads and schema-changing query instructions require the
file's [Full Access] set too; staging ordinary record edits keeps
its existing permissions. The native upload reply does not prove persistence.

Layout names and member revisions returned by `get_layout` come from one layout-catalog download. A concurrent rename can leave that observation older than the host, but cannot pair its old name with a newer revision. Placement readback also verifies the occurrence's base-table identity.

An already-applied placement retry still reaches the host's guarded table/revision check and fresh readback. A local match of occurrence, field and frame is not enough to report that the requested field is placed.
