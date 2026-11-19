#!/usr/bin/env python3
"""Probe files for FileMaker Pro's desktop answers (calc-functions.md 6.2,
"Open on FileMaker Pro" O1-O6), built with FileMaker's own tools.

    python3 -I desktop_probe.py contacts contacts-en.xml > Contacts.xml
    python3 -I desktop_probe.py tasks tasks-en.xml > Tasks.xml
    python3 -I desktop_probe.py plan > plan.json

The inputs are FMDeveloperTool --saveAsXML exports of the Contacts and Tasks
starter templates; the outputs are schema documents for FMUpgradeTool
--generateDBFile (make-desktop-probe.sh) and the run plan for
`runner.py --scripts`. Each document is the template's schema with:

- a global text field `zOut` in the main table, where every probe script
  leaves its answers (read back through Apple events);
- the fmextscriptaccess extended privilege for [Full Access], which Apple
  events need;
- (Contacts) the web viewer's address calculation of O5, which answers a
  `data:` URL (never a network address), an unstored calculation `zPage`
  (`Get ( PageNumber ) & "/" & Get ( PageCount )`) and a footer on Contact
  List with a text object `pageMerge` holding the `{{PageNumber}}` merge
  and a field object `pageCalc` showing `zPage` (O4);
- the probe scripts. Each one starts from the same state (Browse mode, all
  records unsorted, the status toolbar shown, zoom 100 %, the window at
  left 100, top 120, 1100 x 900), reaches one state with script steps only
  and appends one JSON line per expression to `$r`:
  `{"s": state, "x": expression, "t": GetAsText ( Evaluate ( x ) ), "e":
  EvaluationError ( Evaluate ( x ) )}`. Its last step writes a header line
  (script name, `Get ( CurrentTimeUTCMilliseconds )`) and `$r` to `zOut`.

No step needs keyboard or mouse input, a dialog or the network. Scripts use
no control flow (FMUpgradeTool stores block indexes as 0; see the script
harness README), so loops are unrolled here.
"""
import copy
import json
import sys
import uuid
import xml.etree.ElementTree as ET
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "fms-script-harness"))
import make_patch  # noqa: E402

NS = uuid.UUID("6f6d2d64-6573-6b74-6f70-70726f626521")
WINDOW = "Move/Resize Window [ Current ; Height: 900 ; Width: 1100 ; Top: 120 ; Left: 100 ]"
WEB_VIEWER_CALC = ('If ( LayoutObjectUUID = GetLayoutObjectAttribute ( "website" ; "UUID" ) ; '
                   '"data:text/html,1" ; "data:text/html,0" )')
PAGE_MERGE = "{{PageNumber}}"
PAGE_CALC = 'Get ( PageNumber ) & "/" & Get ( PageCount )'


def uid(label):
    return str(uuid.uuid5(NS, label)).upper()


def lit(text):
    """A FileMaker string literal."""
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("¶", "\\¶") + '"'


def measure(state, exprs):
    """Steps that append one JSON line per expression to $r."""
    out = []
    for x in exprs:
        e = lit(x)
        out.append(f'Set Variable [ $r ; Value: $r & JSONSetElement ( "{{}}" ; [ "s" ; {lit(state)} ; JSONString ] ; '
                   f'[ "x" ; {e} ; JSONString ] ; [ "t" ; GetAsText ( Evaluate ( {e} ) ) ; JSONString ] ; '
                   f'[ "e" ; EvaluationError ( Evaluate ( {e} ) ) ; JSONNumber ] ) & ¶ ]')
    return out


def finish(table):
    return ["Revert Record/Request [ With dialog: Off ]", "Enter Browse Mode [ Pause: Off ]", "Show All Records",
            f'Set Field [ {table}::zOut ; JSONSetElement ( "{{}}" ; [ "script" ; Get ( ScriptName ) ; JSONString ] ; '
            '[ "stamp" ; Get ( CurrentTimeUTCMilliseconds ) ; JSONNumber ] ) & ¶ & $r ]']


def start(layout, view=None):
    steps = ["Set Error Capture [ On ]", "Enter Browse Mode [ Pause: Off ]", "Revert Record/Request [ With dialog: Off ]",
             f'Go to Layout [ "{layout}" ]']
    steps += [f"View As [ {view} ]"] if view else []
    steps += ["Show All Records", "Unsort Records", "Go to Record/Request/Page [ First ]", "Show/Hide Toolbars [ Show ]",
              "Set Zoom Level [ 100% ]", WINDOW]
    if layout == "Contact Details":
        # Company is on the first tab panel: focusing it brings that panel
        # (and the web viewer) to the front after the portal probes.
        steps += ["Go to Field [ Contacts::Company ]", "Go to Field [ ]"]
    return steps


def get(*names):
    return [f"Get ( {n} )" for n in names]


O1 = get("FileSize", "SystemLanguage", "ScreenDepth", "ScreenHeight", "ScreenWidth", "SystemVersion", "UserCount",
         "UserName", "CurrentPrivilegeSetName", "AccountPrivilegeSetName", "CurrentExtendedPrivileges",
         "AccountExtendedPrivileges", "AccountGroupName", "AccountPasswordDaysRemaining", "LayoutAccess",
         "RecordAccess", "HighContrastState", "SystemAppearance", "StatusAreaState", "WindowHeight", "WindowLeft",
         "WindowTop", "WindowVisible", "WindowWidth", "WindowDesktopHeight", "WindowDesktopWidth",
         "WindowContentHeight", "WindowContentWidth", "WindowZoomLevel", "WindowStyle", "WindowOrientation",
         "WindowUUID", "MenubarState", "TextRulerVisible", "AllowFormattingBarState", "CustomMenuSetName",
         "SystemNICAddress", "SystemIPAddress", "HostIPAddress", "HostApplicationVersion") + [
    "Get ( CurrentHostTimestamp ) - Get ( CurrentTimestamp )"] + get(
    "PersistentID", "ConnectionState", "ConnectionAttributes", "EncryptionState", "ScreenScaleFactor") + [
    "Get ( SystemStorageAvailable ) > 0"] + get(
    "FileMakerPath", "PreferencesPath", "SystemDrive", "DocumentsPathListing", "InstalledFMPluginsAsJSON",
    "InstalledFMPlugins", "CacheFileName", "CacheFilePath", "SystemLocaleElements", "FileLocaleElements",
    "ApplicationLanguage") + [
    "Length ( Get ( UUIDNumber ) )", "Get ( UUIDNumber )", "Get ( UUIDNumber )"] + get(
    "HostName", "FilePath", "DocumentsPath", "TemporaryPath", "DesktopPath", "SystemPlatform", "ApplicationVersion",
    "ApplicationArchitecture", "Device", "WindowName", "WindowMode", "LayoutName", "LayoutViewState", "AccountName",
    "AccountType", "MultiUserState", "NetworkProtocol", "NetworkType", "DBEngineVersion", "StudioDatasource",
    "LayoutCount", "LayoutNumber", "LayoutTableName", "FoundCount", "RecordNumber", "TotalRecordCount",
    "ActiveFieldName", "SortState", "UseSystemFormatsState", "OpenDataFileInfo", "SessionIdentifier",
    "ErrorCaptureState", "AllowAbortState", "ScriptAnimationState", "TransactionOpenState",
    "RevertTransactionOnErrorState", "TouchKeyboardState", "GuidedAccessState", "RegionMonitorEvents",
    "LastStepTokensUsed")
WINDOW_SIZES = get("StatusAreaState", "WindowTop", "WindowLeft", "WindowHeight", "WindowWidth", "WindowContentHeight",
                   "WindowContentWidth", "WindowDesktopHeight", "WindowDesktopWidth", "WindowZoomLevel")
O2 = get("ActiveFieldName", "ActiveFieldTableName", "ActiveFieldContents", "ActiveRepetitionNumber",
         "ActiveSelectionStart", "ActiveSelectionSize", "ActiveModifierKeys", "ActiveLayoutObjectName",
         "ActivePortalRowNumber", "RecordOpenState", "RecordOpenCount", "ModifiedFields")
TRIGGERS = ["Length ( Get ( TriggerCurrentPanel ) )", "Code ( Get ( TriggerCurrentPanel ) )",
            "Length ( Get ( TriggerTargetPanel ) )", "Code ( Get ( TriggerTargetPanel ) )"] + get(
    "TriggerCurrentPanel", "TriggerTargetPanel", "TriggerKeystroke", "TriggerModifierKeys", "TriggerExternalEvent",
    "TriggerGestureInfo")


def attr(obj, a, *more):
    extra = "".join(f" ; {m}" for m in more)
    return f'GetLayoutObjectAttribute ( "{obj}" ; "{a}"{extra} )'


BOUNDS = [attr("First Name", "bounds"), attr("Company", "bounds"), attr("website", "bounds"),
          attr("Company", "bounds", 2), attr("Back to Contacts", "bounds")]
FIRST_NAME = [attr("First Name", a) for a in ("content", "isActive", "hasFocus", "containsFocus", "source")]
WEB = [attr("website", a) for a in ("source", "content", "objectType", "isActive", "isObjectHidden", "UUID")] + [
    'GetLayoutObjectOwnerInfo ( GetLayoutObjectAttribute ( "website" ; "UUID" ) )',
    'GetLayoutObjectOwnerInfo ( GetLayoutObjectAttribute ( "First Name" ; "UUID" ) )',
    'GetLayoutObjectOwnerInfo ( Lower ( GetLayoutObjectAttribute ( "website" ; "UUID" ) ) )',
    'GetLayoutObjectOwnerInfo ( Substitute ( GetLayoutObjectAttribute ( "website" ; "UUID" ) ; "-" ; "" ) )',
    "LayoutObjectUUID", attr("First Name", "UUID")]
PAGES = get("LastError", "WindowMode", "PageNumber", "PageCount", "RecordNumber", "FoundCount") + [
    attr("pageMerge", "content"), attr("pageMerge", "bounds"), "Contacts::zPage", attr("pageCalc", "content")]
EMPTY = get("LastError", "FoundCount", "RecordID", "RecordModificationCount", "ActiveRecordNumber", "RecordNumber",
            "WindowMode", "RecordOpenState", "TotalRecordCount", "SortState") + [attr("First Name", "content")]
OPEN = get("LastError", "RecordOpenState", "RecordOpenCount", "ModifiedFields", "ActiveFieldName")
PAUSE = "Pause/Resume Script [ Duration: 2 ]"
NAMES = ["Alice Archer", "Ann Baker"] + [f"Name{i:02} Probe{i:02}" for i in range(3, 41)]


def contacts_scripts():
    t = "Contacts"
    setup = ["Set Error Capture [ On ]", 'Go to Layout [ "Contact Details" ]', WINDOW]
    for n in NAMES:
        first, last = n.split()
        setup += ["New Record/Request", f'Set Field [ Contacts::First Name ; "{first}" ]',
                  f'Set Field [ Contacts::Last Name ; "{last}" ]', 'Set Field [ Contacts::Company ; "Probe Co" ]',
                  "Commit Records/Requests [ With dialog: Off ]"]
    setup += ["Go to Record/Request/Page [ First ]", 'Set Field [ Phone Numbers::Number ; "555-0101" ]',
              "Commit Records/Requests [ With dialog: Off ]", "Go to Field [ Phone Numbers::Number ]",
              "Go to Portal Row [ Select: Off ; By Calculation: 2 ]", 'Set Field [ Phone Numbers::Number ; "555-0102" ]',
              "Commit Records/Requests [ With dialog: Off ]"]
    setup += measure("setup", get("LastError", "TotalRecordCount", "FoundCount") + [
        "Count ( Phone Numbers::Number )", "List ( Phone Numbers::Number )"])
    s = {"P0 Setup": setup + finish(t)}
    cd = start("Contact Details")
    s["O1 Toolbar shown"] = cd + measure("toolbar shown", O1) + finish(t)
    s["O1 Toolbar hidden"] = cd + ["Show/Hide Toolbars [ Hide ]"] + measure("toolbar hidden", WINDOW_SIZES) + finish(t)
    corner = get("LastError", "WindowTop", "WindowLeft", "WindowHeight", "WindowWidth") + [attr("Back to Contacts", "bounds")]
    s["O1 Window corner"] = cd + ["Move/Resize Window [ Current ; Top: 0 ; Left: 0 ]", PAUSE] + measure(
        "window moved to top 0, left 0", corner) + ["Move/Resize Window [ Current ; Top: 40 ; Left: 10 ]", PAUSE] + measure(
        "window moved to top 40, left 10", corner) + finish(t)
    s["O2 First Name typed"] = cd + [
        "Go to Field [ Select: On ; Contacts::First Name ]", 'Insert Text [ "Ann" ]',
        "Set Selection [ Contacts::First Name ; 2 ; 3 ]"] + measure(
        "First Name: Ann typed, nn selected", get("LastError") + O2 + FIRST_NAME) + ["Go to Field [ ]"] + measure(
        "First Name: Ann typed, field left", OPEN) + [
        # leaving the field committed the record: put the name back
        'Set Field [ Contacts::First Name ; "Alice" ]', "Commit Records/Requests [ With dialog: Off ]"] + finish(t)
    s["O2 Phone portal row 2"] = cd + [
        "Go to Field [ Phone Numbers::Number ]", "Go to Portal Row [ Select: Off ; By Calculation: 2 ]"] + measure(
        "Phone portal row 2", get("LastError") + O2 + ["Count ( Phone Numbers::Number )"]) + finish(t)
    s["O2 No field"] = cd + measure("no active field", O2 + FIRST_NAME + TRIGGERS) + finish(t)
    cl = start("Contact List")
    for view in ("List", "Table", "Form"):
        s[f"O3 Contact List as {view}"] = cl + [f"View As [ {view} ]"] + measure(
            f"Contact List as {view}", get("LastError", "LayoutViewState", "LayoutName", "SortState", "FoundCount",
                                           "RecordNumber", "PageNumber", "PageCount") + [
                attr("pageMerge", "content"), attr("pageCalc", "content"), "Contacts::zPage"]) + finish(t)
    find = get("RequestCount", "RequestOmitState", "WindowMode", "RecordNumber", "ActiveRecordNumber", "FoundCount")
    s["O3 Find requests"] = cd + [
        "Enter Find Mode [ Pause: Off ]", 'Set Field [ Contacts::First Name ; "Ann" ]', "New Record/Request",
        'Set Field [ Contacts::Last Name ; "Baker" ]', "Omit Record"] + measure(
        "Find mode, request 2 of 2 (omit)", get("LastError") + find) + [
        "Go to Record/Request/Page [ First ]"] + measure("Find mode, request 1 of 2", find) + [
        "Enter Browse Mode [ Pause: Off ]"] + measure("Browse mode, after Find mode without a find", find) + finish(t)
    s["O3 Set Field"] = cd + ['Set Field [ Contacts::First Name ; "Zoe" ]'] + measure(
        "Set Field into First Name, not committed", OPEN) + finish(t)
    sort = get("LastError", "SortState", "FoundCount", "RecordNumber")
    s["O3 Sort"] = cd + measure("unsorted", sort) + ["Sort Records [ Contacts::Last Name ]"] + measure(
        "after Sort Records", sort) + ["New Record/Request"] + measure("sorted, new record open", sort) + [
        "Revert Record/Request [ With dialog: Off ]"] + measure("sorted, new record reverted", sort) + finish(t)
    s["O3 Open record"] = cd + [
        "New Record/Request", "Go to Field [ Select: On ; Contacts::First Name ]", 'Insert Text [ "Zoe" ]'] + measure(
        "new record, Zoe typed, not committed", get("LastError") + O2 + get(
            "RecordID", "RecordModificationCount", "FoundCount", "RecordNumber")) + [
        "Revert Record/Request [ With dialog: Off ]"] + measure(
        "after Revert Record", get("LastError", "RecordOpenState", "RecordOpenCount", "ModifiedFields",
                                   "FoundCount")) + finish(t)
    s["O3 Open record field left"] = cd + [
        "New Record/Request", "Go to Field [ Select: On ; Contacts::First Name ]", 'Insert Text [ "Zoe" ]',
        "Go to Field [ ]"] + measure("new record, Zoe typed, field left", OPEN + get("FoundCount")) + [
        # leaving the field committed the new record: delete it again
        "Delete Record/Request [ With dialog: Off ]"] + measure(
        "after Delete Record", get("LastError", "FoundCount", "TotalRecordCount")) + finish(t)
    s["O3 Quick Find"] = cd + ['Perform Quick Find [ "Ann" ]'] + measure(
        "after Perform Quick Find Ann", get("LastError", "QuickFindText", "FoundCount", "RecordNumber", "WindowMode")
    ) + finish(t)
    s["O3 Empty found set"] = cd + [
        "Enter Find Mode [ Pause: Off ]", 'Set Field [ Contacts::First Name ; "Qqqzz" ]', "Perform Find"] + measure(
        "empty found set (Perform Find, no match)", EMPTY + get("QuickFindText")) + finish(t)
    pdf = ["Set Variable [ $pdf ; Value: \"file:\" & Get ( TemporaryPath ) & \"o4-contact-list.pdf\" ]",
           "Save Records as PDF [ $pdf ]"]
    s["O4 Preview"] = cl + ["Enter Preview Mode [ Pause: Off ]"] + measure("Preview, page 1", PAGES + get(
        "PrinterName")) + ["Go to Record/Request/Page [ By Calculation: 2 ]"] + measure("Preview, page 2", PAGES) + [
        "Go to Record/Request/Page [ Last ]"] + measure("Preview, last page", PAGES) + pdf + measure(
        "pdf saved", get("LastError")) + measure("pdf", ["ConvertFromFileMakerPath ( $pdf ; PosixPath )"]) + finish(t)
    geometry = BOUNDS + FIRST_NAME[:3] + WINDOW_SIZES
    pause = PAUSE
    for zoom in ("100%", "150%"):
        for bar in ("Show", "Hide"):
            label = f"zoom {zoom}, toolbar {'shown' if bar == 'Show' else 'hidden'}"
            s[f"O5 Zoom {zoom[:-1]} toolbar {bar.lower()}"] = cd + [f"Set Zoom Level [ {zoom} ]", f"Show/Hide Toolbars [ {bar} ]",
                                                                 pause] + measure(label, geometry) + finish(t)
    s["O5 Web viewer"] = cd + [pause] + measure("web viewer from its layout calculation", WEB) + [
        'Set Web Viewer [ Object: "website" ; Go to URL: "data:text/html,<p>probe</p>" ]', pause] + measure(
        "after Set Web Viewer Go to URL", get("LastError") + WEB[:2]) + finish(t)
    s["O5 First Name active"] = cd + ["Go to Field [ Contacts::First Name ]"] + measure(
        "First Name active", get("LastError") + FIRST_NAME + BOUNDS[:1] + get("ActiveLayoutObjectName")) + finish(t)
    s["O6 From Contacts"] = cd + measure("from Contacts, Tasks open", [
        "DatabaseNames", "WindowNames", 'WindowNames ( "Tasks" )', 'TableNames ( "Tasks" )', 'LayoutNames ( "Tasks" )',
        'FieldNames ( "Tasks" ; "Task List" )', "Get ( FileName )", "Get ( WindowName )"]) + finish(t)
    return s


def tasks_scripts():
    t = "Tasks"
    setup = ["Set Error Capture [ On ]", 'Go to Layout [ "Task Details" ]', WINDOW]
    for i in range(1, 4):
        setup += ["New Record/Request", f'Set Field [ Tasks::Task ; "Probe task {i}" ]',
                  "Commit Records/Requests [ With dialog: Off ]"]
    s = {"T0 Setup": setup + measure("setup", get("LastError", "TotalRecordCount")) + finish(t)}
    tl = start("Task List", "List")
    details = [attr("Go to details.", "bounds", *a) for a in ((), (0, 1), (0, 2), (0, 3), (0, 4), (1, 2))]
    ctx = get("FoundCount", "RecordNumber", "LayoutViewState", "LayoutName")
    s["O5 Task List"] = tl + measure("Task List, record 1, no pause", details + ctx) + [PAUSE] + measure(
        "Task List, record 1", details + ctx) + ["Go to Record/Request/Page [ Next ]", PAUSE] + measure(
        "Task List, record 2", details + ctx) + finish(t)
    s["O6 From Tasks"] = tl + measure("from Tasks, Contacts open", [
        "DatabaseNames", "WindowNames", 'WindowNames ( "Contacts" )', 'WindowNames ( "Tasks" )',
        'TableNames ( "Contacts" )', 'TableNames ( "contacts" )', 'TableNames ( "CONTACTS.FMP12" )',
        'TableNames ( " Contacts" )', 'LayoutNames ( "Contacts.fmp12" )', 'LayoutNames ( "Contacts" )',
        'FieldNames ( "Contacts" ; "Contact Details" )', 'FieldNames ( "Contacts" ; "Contacts" )',
        'LayoutIDs ( "Contacts" )', 'TableIDs ( "Contacts" )', "Get ( FileName )", "Get ( WindowName )"]) + finish(t)
    return s


def add_output_field(root, table):
    for fc in root.iter("FieldCatalog"):
        if fc.find("BaseTableReference").get("name") == table:
            break
    objs = fc.find("ObjectList")
    fid = str(max(int(f.get("id")) for f in objs) + 1)
    f = ET.SubElement(objs, "Field", {"id": fid, "name": "zOut", "fieldtype": "Normal", "datatype": "Text",
                                      "comment": "OpenMaker desktop probe answers"})
    ET.SubElement(f, "UUID").text = uid(f"{table} zOut")
    ET.SubElement(f, "AutoEnter", {"type": "", "prohibitModification": "False"})
    ET.SubElement(f, "Validation", {"alwaysValidate": "False", "type": "OnlyDuringDataEntry", "allowOverride": "True",
                                    "notEmpty": "False", "unique": "False", "existing": "False"})
    st = ET.SubElement(f, "Storage", {"autoIndex": "False", "index": "None", "global": "True", "maxRepetitions": "1"})
    ET.SubElement(st, "LanguageReference", {"name": "English", "id": "21"})
    ET.SubElement(f, "TagList")
    objs.set("membercount", str(len(objs)))
    return fid


def add_page_field(root):
    """An unstored calculation zPage in Contacts, shown on the Contact List
    footer (O4: Get ( PageNumber ) and Get ( PageCount ) in a layout)."""
    for fc in root.iter("FieldCatalog"):
        if fc.find("BaseTableReference").get("name") == "Contacts":
            break
    objs = fc.find("ObjectList")
    fid = str(max(int(f.get("id")) for f in objs) + 1)
    f = ET.SubElement(objs, "Field", {"id": fid, "name": "zPage", "fieldtype": "Calculated", "datatype": "Text",
                                      "comment": "OpenMaker desktop probe: page number and count"})
    ET.SubElement(f, "UUID").text = uid("Contacts zPage")
    ET.SubElement(f, "AutoEnter", {"alwaysEvaluate": "False"})
    st = ET.SubElement(f, "Storage", {"storeCalculationResults": "False", "autoIndex": "True", "index": "None",
                                      "global": "False", "maxRepetitions": "1"})
    ET.SubElement(st, "LanguageReference", {"name": "English", "id": "21"})
    c = ET.SubElement(f, "Calculation")
    ET.SubElement(c, "TableOccurrenceReference", {"id": "1065090", "name": "Contacts", "UUID": ""})
    ET.SubElement(c, "Text").text = PAGE_CALC
    ET.SubElement(f, "TagList")
    objs.set("membercount", str(len(objs)))
    return fid


def allow_apple_events(root):
    for ep in root.iter("ExtendedPrivilege"):
        if ep.get("name") == "fmextscriptaccess":
            ol = ep.find("ObjectList")
            if ol is None:
                ol = ET.SubElement(ep, "ObjectList")
            ET.SubElement(ol, "PrivilegeSetReference", {"id": "1", "name": "[Full Access]"})
            ol.set("membercount", str(len(ol)))
            return
    raise ValueError("no fmextscriptaccess extended privilege")


def layout(root, name):
    return next(l for l in root.iter("Layout") if l.get("name") == name and l.get("isFolder") is None)


def contacts_layouts(root, page_field):
    for o in layout(root, "Contact Details").iter("LayoutObject"):
        if o.get("type") == "Web Viewer" and o.get("name") == "website":
            o.find(".//WebViewer//Calculation/Text").text = WEB_VIEWER_CALC
            break
    else:
        raise ValueError("no web viewer named website")
    cl = layout(root, "Contact List")
    parts = cl.find("PartsList")
    body = next(p for p in parts.findall("Part") if p.get("type") == "Body").find("Definition")
    top = int(body.get("absolute")) + int(body.get("size"))
    header = next(p for p in parts.findall("Part") if p.get("type") == "Header")
    text = copy.deepcopy(header.find("ObjectList").find("LayoutObject"))
    text.attrib.pop("hash", None)
    text.set("id", str(max(int(o.get("id")) for o in cl.iter("LayoutObject")) + 1))
    text.set("name", "pageMerge")
    for tag in ("SourceUUID",):
        for e in text.findall(tag):
            text.remove(e)
    text.find("UUID").text = uid("Contact List pageMerge")
    text.find("Bounds").attrib.update({"top": str(top + 10), "left": "400", "bottom": str(top + 29), "right": "620"})
    text.find("Text/StyledText/Data").text = PAGE_MERGE
    text.find("Text/Options").text = "10"  # as the template's merge text <<Initial>>
    footer = ET.SubElement(parts, "Part", {"type": "Footer", "kind": "7"})
    ET.SubElement(footer, "Definition", {"type": "Footer", "kind": "7", "size": "40", "absolute": str(top),
                                         "Options": "0"})
    # an edit box showing zPage, cloned from the body's first field
    box = copy.deepcopy(next(o for o in next(p for p in parts.findall("Part") if p.get("type") == "Body")
                             .find("ObjectList") if o.get("type") == "Edit Box"))
    box.attrib.pop("hash", None)
    box.set("id", str(int(text.get("id")) + 1))
    box.set("name", "pageCalc")
    for tag in ("SourceUUID", "TabOrder", "Tooltip"):
        for e in box.findall(tag):
            box.remove(e)
    box.find("UUID").text = uid("Contact List pageCalc")
    # inside the printable width (the PDF cuts the layout at the page edge)
    box.find("Bounds").attrib.update({"top": str(top + 2), "left": "96", "bottom": str(top + 38), "right": "362"})
    ref = box.find("Field/FieldReference")
    ref.attrib.update({"id": page_field, "name": "zPage", "UUID": ""})
    display = box.find("Field/Display")
    for e in display.findall("Placeholder"):
        display.remove(e)
    ol = ET.SubElement(footer, "ObjectList", {"membercount": "2"})
    ol.append(text)
    ol.append(box)
    parts.set("membercount", str(len(parts.findall("Part"))))


def document(kind, schema_path):
    schema = make_patch.Schema(schema_path)
    root = schema.root
    table = {"contacts": "Contacts", "tasks": "Tasks"}[kind]
    schema.fields[(table, "zOut")] = add_output_field(root, table)
    allow_apple_events(root)
    if kind == "contacts":
        schema.fields[(table, "zPage")] = add_page_field(root)
        contacts_layouts(root, schema.fields[(table, "zPage")])
    scripts = contacts_scripts() if kind == "contacts" else tasks_scripts()
    probes = [{"name": n, "full": False, "steps": list(enumerate(steps, 1))} for n, steps in scripts.items()]
    ctx, cat, steps = make_patch.build(schema, probes)
    make_patch.append(schema.script_catalog, cat)
    make_patch.append(schema.steps_for_scripts, steps)
    return '<?xml version="1.0" encoding="UTF-8"?>\n' + ET.tostring(root, encoding="unicode")


def plan():
    """The run order: Contacts alone for O1-O5, then Tasks as the second file."""
    def run(db, table, names):
        return [{"script": n, "database": db, "table": table, "field": "zOut"} for n in names]
    c = [n for n in contacts_scripts() if n != "O6 From Contacts"]
    return {"files": ["Contacts.fmp12", "Tasks.fmp12"],
            "steps": [{"open": "Contacts.fmp12"}] + run("Contacts.fmp12", "Contacts", c)
            + [{"open": "Tasks.fmp12"}] + run("Tasks.fmp12", "Tasks", list(tasks_scripts()))
            + run("Contacts.fmp12", "Contacts", ["O6 From Contacts"])}


def check(wanted_path, stored_path):
    """Compare the document given to FMUpgradeTool with FMDeveloperTool's
    export of the file it generated: every probe script's step ids, the
    output field, the extended privilege and the layout changes."""
    def scripts(root):
        out = {}
        for s in root.find(".//StepsForScripts").findall("Script"):
            out[s.find("ScriptReference").get("name")] = [st.get("id") for st in s.find("ObjectList").findall("Step")]
        return out
    wanted, stored = ET.parse(wanted_path).getroot(), ET.parse(stored_path).getroot()
    problems = []
    w, s = scripts(wanted), scripts(stored)
    for name, ids in w.items():
        if s.get(name) != ids:
            problems.append(f"script {name!r}: {len(ids)} steps wanted, stored {len(s.get(name) or [])}")
    if not any(f.get("name") == "zOut" and f.find("Storage").get("global") == "True" for f in stored.iter("Field")):
        problems.append("no global field zOut")
    ext = next(e for e in stored.iter("ExtendedPrivilege") if e.get("name") == "fmextscriptaccess")
    if "[Full Access]" not in [p.get("name") for p in ext.iter("PrivilegeSetReference")]:
        problems.append("fmextscriptaccess is not granted to [Full Access]")
    if any(l.get("name") == "Contact Details" for l in stored.iter("Layout")):
        texts = ["".join(t.itertext()).strip() for o in layout(stored, "Contact Details").iter("LayoutObject")
                 if o.get("name") == "website" for t in o.iter("Calculation")]
        if WEB_VIEWER_CALC not in texts:
            problems.append(f"web viewer calculation not stored: {texts}")
        merges = [o for o in layout(stored, "Contact List").iter("LayoutObject") if o.get("name") == "pageMerge"]
        if not merges or (merges[0].findtext("Text/StyledText/Data") or "").strip() != PAGE_MERGE:
            problems.append("no pageMerge text object on Contact List")
        if not [o for o in layout(stored, "Contact List").iter("LayoutObject") if o.get("name") == "pageCalc"
                and o.find("Field/FieldReference").get("name") == "zPage"]:
            problems.append("no pageCalc field object on Contact List")
    print(f"{Path(stored_path).name}: {len(w)} scripts, {sum(map(len, w.values()))} steps; "
          + ("; ".join(problems) if problems else "stored as generated"))
    if problems:
        sys.exit(1)


def main():
    if sys.argv[1:] == ["plan"]:
        print(json.dumps(plan(), indent=2))
    elif len(sys.argv) == 4 and sys.argv[1] == "check":
        check(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 3 and sys.argv[1] in ("contacts", "tasks"):
        sys.stdout.write(document(sys.argv[1], sys.argv[2]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
