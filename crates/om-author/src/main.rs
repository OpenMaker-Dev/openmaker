//! `om-author`: the authoring commands of `om_author` from the command line
//! or as an MCP server over stdio. Every command prints one JSON response
//! (`openmaker.authoring/v1`) on stdout; messages about the tool itself go
//! to stderr. Each command's CLI name comes from `om_author::catalog`; its
//! options are mapped to the command's parameters here.

use std::io::Read;
use std::process::ExitCode;

use om_author::api::{
    self, ApplyParams, ChangePartsParams, CreateFieldParams, CreateLayoutParams,
    DeleteLayoutParams, DeleteObjectsParams, DuplicateLayoutParams, EditParams, FieldControlParams,
    FieldParams, FieldType, FrameParams, InspectParams, LabelParams, LayoutParams,
    LayoutSetupParams, LayoutWidthParams, ObjectFrameParams, ObjectFramesParams, Part,
    PartOptionsParams, PlaceFieldParams, PlacePortalParams, PlaceTextParams, PlacementParams,
    RunScriptParams, ScriptParams, TextParams,
};
use om_author::api::{
    ChoiceParams, ChooseParams, CompletionParams, NoParams, OtherParams, ValueListParams,
    ValueListValuesParams,
};
use om_author::config::ConnectionOptions;
use om_author::service::handle_offline;
use om_author::{catalog, Command, Service};

const USAGE: &str = "\
usage: om-author [connection] <command> [options]

commands (each prints one JSON response):
  capabilities                       what the host and account allow
  inspect [--include PARTS]          metadata: scripts,tables,occurrences,
                                     layouts,value_lists,data_sources
  get-script SCRIPT                  a script as editable text
  validate SCRIPT --source FILE      compile edited text (FILE may be -)
  plan SCRIPT --source FILE [--expected-revision REV]
                                     the change, as revisions and a diff
  apply SCRIPT --source FILE --expected-revision REV
        [--expected-after-revision REV]
                                     replace the steps, then read them back
  plan-field TABLE --name NAME --type TYPE [--comment TEXT]
        [--expected-revision REV]    what adding the field would change
  create-field TABLE --name NAME --type TYPE [--comment TEXT]
        --expected-revision REV      add the field, then read it back
  get-layout LAYOUT                  a layout's parts, objects and revision
  plan-placement LAYOUT FIELD --frame FRAME [--label TEXT --label-frame FRAME]
        [--expected-revision REV]    what placing the field would change
  place-field LAYOUT FIELD --frame FRAME [--label TEXT --label-frame FRAME]
        --expected-revision REV      place the field, then read the layout back
  set-object-frames LAYOUT --object REF --frame FRAME --expected-revision REV
                                     move or resize one object, then read the
                                     layout back
  arrange-objects LAYOUT --object REF --object REF [--object REF ...]
        --arrangement ID --expected-revision REV
                                     align, distribute or resize the objects
                                     relative to one another (ARRANGEMENT:
                                     align_left_edges, distribute_horizontally,
                                     resize_to_largest_width, ...), then read
                                     the layout back
  place-text LAYOUT --text TEXT --frame FRAME --expected-revision REV
                                     place a text on the body, then read the
                                     layout back
  place-portal LAYOUT --occurrence-name NAME --field-name NAME [--field-name NAME ...]
        --rows N --frame FRAME --expected-revision REV
                                     place a portal of a related occurrence with
                                     those fields in its first row, then read
                                     the layout back
  delete-objects LAYOUT --object REF --expected-revision REV
                                     delete one object, then read the layout
                                     back
  create-layout --name NAME OCCURRENCE [LAYOUT] --expected-revision REV
                                     add a Form layout with an empty body,
                                     then read it back
  layout-setup LAYOUT --expected-revision REV [--name NAME]
      [--views form,list,table] [--default-view VIEW] [--in-menus yes|no]
      [--options OPTION=on|off,...]
                                     set a layout's name, views, General
                                     options and menu inclusion as Layout
                                     Setup does
  set-field-control OBJECT --expected-revision REV --control CONTROL
      [--value-list REF]             show a field object with another control
  set-text OBJECT --expected-revision REV --text TEXT
                                     change a text's or a label's words
  change-parts LAYOUT --action add|delete|resize [--part ID] [--kind KIND]
      [--height N] --expected-revision REV
                                     add, delete or resize a part as Part
                                     Setup and a dragged boundary do
  set-part-options LAYOUT --part ID --expected-revision REV
      [--options OPTION=on|off,...] [--page-break-after N] [--sorted-by FIELD]
                                     set a part's page options as Part
                                     Definition does
  set-layout-width LAYOUT --expected-revision REV --width POINTS
                                     set a layout's width
  delete-layout LAYOUT --expected-revision REV
                                     delete a layout as Delete Layout does,
                                     then read the layout list back
  duplicate-layout LAYOUT --expected-revision REV
                                     copy a layout as Duplicate Layout does,
                                     then read the copy back
  run-script SCRIPT [--parameter TEXT] [--expected-revision REV]
                                     run the script on the host and wait for
                                     its last error and result
  list-choices --object REF --record ID
                                     a field control's value-list items
  choose-item --object REF --record ID --value VALUE
                                     choose a pop-up menu or drop-down
                                     list item; needs a desktop window:
                                     answers no_window
  enter-other --object REF --record ID --value TEXT
                                     enter TEXT through a pop-up menu's
                                     Other...; needs a desktop window:
                                     answers no_window
  get-value-list-values --value-list REF
                                     a custom value list's name, values
                                     and revision
  set-value-list-values --value-list REF --source FILE --expected-revision REV
                                     replace a custom value list's values
                                     with FILE's lines, as Edit Value
                                     List does
  get-security                       the file's accounts, privilege sets and
                                     extended privileges, without passwords
  get-value-lists                    the value lists and their sources,
                                     read only
  complete-calculation (--text TEXT | --source FILE) [--cursor N]
        [--occurrence REF | --occurrence-name NAME] [--limit N]
                                     the names completing the one typed
                                     at the cursor in formula text
  create-file --path PATH            make a new, blank file at PATH (.fmp12
                                     added), as File > Create New... does;
                                     never replaces a file (no connection)
  compare-designs --source PATH --target PATH [--base PATH] [--include-same]
                                     compare two files' designs on this
                                     computer: matched objects and how
                                     they differ, never content (no
                                     connection)
  commands                           every command, what it needs and its
                                     parameters' JSON Schema (no connection)
  request                            run one JSON request read from stdin
  mcp                                serve the commands as MCP tools on stdio

SCRIPT is --script REF (a reference from inspect, script:3@File.fmp12) or
--script-name NAME. TABLE is --table REF (table:32769@File.fmp12) or
--table-name NAME; TYPE is text, number, date, time or timestamp, and REV
for a field the table's fields_revision from inspect. LAYOUT is --layout REF
(layout:1@File.fmp12) or --layout-name NAME; FIELD is --field REF
(field:32769.2@File.fmp12) or --field-name NAME, a field of the layout's
table; FRAME is LEFT,TOP,WIDTH,HEIGHT in points, and REV for a placement, a
text, a move or a deletion the layout's revision from get-layout. For create-layout,
OCCURRENCE is --occurrence REF (occurrence:13631490@File.fmp12) or
--occurrence-name NAME, LAYOUT the layout whose theme and width the new one
takes (default: the first with a theme), and REV the file's layouts_revision
from inspect. REF for set-object-frames, delete-objects, list-choices, choose-item and
enter-other is a layout object reference from get-layout
(layout_object:1.4@File.fmp12), ID a record id and VALUE an item's stored
value from list-choices. For
complete-calculation, N for --cursor counts characters (default: the end of
the text) and REF is an occurrence reference from inspect
(occurrence:13631490@File.fmp12); the default occurrence is the file's first.
run-script exits 1 when the script's last
error is not 0; a run whose answer is lost is outcome_unknown, and it may
have run.

connection (or the environment variable):
  --config PATH          OPENMAKER_AUTHOR_CONFIG, else user author.json
  --host HOST            OPENMAKER_HOST
  --port PORT            OPENMAKER_PORT (default 5003)
  --file NAME            OPENMAKER_FILE
  --account NAME         OPENMAKER_ACCOUNT
  --password-file PATH   OPENMAKER_PASSWORD_FILE, else OPENMAKER_PASSWORD
License: OPENMAKER_LICENSE_KEY, OPENMAKER_LICENSE_FILE (first line), or
         license_file in author.json.
TLS: OPENMAKER_CA_CERT, OPENMAKER_CLIENT_CERT and OPENMAKER_CLIENT_KEY.
Run om-author config-path to print the selected configuration file path.
Passwords and license keys are never taken as arguments.";

/// The arguments, parsed.
#[derive(Clone, Default)]
struct Args {
    command: Option<String>,
    connection: ConnectionOptions,
    script: Option<String>,
    script_name: Option<String>,
    source: Option<String>,
    expected_revision: Option<String>,
    expected_after_revision: Option<String>,
    include: Option<String>,
    table: Option<String>,
    table_name: Option<String>,
    name: Option<String>,
    field_type: Option<String>,
    comment: Option<String>,
    layout: Option<String>,
    layout_name: Option<String>,
    field: Option<String>,
    field_name: Option<String>,
    /// Every `--field-name` given, in order (place-portal's fields).
    field_names: Vec<String>,
    rows: Option<String>,
    frame: Option<String>,
    label: Option<String>,
    label_frame: Option<String>,
    parameter: Option<String>,
    object: Option<String>,
    /// Every `--object`, in order (`arrange-objects`).
    objects: Vec<String>,
    arrangement: Option<String>,
    record: Option<String>,
    value: Option<String>,
    text: Option<String>,
    cursor: Option<String>,
    occurrence: Option<String>,
    occurrence_name: Option<String>,
    limit: Option<String>,
    path: Option<String>,
    target_file: Option<String>,
    base: Option<String>,
    include_same: bool,
    views: Option<String>,
    default_view: Option<String>,
    in_menus: Option<String>,
    options: Option<String>,
    width: Option<String>,
    control: Option<String>,
    value_list: Option<String>,
    part: Option<String>,
    page_break_after: Option<String>,
    action: Option<String>,
    kind: Option<String>,
    height: Option<String>,
    sorted_by: Option<String>,
}

/// `--options NAME=on|off,...`, as a map of the names to on (true) or off.
fn on_off(
    options: &Option<String>,
) -> Result<Option<std::collections::BTreeMap<String, bool>>, String> {
    let Some(list) = options else {
        return Ok(None);
    };
    list.split(',')
        .map(|item| match item.trim().split_once('=') {
            Some((name, "on")) => Ok((name.to_owned(), true)),
            Some((name, "off")) => Ok((name.to_owned(), false)),
            _ => Err(format!("--options takes OPTION=on or OPTION=off: {item:?}")),
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut raw = raw.into_iter();
    while let Some(arg) = raw.next() {
        let mut value = |name: &str| raw.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--config" => args.connection.config = Some(value(&arg)?.into()),
            "--host" => args.connection.host = Some(value(&arg)?),
            "--port" => args.connection.port = Some(value(&arg)?),
            "--file" => args.connection.file = Some(value(&arg)?),
            "--account" => args.connection.account = Some(value(&arg)?),
            "--password-file" => args.connection.password_file = Some(value(&arg)?.into()),
            "--script" => args.script = Some(value(&arg)?),
            "--script-name" => args.script_name = Some(value(&arg)?),
            "--source" => args.source = Some(value(&arg)?),
            "--expected-revision" => args.expected_revision = Some(value(&arg)?),
            "--expected-after-revision" => args.expected_after_revision = Some(value(&arg)?),
            "--include" => args.include = Some(value(&arg)?),
            "--table" => args.table = Some(value(&arg)?),
            "--table-name" => args.table_name = Some(value(&arg)?),
            "--name" => args.name = Some(value(&arg)?),
            "--views" => args.views = Some(value(&arg)?),
            "--default-view" => args.default_view = Some(value(&arg)?),
            "--in-menus" => args.in_menus = Some(value(&arg)?),
            "--options" => args.options = Some(value(&arg)?),
            "--width" => args.width = Some(value(&arg)?),
            "--control" => args.control = Some(value(&arg)?),
            "--value-list" => args.value_list = Some(value(&arg)?),
            "--part" => args.part = Some(value(&arg)?),
            "--page-break-after" => args.page_break_after = Some(value(&arg)?),
            "--action" => args.action = Some(value(&arg)?),
            "--kind" => args.kind = Some(value(&arg)?),
            "--height" => args.height = Some(value(&arg)?),
            "--sorted-by" => args.sorted_by = Some(value(&arg)?),
            "--type" => args.field_type = Some(value(&arg)?),
            "--comment" => args.comment = Some(value(&arg)?),
            "--layout" => args.layout = Some(value(&arg)?),
            "--object" => {
                let object = value(&arg)?;
                args.objects.push(object.clone());
                args.object = Some(object);
            }
            "--arrangement" => args.arrangement = Some(value(&arg)?),
            "--record" => args.record = Some(value(&arg)?),
            "--value" => args.value = Some(value(&arg)?),
            "--layout-name" => args.layout_name = Some(value(&arg)?),
            "--field" => args.field = Some(value(&arg)?),
            "--field-name" => {
                let name = value(&arg)?;
                args.field_names.push(name.clone());
                args.field_name = Some(name);
            }
            "--rows" => args.rows = Some(value(&arg)?),
            "--frame" => args.frame = Some(value(&arg)?),
            "--label" => args.label = Some(value(&arg)?),
            "--label-frame" => args.label_frame = Some(value(&arg)?),
            "--parameter" => args.parameter = Some(value(&arg)?),
            "--text" => args.text = Some(value(&arg)?),
            "--cursor" => args.cursor = Some(value(&arg)?),
            "--occurrence" => args.occurrence = Some(value(&arg)?),
            "--occurrence-name" => args.occurrence_name = Some(value(&arg)?),
            "--limit" => args.limit = Some(value(&arg)?),
            "--path" => args.path = Some(value(&arg)?),
            "--target" => args.target_file = Some(value(&arg)?),
            "--base" => args.base = Some(value(&arg)?),
            "--include-same" => args.include_same = true,
            "--password" => {
                return Err(
                    "Passwords are not taken as arguments (other users can list them); use \
                     --password-file or OPENMAKER_PASSWORD."
                        .into(),
                )
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            command if args.command.is_none() => args.command = Some(command.to_owned()),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(args)
}

/// The edited text: a file, or stdin for `-`.
fn read_source(path: &Option<String>) -> Result<String, String> {
    let path = path
        .as_deref()
        .ok_or("No --source: give the edited text's file, or - for stdin.")?;
    if path == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("Cannot read stdin: {e}"))?;
        Ok(text)
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("Cannot read {path}: {e}"))
    }
}

fn include(parts: &Option<String>) -> Result<Option<Vec<Part>>, String> {
    let Some(parts) = parts else {
        return Ok(None);
    };
    parts
        .split(',')
        .map(|part| {
            serde_json::from_value(serde_json::Value::String(part.trim().to_owned()))
                .map_err(|_| format!("“{part}” is not one of the metadata parts inspect lists."))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// A new field's name and type.
fn field(args: &Args) -> Result<(String, FieldType), String> {
    let name = args
        .name
        .clone()
        .ok_or("No --name: give the new field's name.")?;
    let text = args
        .field_type
        .as_deref()
        .ok_or("No --type: give text, number, date, time or timestamp.")?;
    let field_type = serde_json::from_value(serde_json::Value::String(text.to_owned()))
        .map_err(|_| format!("“{text}” is not text, number, date, time or timestamp."))?;
    Ok((name, field_type))
}

/// A frame given as LEFT,TOP,WIDTH,HEIGHT.
fn frame(text: Option<&str>, option: &str) -> Result<FrameParams, String> {
    let text = text.ok_or_else(|| format!("No {option}: give LEFT,TOP,WIDTH,HEIGHT in points."))?;
    let numbers: Vec<f64> = text
        .split(',')
        .map(|n| n.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("“{text}” is not LEFT,TOP,WIDTH,HEIGHT in points."))?;
    match numbers.as_slice() {
        &[left, top, width, height] => Ok(FrameParams {
            left,
            top,
            width,
            height,
        }),
        _ => Err(format!("“{text}” is not LEFT,TOP,WIDTH,HEIGHT in points.")),
    }
}

/// The placement the arguments ask for.
fn placement(args: &Args) -> Result<PlacementParams, String> {
    let label = match (&args.label, &args.label_frame) {
        (Some(text), frame_text) => Some(LabelParams {
            text: text.clone(),
            frame: frame(frame_text.as_deref(), "--label-frame")?,
        }),
        (None, Some(_)) => return Err("--label-frame needs --label, the label's text.".into()),
        (None, None) => None,
    };
    Ok(PlacementParams {
        layout: args.layout.clone(),
        layout_name: args.layout_name.clone(),
        field: args.field.clone(),
        field_name: args.field_name.clone(),
        frame: frame(args.frame.as_deref(), "--frame")?,
        label,
        expected_revision: args.expected_revision.clone(),
    })
}

/// A value-list control and record: `--object` and `--record`.
fn choice_target(args: &Args) -> Result<(String, u64), String> {
    let object = args
        .object
        .clone()
        .ok_or("--object is needed: a layout object reference from get-layout.")?;
    let record = args
        .record
        .as_deref()
        .ok_or("--record is needed: a record id.")?
        .parse()
        .map_err(|_| "--record must be a record id (a whole number).")?;
    Ok((object, record))
}

/// The command the arguments ask for (not `request` or `mcp`).
fn command(name: &str, args: &Args) -> Result<Command, String> {
    let spec = catalog::by_cli(name).ok_or_else(|| {
        let names: Vec<&str> = catalog::ALL.iter().map(|spec| spec.cli).collect();
        format!(
            "unknown command {name}; the commands are {}",
            names.join(", ")
        )
    })?;
    let script = ScriptParams {
        script: args.script.clone(),
        script_name: args.script_name.clone(),
    };
    Ok(match spec.id {
        "commands" => Command::Commands,
        "create_file" => Command::CreateFile(api::CreateFileParams {
            path: args.path.clone().ok_or("create-file needs --path PATH")?,
        }),
        "compare_designs" => {
            let needs = "compare-designs needs --source PATH and --target PATH";
            Command::CompareDesigns(api::CompareDesignsParams {
                source: args.source.clone().ok_or(needs)?,
                target: args.target_file.clone().ok_or(needs)?,
                base: args.base.clone(),
                include_same: args.include_same,
            })
        }
        "capabilities" => Command::Capabilities,
        "get_security" => Command::GetSecurity,
        "inspect" => Command::Inspect(InspectParams {
            include: include(&args.include)?,
        }),
        "get_script" => Command::GetScript(script),
        "validate" | "plan" => {
            let params = EditParams {
                script: script.script,
                script_name: script.script_name,
                source: read_source(&args.source)?,
                expected_revision: args.expected_revision.clone(),
            };
            if spec.id == "plan" {
                Command::Plan(params)
            } else {
                Command::Validate(params)
            }
        }
        "apply" => Command::Apply(ApplyParams {
            script: script.script,
            script_name: script.script_name,
            source: read_source(&args.source)?,
            expected_revision: args
                .expected_revision
                .clone()
                .ok_or("apply needs --expected-revision: the revision the text was edited from.")?,
            expected_after_revision: args.expected_after_revision.clone(),
        }),
        "plan_field" => {
            let (name, field_type) = field(args)?;
            Command::PlanField(FieldParams {
                table: args.table.clone(),
                table_name: args.table_name.clone(),
                name,
                field_type,
                comment: args.comment.clone(),
                expected_revision: args.expected_revision.clone(),
            })
        }
        "create_field" => {
            let (name, field_type) = field(args)?;
            Command::CreateField(CreateFieldParams {
                table: args.table.clone(),
                table_name: args.table_name.clone(),
                name,
                field_type,
                comment: args.comment.clone(),
                expected_revision: args.expected_revision.clone().ok_or(
                    "create-field needs --expected-revision: the table's fields_revision from \
                     inspect.",
                )?,
            })
        }
        "get_layout" => Command::GetLayout(LayoutParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
        }),
        "plan_field_placement" => Command::PlanFieldPlacement(placement(args)?),
        "place_field" => {
            let plan = placement(args)?;
            Command::PlaceField(PlaceFieldParams {
                layout: plan.layout,
                layout_name: plan.layout_name,
                field: plan.field,
                field_name: plan.field_name,
                frame: plan.frame,
                label: plan.label,
                expected_revision: plan.expected_revision.ok_or(
                    "place-field needs --expected-revision: the layout's revision from \
                     get-layout.",
                )?,
            })
        }
        "set_object_frames" => Command::SetObjectFrames(ObjectFramesParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            objects: vec![ObjectFrameParams {
                object: args.object.clone().ok_or(
                    "set-object-frames needs --object: a layout object reference from \
                     get-layout.",
                )?,
                frame: frame(args.frame.as_deref(), "--frame")?,
            }],
            expected_revision: args.expected_revision.clone().ok_or(
                "set-object-frames needs --expected-revision: the layout's revision from \
                 get-layout.",
            )?,
        }),
        "arrange_objects" => Command::ArrangeObjects(api::ArrangeObjectsParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            objects: if args.objects.is_empty() {
                args.object.iter().cloned().collect()
            } else {
                args.objects.clone()
            },
            arrangement: args.arrangement.clone().ok_or(
                "arrange-objects needs --arrangement: align_left_edges, distribute_horizontally, \
                 resize_to_largest_width or another arrangement id.",
            )?,
            expected_revision: args.expected_revision.clone().ok_or(
                "arrange-objects needs --expected-revision: the layout's revision from \
                 get-layout.",
            )?,
        }),
        "place_text" => Command::PlaceText(PlaceTextParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            text: args
                .text
                .clone()
                .ok_or("place-text needs --text: the text, one line.")?,
            frame: frame(args.frame.as_deref(), "--frame")?,
            expected_revision: args.expected_revision.clone().ok_or(
                "place-text needs --expected-revision: the layout's revision from get-layout.",
            )?,
        }),
        "place_portal" => Command::PlacePortal(PlacePortalParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            occurrence: args.occurrence.clone(),
            occurrence_name: args.occurrence_name.clone(),
            field_names: if args.field_names.is_empty() {
                args.field_name.clone().into_iter().collect()
            } else {
                args.field_names.clone()
            },
            rows: args
                .rows
                .as_deref()
                .ok_or("place-portal needs --rows: how many rows the portal shows.")?
                .parse()
                .map_err(|_| "--rows takes a whole number of rows.")?,
            frame: frame(args.frame.as_deref(), "--frame")?,
            expected_revision: args.expected_revision.clone().ok_or(
                "place-portal needs --expected-revision: the layout's revision from get-layout.",
            )?,
        }),
        "delete_objects" => Command::DeleteObjects(DeleteObjectsParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            objects: vec![args.object.clone().ok_or(
                "delete-objects needs --object: a layout object reference from get-layout.",
            )?],
            expected_revision: args.expected_revision.clone().ok_or(
                "delete-objects needs --expected-revision: the layout's revision from \
                 get-layout.",
            )?,
        }),
        "layout_setup" => Command::LayoutSetup(LayoutSetupParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "layout-setup needs --expected-revision: the layout's revision from get-layout.",
            )?,
            name: args.name.clone(),
            views: args
                .views
                .as_ref()
                .map(|views| views.split(',').map(|v| v.trim().to_owned()).collect()),
            default_view: args.default_view.clone(),
            include_in_layout_menus: match args.in_menus.as_deref() {
                None => None,
                Some("yes") => Some(true),
                Some("no") => Some(false),
                Some(_) => return Err("--in-menus takes yes or no.".into()),
            },
            options: on_off(&args.options)?,
        }),
        "change_parts" => Command::ChangeParts(ChangePartsParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "change-parts needs --expected-revision: the layout's revision from get-layout.",
            )?,
            action: args
                .action
                .clone()
                .ok_or("change-parts needs --action: add, delete or resize.")?,
            part: match args.part.as_deref() {
                None => None,
                Some(part) => Some(
                    part.parse()
                        .map_err(|_| "--part takes a part's id from get-layout.")?,
                ),
            },
            kind: args.kind.clone(),
            height: match args.height.as_deref() {
                None => None,
                Some(height) => Some(
                    height
                        .parse()
                        .map_err(|_| "--height takes a height in points.")?,
                ),
            },
        }),
        "set_part_options" => Command::SetPartOptions(PartOptionsParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "set-part-options needs --expected-revision: the layout's revision from \
                 get-layout.",
            )?,
            part: args
                .part
                .as_deref()
                .and_then(|part| part.parse().ok())
                .ok_or("set-part-options needs --part: a part's id from get-layout.")?,
            options: on_off(&args.options)?,
            page_break_after: match args.page_break_after.as_deref() {
                None => None,
                Some(n) => Some(
                    n.parse()
                        .map_err(|_| "--page-break-after takes a count, 0 for none.")?,
                ),
            },
            sorted_by: args.sorted_by.clone(),
        }),
        "set_text" => Command::SetText(TextParams {
            object: args
                .object
                .clone()
                .ok_or("set-text needs a text object reference from get-layout.")?,
            expected_revision: args.expected_revision.clone().ok_or(
                "set-text needs --expected-revision: the layout's revision from get-layout.",
            )?,
            text: args
                .text
                .clone()
                .ok_or("set-text needs --text, the new words.")?,
        }),
        "set_field_control" => Command::SetFieldControl(FieldControlParams {
            object: args
                .object
                .clone()
                .ok_or("set-field-control needs a field object reference from get-layout.")?,
            expected_revision: args.expected_revision.clone().ok_or(
                "set-field-control needs --expected-revision: the layout's revision from \
                 get-layout.",
            )?,
            control: args
                .control
                .clone()
                .ok_or("set-field-control needs --control, as edit_box or pop_up_menu.")?,
            value_list: args.value_list.clone(),
        }),
        "set_layout_width" => Command::SetLayoutWidth(LayoutWidthParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "set-layout-width needs --expected-revision: the layout's revision from \
                 get-layout.",
            )?,
            width: args
                .width
                .as_deref()
                .ok_or("set-layout-width needs --width: the width in points.")?
                .parse()
                .map_err(|_| "--width takes a number of points.")?,
        }),
        "delete_layout" => Command::DeleteLayout(DeleteLayoutParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "delete-layout needs --expected-revision: the file's layouts_revision from \
                 inspect.",
            )?,
        }),
        "duplicate_layout" => Command::DuplicateLayout(DuplicateLayoutParams {
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "duplicate-layout needs --expected-revision: the file's layouts_revision from \
                 inspect.",
            )?,
        }),
        "create_layout" => Command::CreateLayout(CreateLayoutParams {
            name: args
                .name
                .clone()
                .ok_or("create-layout needs --name: the new layout's name.")?,
            occurrence: args.occurrence.clone(),
            occurrence_name: args.occurrence_name.clone(),
            layout: args.layout.clone(),
            layout_name: args.layout_name.clone(),
            expected_revision: args.expected_revision.clone().ok_or(
                "create-layout needs --expected-revision: the file's layouts_revision from \
                 inspect.",
            )?,
        }),
        "run_script" => Command::RunScript(RunScriptParams {
            script: script.script,
            script_name: script.script_name,
            parameter: args.parameter.clone(),
            expected_revision: args.expected_revision.clone(),
        }),
        "list_choices" => {
            let (object, record) = choice_target(args)?;
            Command::ListChoices(ChoiceParams { object, record })
        }
        "choose_value_list_item" => {
            let (object, record) = choice_target(args)?;
            Command::ChooseValueListItem(ChooseParams {
                object,
                record,
                value: args
                    .value
                    .clone()
                    .ok_or("choose-item needs --value: an item's stored value.")?,
            })
        }
        "get_value_list_values" => Command::GetValueListValues(ValueListParams {
            value_list: args
                .value_list
                .clone()
                .ok_or("get-value-list-values needs --value-list, from get-value-lists.")?,
        }),
        "set_value_list_values" => Command::SetValueListValues(ValueListValuesParams {
            value_list: args
                .value_list
                .clone()
                .ok_or("set-value-list-values needs --value-list, from get-value-lists.")?,
            values: read_source(&args.source)?
                .lines()
                .map(str::to_owned)
                .collect(),
            expected_revision: args.expected_revision.clone().ok_or(
                "set-value-list-values needs --expected-revision: the list's revision from \
                 get-value-list-values.",
            )?,
        }),
        "enter_other_value" => {
            let (object, record) = choice_target(args)?;
            Command::EnterOtherValue(OtherParams {
                object,
                record,
                text: args
                    .value
                    .clone()
                    .ok_or("enter-other needs --value: the text to enter.")?,
            })
        }
        "get_value_lists" => Command::GetValueLists(NoParams {}),
        "complete_calculation" => {
            let number = |option: &Option<String>, name: &str| {
                option
                    .as_deref()
                    .map(|n| n.parse::<usize>())
                    .transpose()
                    .map_err(|_| format!("{name} must be a whole number."))
            };
            Command::CompleteCalculation(CompletionParams {
                text: match (&args.text, &args.source) {
                    (Some(text), None) => text.clone(),
                    (None, Some(_)) => read_source(&args.source)?,
                    _ => {
                        return Err("complete-calculation needs the formula as either --text \
                                    TEXT or --source FILE (- for stdin)."
                            .into())
                    }
                },
                cursor: number(&args.cursor, "--cursor")?,
                occurrence: args.occurrence.clone(),
                occurrence_name: args.occurrence_name.clone(),
                limit: number(&args.limit, "--limit")?,
            })
        }
        _ => return Err(format!("{name} has no command-line form yet")),
    })
}

/// The response to `command`: from the host, through the service, unless
/// the command needs no connection.
fn respond(command: &Command, args: &Args) -> Result<serde_json::Value, String> {
    Ok(match handle_offline(command) {
        Some(response) => response,
        None => Service::new(args.connection.target()?).handle(command),
    })
}

fn run() -> Result<ExitCode, String> {
    let raw: Vec<_> = std::env::args().skip(1).collect();
    if matches!(raw.as_slice(), [flag] if flag == "--help" || flag == "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let args = parse_args(raw)?;
    let name = match args.command.as_deref() {
        None | Some("help") => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Some("--version" | "version") => {
            println!(
                "om-author {} ({})",
                env!("CARGO_PKG_VERSION"),
                om_author::API_VERSION
            );
            return Ok(ExitCode::SUCCESS);
        }
        Some(name) => name.to_owned(),
    };
    let response = match name.as_str() {
        "config-path" => {
            let path = args.connection.config_path().ok_or(
                "No user config directory is available. Give --config or set OPENMAKER_AUTHOR_CONFIG.",
            )?;
            println!("{}", path.display());
            return Ok(ExitCode::SUCCESS);
        }
        "mcp" => {
            om_author::mcp::serve_stdio_lazy(move || args.connection.target().map(Service::new))?;
            return Ok(ExitCode::SUCCESS);
        }
        "request" => {
            let mut request = String::new();
            std::io::stdin()
                .read_to_string(&mut request)
                .map_err(|e| format!("Cannot read the request from stdin: {e}"))?;
            match api::parse_request(&request) {
                Ok(command) => respond(&command, &args)?,
                Err(error) => api::response("", Err(error)),
            }
        }
        name => respond(&command(name, &args)?, &args)?,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&response).expect("JSON values serialize")
    );
    // A valid validate, and a run whose script ended without an error.
    let succeeded = response["ok"] == serde_json::Value::Bool(true)
        && response["result"]["valid"] != serde_json::Value::Bool(false)
        && response["result"]["last_error"]
            .as_i64()
            .is_none_or(|error| error == 0);
    Ok(if succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("om-author: {message}");
            eprintln!("Run om-author help for usage.");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command of the catalog has a command line, listed in the
    /// usage, that builds that command.
    #[test]
    fn every_command_has_a_command_line() {
        let source = std::env::temp_dir().join(format!("om-author-cli-{}", std::process::id()));
        std::fs::write(&source, "").unwrap();
        let args = Args {
            script_name: Some("S".into()),
            source: Some(source.to_string_lossy().into_owned()),
            expected_revision: Some("r".into()),
            table_name: Some("T".into()),
            name: Some("F".into()),
            field_type: Some("text".into()),
            layout_name: Some("L".into()),
            field_name: Some("F".into()),
            frame: Some("10,60,120,20".into()),
            rows: Some("3".into()),
            object: Some("layout_object:1.4@F.fmp12".into()),
            record: Some("1".into()),
            value: Some("v".into()),
            width: Some("900".into()),
            control: Some("pop_up_menu".into()),
            part: Some("2".into()),
            action: Some("resize".into()),
            height: Some("100".into()),
            path: Some("New.fmp12".into()),
            target_file: Some("B.fmp12".into()),
            value_list: Some("value_list:1@F.fmp12".into()),
            arrangement: Some("align_left_edges".into()),
            ..Args::default()
        };
        // A text to place, where `--text` is not a formula's.
        let text = Args {
            text: Some("T".into()),
            source: None,
            ..args.clone()
        };
        for spec in catalog::ALL {
            let built = command(
                spec.cli,
                if matches!(spec.cli, "place-text" | "set-text") {
                    &text
                } else {
                    &args
                },
            );
            assert!(
                built.is_ok_and(|command| std::ptr::eq(command.spec(), spec)),
                "{}",
                spec.cli
            );
            assert!(
                USAGE.contains(&format!("\n  {} ", spec.cli)),
                "{}",
                spec.cli
            );
        }
        std::fs::remove_file(source).unwrap();
    }
}
