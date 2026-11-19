#!/usr/bin/env python3
"""Write a probe FMSaveAsXML document that FileMaker turns into a file.

    python3 -I tools/file_metadata_probe.py [--no-triggers] [--find-fields] <tasks-en.xml> > probe.xml
    FMUpgradeTool --generateDBFile -src_path probe.xml        (in the container)

`tasks-en.xml` is FMDeveloperTool --saveAsXML of the shipped Tasks template
(English). The probe adds, through FileMaker's own XML importer, every file
structure value the template and example corpus never use, so that
docs/re/file-metadata.md can be checked against what FileMaker writes:

- number fields N, W and a 3-repetition NR in Tasks, and one summary field
  per operation (Total, RunningTotal with restart, Average, WeightedAverage,
  Count, RunningCount, Minimum, Maximum, StdDeviation,
  StdDeviationByPopulation, Fractional, FractionalSubtotal, List, a Total
  summarizing repetitions individually);
- seven Tasks occurrences P_A..P_G and relationships with every operator
  (= is in the template), a three-predicate join, a cartesian join, cascade
  create/delete on both sides, and both sides sorted (descending, by value
  list, blanks last, not maintained);
- value lists from a field with a second field, related values only, a
  language re-sort, and custom values with a divider;
- layout triggers for every layout event, find/preview modes and a
  parameter, and file triggers for every file event.

`--no-triggers` leaves the triggers out (for running scripts in the file
with FileMaker Server's script engine, where file triggers would fire).
`--find-fields` adds what finds the host cannot answer from an index need
(script-runtime.md 13.19): unstored calculations U (`N * 2`, a number) and
UT (`Task & " " & Status`, text) and a global text field G.

Nothing here encodes a stored format: FileMaker's importer writes the file.
"""
import sys
import uuid
import xml.etree.ElementTree as ET

NS = uuid.UUID("6f6d2d66-696c-652d-6d65-746164617461")


def uid(label):
    return str(uuid.uuid5(NS, label)).upper()


def sub(parent, tag, attrib=None, text=None):
    e = ET.SubElement(parent, tag, {k: str(v) for k, v in (attrib or {}).items()})
    if text is not None:
        e.text = text
    return e


def field_ref(parent, tag, fid, name, to=None, table=None):
    holder = sub(parent, tag) if tag else parent
    fr = sub(holder, "FieldReference", {"id": fid, "name": name, "UUID": ""})
    if to:
        sub(fr, "TableOccurrenceReference", {"id": to[0], "name": to[1], "UUID": ""})
    if table:
        sub(fr, "BaseTableReference", {"id": table[0], "name": table[1], "UUID": ""})
    return holder


TASKS = ("130", "Tasks")
OCC = {name: str(1065100 + i) for i, name in enumerate(["P_A", "P_B", "P_C", "P_D", "P_E", "P_F", "P_G"])}
NEW_OCC = list(OCC)
OCC["Tasks"] = "1065090"
FIELDS = {"Task": "1", "Due Date": "2", "Status": "4", "Category": "5", "N": "100", "W": "101", "NR": "102"}

SUMMARIES = [  # id, name, operation, summarized, additional, restart, repetitions
    ("110", "S_Total", "Total", "N", None, False, "Together"),
    ("111", "S_RunTotal", "RunningTotal", "N", "Status", True, "Together"),
    ("112", "S_Avg", "Average", "N", None, False, "Together"),
    ("113", "S_WAvg", "WeightedAverage", "N", "W", False, "Together"),
    ("114", "S_Count", "Count", "N", None, False, "Together"),
    ("115", "S_RunCount", "RunningCount", "N", None, False, "Together"),
    ("116", "S_Min", "Minimum", "N", None, False, "Together"),
    ("117", "S_Max", "Maximum", "N", None, False, "Together"),
    ("118", "S_SD", "StdDeviation", "N", None, False, "Together"),
    ("119", "S_SDP", "StdDeviationByPopulation", "N", None, False, "Together"),
    ("120", "S_Frac", "Fractional", "N", None, False, "Together"),
    ("121", "S_FracSub", "FractionalSubtotal", "N", "Category", False, "Together"),
    ("122", "S_List", "List", "Task", None, False, "Together"),
    ("123", "S_TotalReps", "Total", "NR", None, False, "Individually"),
]


def add_fields(st):
    for fc in st.iterfind("FieldsForTables/FieldCatalog"):
        if fc.find("BaseTableReference").get("id") == TASKS[0]:
            break
    objs = fc.find("ObjectList")
    for fid, name, reps in (("100", "N", 1), ("101", "W", 1), ("102", "NR", 3)):
        f = sub(objs, "Field", {"id": fid, "name": name, "fieldtype": "Normal", "datatype": "Number", "comment": ""})
        sub(f, "UUID", {}, uid("field " + name))
        sub(f, "AutoEnter", {"type": "", "prohibitModification": "False"})
        sub(f, "Validation", {"alwaysValidate": "False", "type": "OnlyDuringDataEntry", "allowOverride": "True",
                              "notEmpty": "False", "unique": "False", "existing": "False"})
        st_ = sub(f, "Storage", {"autoIndex": "True", "index": "None", "global": "False", "maxRepetitions": reps})
        sub(st_, "LanguageReference", {"name": "English", "id": "21"})
        sub(f, "TagList")
    for fid, name, op, src, extra, restart, reps in SUMMARIES:
        dtype = "Text" if op == "List" else "Number"
        f = sub(objs, "Field", {"id": fid, "name": name, "fieldtype": "Summary", "datatype": dtype, "comment": ""})
        sub(f, "UUID", {}, uid("field " + name))
        sub(f, "AutoEnter", {"type": "", "prohibitModification": "False"})
        info = sub(f, "SummaryInfo", {"restartEachGroup": restart, "summarizeRepetition": reps, "operation": op})
        field_ref(info, "SummaryField", FIELDS[src], src, table=TASKS)
        if extra:
            # FileMaker Pro stores the additional field with its occurrence.
            field_ref(info, "AdditionalField", FIELDS[extra], extra, to=("1065090", "Tasks"))
        sub(f, "TagList")
    fc.set("membercount", str(len(objs)))
    objs.set("membercount", str(len(objs)))


# id, name, field type, data type, unstored calculation (None: a global)
FIND_FIELDS = [
    ("103", "U", "Calculated", "Number", "N * 2"),
    ("104", "UT", "Calculated", "Text", 'Task & " " & Status'),
    ("105", "G", "Normal", "Text", None),
]


def add_find_fields(st):
    for fc in st.iterfind("FieldsForTables/FieldCatalog"):
        if fc.find("BaseTableReference").get("id") == TASKS[0]:
            break
    objs = fc.find("ObjectList")
    for fid, name, kind, dtype, formula in FIND_FIELDS:
        f = sub(objs, "Field", {"id": fid, "name": name, "fieldtype": kind, "datatype": dtype, "comment": ""})
        sub(f, "UUID", {}, uid("field " + name))
        if formula is None:
            sub(f, "AutoEnter", {"type": "", "prohibitModification": "False"})
            sub(f, "Validation", {"alwaysValidate": "False", "type": "OnlyDuringDataEntry", "allowOverride": "True",
                                  "notEmpty": "False", "unique": "False", "existing": "False"})
            storage = {"autoIndex": "True", "index": "None", "global": "True", "maxRepetitions": 1}
        else:
            sub(f, "AutoEnter", {"alwaysEvaluate": "False"})
            storage = {"storeCalculationResults": "False", "autoIndex": "True", "index": "None",
                       "global": "False", "maxRepetitions": 1}
        st_ = sub(f, "Storage", storage)
        sub(st_, "LanguageReference", {"name": "English", "id": "21"})
        if formula is not None:
            c = sub(f, "Calculation")
            sub(c, "TableOccurrenceReference", {"id": OCC["Tasks"], "name": "Tasks", "UUID": ""})
            sub(c, "Text", {}, formula)
        sub(f, "TagList")
    fc.set("membercount", str(len(objs)))
    objs.set("membercount", str(len(objs)))


def add_occurrences(st):
    cat = st.find("TableOccurrenceCatalog")
    for i, name in enumerate(NEW_OCC):
        oid = OCC[name]
        to = sub(cat, "TableOccurrence", {"View": "Full", "height": "120", "id": oid, "name": name, "type": "Local"})
        sub(to, "UUID", {}, uid("occurrence " + name))
        src = sub(to, "BaseTableSourceReference", {"type": "BaseTableReference"})
        sub(src, "BaseTableReference", {"id": TASKS[0], "name": TASKS[1], "UUID": ""})
        sub(to, "CoordRect", {"top": 300, "left": 20 + 160 * i, "bottom": 420, "right": 151 + 160 * i})
        sub(to, "Color", {"red": 120, "green": 120, "blue": 120, "alpha": "1.00"})
        sub(to, "TagList")
    cat.set("membercount", str(len(cat.findall("TableOccurrence"))))


def sort_spec(parent, sorts, blanks_last=False, maintain=True):
    spec = sub(parent, "SortSpecification", {"value": "True", "blanksLast": blanks_last, "maintain": maintain})
    lst = sub(spec, "SortList", {"membercount": len(sorts)})
    for to, field, kind, vl in sorts:
        s = sub(lst, "Sort", {"type": kind})
        field_ref(s, "PrimaryField", FIELDS[field], field, to=(OCC[to], to))
        if vl:
            sub(s, "ValueListReference", {"id": vl[0], "name": vl[1], "UUID": ""})


# Relationships start from the template's Tasks occurrence so that layouts
# and scripts on Tasks reach them.
RELATIONSHIPS = [  # id, left, right, predicates, left side, right side
    ("100", "Tasks", "P_B", [("LessOrEqual", "N", "N")],
     dict(create=False, delete=True, sort=[("Tasks", "Status", "Custom", ("2", "Status"))]),
     dict(create=True, delete=True, sort=[("P_B", "N", "Descending", None), ("P_B", "Task", "Ascending", None)])),
    ("101", "Tasks", "P_C", [("Greater", "N", "N")], dict(), dict()),
    ("102", "Tasks", "P_D", [("NotEqual", "N", "N"), ("Equal", "Status", "Status"), ("Less", "Due Date", "Due Date")],
     dict(), dict(create=True)),
    ("103", "Tasks", "P_E", [("CartesianProduct", "Task", "Task")], dict(create=True), dict()),
    ("104", "Tasks", "P_F", [("GreaterOrEqual", "N", "N")], dict(),
     dict(sort=[("P_F", "Task", "Ascending", None)], blanks_last=True, maintain=False)),
    ("105", "Tasks", "P_G", [("Less", "N", "N"), ("LessOrEqual", "W", "W")], dict(delete=True), dict()),
]


def add_relationships(st):
    cat = st.find("RelationshipCatalog")
    for rid, left, right, preds, lside, rside in RELATIONSHIPS:
        r = sub(cat, "Relationship", {"id": rid})
        sub(r, "UUID", {}, uid("relationship " + rid))
        for tag, to, side in (("LeftTable", left, lside), ("RightTable", right, rside)):
            t = sub(r, tag, {"cascadeCreate": side.get("create", False), "cascadeDelete": side.get("delete", False),
                             "type": "Local"})
            sub(t, "TableOccurrenceReference", {"id": OCC[to], "name": to, "UUID": ""})
            if side.get("sort"):
                sort_spec(t, side["sort"], side.get("blanks_last", False), side.get("maintain", True))
        pl = sub(r, "JoinPredicateList", {"membercount": len(preds)})
        for op, lf, rf in preds:
            p = sub(pl, "JoinPredicate", {"type": op})
            field_ref(p, "LeftField", FIELDS[lf], lf, to=(OCC[left], left))
            field_ref(p, "RightField", FIELDS[rf], rf, to=(OCC[right], right))
        sub(r, "TagList")
    cat.set("membercount", str(len(cat.findall("Relationship"))))


def add_value_lists(st):
    cat = st.find("ValueListCatalog")
    v = sub(cat, "ValueList", {"id": "100", "name": "P_TaskStatus"})
    sub(v, "Source", {"value": "FromField"})
    fld = sub(v, "Field")
    pf = sub(fld, "PrimaryField", {"show": "False", "sort": "False"})
    field_ref(pf, None, FIELDS["Task"], "Task", to=(OCC["P_B"], "P_B"))
    sf = sub(fld, "SecondaryField", {"show": "True", "sort": "True"})
    field_ref(sf, None, FIELDS["Status"], "Status", to=(OCC["P_B"], "P_B"))
    sr = sub(fld, "ShowRelated", {"value": "True"})
    sub(sr, "TableOccurrenceReference", {"id": OCC["Tasks"], "name": "Tasks", "UUID": ""})
    sub(v, "UUID", {}, uid("valuelist 100"))
    sub(v, "TagList")
    v = sub(cat, "ValueList", {"id": "101", "name": "P_NumbersGerman"})
    sub(v, "Source", {"value": "FromField"})
    fld = sub(v, "Field")
    pf = sub(fld, "PrimaryField", {"show": "True", "sort": "True"})
    field_ref(pf, None, FIELDS["N"], "N", to=("1065090", "Tasks"))
    sub(fld, "ShowRelated", {"value": "False"})
    sub(v, "LanguageReference", {"name": "German", "id": "25"})
    sub(v, "UUID", {}, uid("valuelist 101"))
    sub(v, "TagList")
    v = sub(cat, "ValueList", {"id": "103", "name": "P_TaskAndStatus"})
    sub(v, "Source", {"value": "FromField"})
    fld = sub(v, "Field")
    pf = sub(fld, "PrimaryField", {"show": "True", "sort": "False"})
    field_ref(pf, None, FIELDS["Task"], "Task", to=("1065090", "Tasks"))
    sf = sub(fld, "SecondaryField", {"show": "False", "sort": "True"})
    field_ref(sf, None, FIELDS["Status"], "Status", to=("1065090", "Tasks"))
    sub(fld, "ShowRelated", {"value": "False"})
    sub(v, "UUID", {}, uid("valuelist 103"))
    sub(v, "TagList")
    v = sub(cat, "ValueList", {"id": "102", "name": "P_Custom"})
    sub(v, "Source", {"value": "Custom"})
    cv = sub(v, "CustomValues")
    sub(cv, "Text", {}, "Alpha\r-\rBeta\rGamma")
    sub(v, "UUID", {}, uid("valuelist 102"))
    sub(v, "TagList")
    cat.set("membercount", str(len(cat.findall("ValueList"))))


LAYOUT_TRIGGERS = [  # event id, action, script id, modes, parameter
    (101, "OnLayoutEnter", "8", "B", None),
    (102, "OnLayoutKeystroke", "7", "BF", None),
    (103, "OnRecordLoad", "7", "B", "Get ( RecordID )"),
    (104, "OnRecordCommit", "7", "B", '"commit"'),
    (105, "OnRecordRevert", "7", "B", None),
    (106, "OnModeEnter", "7", "BFP", None),
    (107, "OnModeExit", "7", "BFP", None),
    (109, "OnLayoutExit", "7", "B", None),
    (110, "OnViewChange", "7", "B", None),
    (111, "OnLayoutSizeChange", "7", "BP", None),
    (112, "OnGestureTap", "7", "B", None),
    (113, "OnExternalCommandReceived", "7", "B", None),
]
FILE_TRIGGERS = [(201, "OnFirstWindowOpen", "1"), (202, "OnLastWindowClose", "2"), (204, "OnWindowOpen", "3"),
                 (205, "OnWindowClose", "4"), (206, "OnFileAVPlayerChange", "5"), (209, "OnWindowTransaction", "6")]


def trigger(parent, event, action, script, modes, param, names):
    attrs = {"id": event, "action": action}
    for flag, attr in (("B", "browseMode"), ("F", "findMode"), ("P", "previewMode")):
        if flag in modes:
            attrs[attr] = "True"
    t = sub(parent, "ScriptTrigger", attrs)
    ref = sub(t, "ScriptReference", {"id": script, "name": names[script], "UUID": ""})
    if param is not None:
        c = sub(ref, "Calculation")
        sub(c, "Text", {}, param)
    return t


def add_triggers(root, st):
    names = {s.get("id"): s.get("name") for s in st.iterfind("ScriptCatalog//Script")}
    for lay in st.iterfind("LayoutCatalog//Layout"):
        if lay.get("id") != "3":
            continue
        old = lay.find("ScriptTriggers")
        if old is not None:
            lay.remove(old)
        tr = ET.Element("ScriptTriggers", {"membercount": str(len(LAYOUT_TRIGGERS))})
        for event, action, script, modes, param in LAYOUT_TRIGGERS:
            trigger(tr, event, action, script, modes, param, names)
        lay.insert(list(lay).index(lay.find("Options")) + 1, tr)
        # An object trigger with a parameter on the first field object.
        for obj in lay.iter("LayoutObject"):
            if obj.get("type") == "Edit Box" and obj.find("Field") is not None:
                ot = sub(obj, "ScriptTriggers", {"membercount": "2"})
                trigger(ot, 3, "OnObjectModify", "7", "B", '"modify"', names)
                trigger(ot, 8, "OnObjectAVPlayerChange", "7", "B", None, names)
                break
    md = root.find("Metadata/AddAction")
    old = md.find("ScriptTriggers")
    if old is not None:
        md.remove(old)
    ft = sub(md, "ScriptTriggers", {"membercount": len(FILE_TRIGGERS)})
    for event, action, script in FILE_TRIGGERS:
        attrs = {"id": event, "action": action, "browseMode": "True"}
        if event == 209:
            attrs["scriptParameterFieldName"] = "P_TransactionLog"
        t = sub(ft, "ScriptTrigger", attrs)
        sub(t, "ScriptReference", {"id": script, "name": names[script], "UUID": ""})


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    tree = ET.parse(args[0])
    root = tree.getroot()
    st = root.find("Structure/AddAction")
    add_fields(st)
    if "--find-fields" in sys.argv:
        add_find_fields(st)
    add_occurrences(st)
    add_relationships(st)
    add_value_lists(st)
    if "--no-triggers" not in sys.argv:
        add_triggers(root, st)
    sys.stdout.write('<?xml version="1.0" encoding="UTF-8"?>\n')
    sys.stdout.write(ET.tostring(root, encoding="unicode"))


if __name__ == "__main__":
    main()
