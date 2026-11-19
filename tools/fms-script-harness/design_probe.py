#!/usr/bin/env python3
"""Design-function probe document: file_metadata_probe.py's output plus
what Design functions tell apart (fields of every storage, comments,
serial numbers, occurrences whose names sort differently from their ids).

    python3 -I tools/file_metadata_probe.py --no-triggers --find-fields tasks-en.xml > meta.xml
    python3 -I design_probe.py meta.xml > designprobe.xml
"""
import sys
import uuid
import xml.etree.ElementTree as ET

NS = uuid.UUID("6f6d2d64-6573-6967-6e2d-70726f626521")


def uid(label):
    return str(uuid.uuid5(NS, label)).upper()


def sub(parent, tag, attrib=None, text=None):
    e = ET.SubElement(parent, tag, {k: str(v) for k, v in (attrib or {}).items()})
    if text is not None:
        e.text = text
    return e


# id, name, field type, data type, storage attributes, extra
FIELDS = [
    ("130", "SER", "Normal", "Text", {"index": "All", "global": "False", "maxRepetitions": 1}, "serial"),
    ("131", "SC", "Calculated", "Number",
     {"storeCalculationResults": "True", "autoIndex": "True", "index": "All", "global": "False", "maxRepetitions": 1},
     "N + 1"),
    ("132", "GR", "Normal", "Number", {"autoIndex": "True", "index": "None", "global": "True", "maxRepetitions": 2}, None),
    ("133", "C", "Normal", "Binary", {"global": "False", "maxRepetitions": 1}, None),
    ("134", "TS", "Normal", "TimeStamp", {"autoIndex": "True", "index": "Minimal", "global": "False", "maxRepetitions": 1}, None),
    ("135", "TM", "Normal", "Time", {"autoIndex": "False", "index": "None", "global": "False", "maxRepetitions": 1}, None),
    ("136", "GC", "Calculated", "Text",
     {"storeCalculationResults": "False", "autoIndex": "True", "index": "None", "global": "True", "maxRepetitions": 1},
     '"g"'),
    ("137", "RC", "Calculated", "Number",
     {"storeCalculationResults": "True", "autoIndex": "True", "index": "None", "global": "False", "maxRepetitions": 4},
     "Extend ( NR ) * 2"),
    ("138", "SER2", "Normal", "Number", {"autoIndex": "True", "index": "None", "global": "False", "maxRepetitions": 1}, "serial2"),
]
OCCURRENCES = ["a lower", "Zeta", "_under", "10 ten", "9 nine", "Éclair", "eclair two", "日本", "Tasks2"]


def main():
    tree = ET.parse(sys.argv[1])
    root = tree.getroot()
    st = root.find("Structure/AddAction")
    for bt in st.iterfind("BaseTableCatalog/BaseTable"):
        if bt.get("id") == "130":
            bt.set("comment", "Things to do")
    for fc in st.iterfind("FieldsForTables/FieldCatalog"):
        if fc.find("BaseTableReference").get("id") == "130":
            break
    objs = fc.find("ObjectList")
    for f in objs.iterfind("Field"):
        if f.get("name") == "N":
            f.set("comment", "a number")
    for fid, name, kind, dtype, storage, extra in FIELDS:
        f = sub(objs, "Field", {"id": fid, "name": name, "fieldtype": kind, "datatype": dtype,
                                "comment": "comment of " + name if name == "SC" else ""})
        sub(f, "UUID", {}, uid("field " + name))
        if kind == "Calculated":
            sub(f, "AutoEnter", {"alwaysEvaluate": "False"})
        elif extra == "serial":
            ae = sub(f, "AutoEnter", {"type": "SerialNumber", "prohibitModification": "False"})
            sub(ae, "SerialNumber", {"increment": "3", "nextvalue": "A0007", "generate": "OnCreation"})
        elif extra == "serial2":
            ae = sub(f, "AutoEnter", {"type": "SerialNumber", "prohibitModification": "False"})
            sub(ae, "SerialNumber", {"increment": "1", "nextvalue": "41", "generate": "OnCommit"})
        else:
            sub(f, "AutoEnter", {"type": "", "prohibitModification": "False"})
        if kind != "Calculated":
            sub(f, "Validation", {"alwaysValidate": "False", "type": "OnlyDuringDataEntry", "allowOverride": "True",
                                  "notEmpty": "False", "unique": "False", "existing": "False"})
        s = sub(f, "Storage", storage)
        sub(s, "LanguageReference", {"name": "English", "id": "21"})
        if kind == "Calculated":
            c = sub(f, "Calculation")
            sub(c, "TableOccurrenceReference", {"id": "1065090", "name": "Tasks", "UUID": ""})
            sub(c, "Text", {}, extra)
        sub(f, "TagList")
    fc.set("membercount", str(len(objs)))
    objs.set("membercount", str(len(objs)))
    cat = st.find("TableOccurrenceCatalog")
    for i, name in enumerate(OCCURRENCES):
        to = sub(cat, "TableOccurrence", {"View": "Full", "height": "120", "id": str(1065200 + i), "name": name,
                                          "type": "Local"})
        sub(to, "UUID", {}, uid("occurrence " + name))
        src = sub(to, "BaseTableSourceReference", {"type": "BaseTableReference"})
        sub(src, "BaseTableReference", {"id": "130", "name": "Tasks", "UUID": ""})
        sub(to, "CoordRect", {"top": 500, "left": 20 + 160 * i, "bottom": 620, "right": 151 + 160 * i})
        sub(to, "Color", {"red": 120, "green": 120, "blue": 120, "alpha": "1.00"})
        sub(to, "TagList")
    cat.set("membercount", str(len(cat.findall("TableOccurrence"))))
    sys.stdout.write('<?xml version="1.0" encoding="UTF-8"?>\n')
    sys.stdout.write(ET.tostring(root, encoding="unicode"))


if __name__ == "__main__":
    main()
