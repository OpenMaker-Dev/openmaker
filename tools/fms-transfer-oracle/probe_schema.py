#!/usr/bin/env python3
"""Add the import/export probe fields to a FileMaker schema document.

    python3 -I probe_schema.py <contacts-en.xml> [accounts.xml] > probe-en.xml

The input is FMDeveloperTool --saveAsXML output of the Contacts starter
template; the output is the same document with the fields below added to
the Contacts table, in the shapes FileMaker's own Save as XML writes them
(copied from the Contacts and Assets templates). FMUpgradeTool
--generateDBFile then builds the probe database from it (run.sh -g).

FileMaker Server does not open a file whose full access account has no
password, and the template's Admin account has none: `accounts.xml`, a
schema document of a file with a password on its Admin account, lends its
AccountsCatalog (password hashes as FileMaker stores them). Keep that
document and the built file out of the repository.
"""
import sys
import uuid
import xml.etree.ElementTree as ET

ENGLISH = '<LanguageReference name="English" id="21"></LanguageReference>'
TO = '<TableOccurrenceReference id="1065090" name="Contacts" UUID=""></TableOccurrenceReference>'

# name, field type, data type, extra (repetitions, global, calc, summary, auto-enter, validation)
FIELDS = [
    ("N", "Normal", "Number", {}),
    ("D", "Normal", "Date", {}),
    ("T", "Normal", "Time", {}),
    ("TS", "Normal", "Timestamp", {}),
    ("R", "Normal", "Text", {"reps": 3}),
    ("NR", "Normal", "Number", {"reps": 3}),
    ("CN", "Calculated", "Number", {"calc": "N * 2"}),
    ("CU", "Calculated", "Text", {"calc": 'First Name & "!"', "unstored": True}),
    ("S", "Summary", "Number", {"summary": ("Total", "N")}),
    ("SC", "Summary", "Number", {"summary": ("Count", "First Name")}),
    ("G", "Normal", "Text", {"global": True}),
    ("AE", "Normal", "Text", {"auto": ('"auto"', False)}),
    ("AEO", "Normal", "Text", {"auto": ('"over"', True)}),
    ("VNE", "Normal", "Text", {"validate": "notEmpty"}),
    ("VU", "Normal", "Text", {"validate": "unique"}),
    ("VNO", "Normal", "Text", {"validate": "notEmpty", "entry_only": True}),
    ("LOG", "Normal", "Text", {}),
    ("K", "Normal", "Text", {}),
    # Summaries of every operation and option (battery x05): B is the break
    # field, W the weight. "summary" is (operation, field[, additional field,
    # restart, summarize repetitions]); "reps" gives a summary its repetitions.
    ("B", "Normal", "Text", {}),
    ("W", "Normal", "Number", {}),
    ("SRT", "Summary", "Number", {"summary": ("RunningTotal", "N", "B", True)}),
    ("SRT0", "Summary", "Number", {"summary": ("RunningTotal", "N")}),
    ("SRC", "Summary", "Number", {"summary": ("RunningCount", "N")}),
    ("SRCR", "Summary", "Number", {"summary": ("RunningCount", "N", "B", True)}),
    ("SF", "Summary", "Number", {"summary": ("Fractional", "N")}),
    ("SFS", "Summary", "Number", {"summary": ("FractionalSubtotal", "N", "B")}),
    ("SA", "Summary", "Number", {"summary": ("Average", "N")}),
    ("SWA", "Summary", "Number", {"summary": ("WeightedAverage", "N", "W")}),
    ("SMIN", "Summary", "Number", {"summary": ("Minimum", "N")}),
    ("SMAX", "Summary", "Number", {"summary": ("Maximum", "N")}),
    ("SSD", "Summary", "Number", {"summary": ("StdDeviation", "N")}),
    ("SSDP", "Summary", "Number", {"summary": ("StdDeviationByPopulation", "N")}),
    ("SL", "Summary", "Text", {"summary": ("List", "N")}),
    ("SNR", "Summary", "Number", {"summary": ("Total", "NR")}),
    ("SNRI", "Summary", "Number", {"summary": ("Total", "NR", None, False, "Individually"), "reps": 3}),
    ("SNRRI", "Summary", "Number", {"summary": ("RunningTotal", "NR", None, False, "Individually"), "reps": 3}),
]


def field_xml(fid, name, ftype, dtype, x, ids):
    u = str(uuid.uuid5(uuid.NAMESPACE_URL, "om-transfer-probe:" + name)).upper()
    s = f'<Field id="{fid}" name="{name}" fieldtype="{ftype}" datatype="{dtype}" comment="">'
    s += f'<UUID modifications="0" userName="" accountName="Admin" timestamp="2026-10-29T00:00:00">{u}</UUID>'
    if ftype == "Calculated":
        s += '<AutoEnter alwaysEvaluate="False"></AutoEnter>'
        stored = "False" if x.get("unstored") else "True"
        s += (f'<Storage storeCalculationResults="{stored}" autoIndex="True" index="None" global="False" '
              f'maxRepetitions="1">{ENGLISH}</Storage>')
        s += f'<Calculation>{TO}<Text><![CDATA[{x["calc"]}]]></Text></Calculation>'
    elif ftype == "Summary":
        summary = x["summary"]
        defaults = (None, False, "Together")
        op, of, extra, restart, reps = summary + defaults[len(summary) - 2:]
        s += '<AutoEnter type="" prohibitModification="False"></AutoEnter>'
        s += (f'<SummaryInfo restartEachGroup="{restart}" summarizeRepetition="{reps}" operation="{op}"><SummaryField>'
              f'<FieldReference id="{ids[of]}" name="{of}" UUID=""><BaseTableReference id="130" name="Contacts" UUID="">'
              f'</BaseTableReference></FieldReference></SummaryField>')
        if extra:
            # With its occurrence: given a base table, the importer stores a
            # key the engine ignores (file-metadata.md 5.4).
            s += (f'<AdditionalField><FieldReference id="{ids[extra]}" name="{extra}" UUID="">{TO}'
                  f'</FieldReference></AdditionalField>')
        s += '</SummaryInfo>'
        if x.get("reps", 1) > 1:
            s += (f'<Storage autoIndex="True" index="None" global="False" maxRepetitions="{x["reps"]}">'
                  f'{ENGLISH}</Storage>')
    else:
        if "auto" in x:
            calc, over = x["auto"]
            s += (f'<AutoEnter type="Calculated" prohibitModification="False" overwriteExisting="{over}" '
                  f'alwaysEvaluate="False"><Calculated enable="True"><Calculation>{TO}<Text><![CDATA[{calc}]]>'
                  f'</Text></Calculation></Calculated></AutoEnter>')
        else:
            s += '<AutoEnter type="" prohibitModification="False"></AutoEnter>'
        v = x.get("validate")
        if v and x.get("entry_only"):
            s += ('<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="False" '
                  'notEmpty="True" unique="False" existing="False"></Validation>')
        elif v:
            s += (f'<Validation alwaysValidate="True" type="Always" allowOverride="False" '
                  f'notEmpty="{v == "notEmpty"}" unique="{v == "unique"}" existing="False"></Validation>')
        else:
            s += ('<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="True" '
                  'notEmpty="False" unique="False" existing="False"></Validation>')
        s += (f'<Storage autoIndex="True" index="None" global="{x.get("global", False)}" '
              f'maxRepetitions="{x.get("reps", 1)}">{ENGLISH}</Storage>')
    s += '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>'
    return s


def main():
    root = ET.parse(sys.argv[1]).getroot()
    for cat in root.iter("FieldCatalog"):
        bt = cat.find("BaseTableReference")
        if bt is not None and bt.get("name") == "Contacts":
            break
    else:
        raise SystemExit("no Contacts field catalog")
    objects = cat.find("ObjectList")
    ids = {f.get("name"): int(f.get("id")) for f in objects if f.tag == "Field"}
    next_id = max(ids.values()) + 1
    for name, _, _, _ in FIELDS:
        ids[name] = next_id
        next_id += 1
    for name, ftype, dtype, x in FIELDS:
        objects.append(ET.fromstring(field_xml(ids[name], name, ftype, dtype, x, ids)))
        order = cat.find("CustomOrderList")
        if order is not None:
            ET.SubElement(order, "key").text = str(ids[name])
    objects.set("membercount", str(len([f for f in objects if f.tag == "Field"])))
    if len(sys.argv) > 2:
        lender = next(ET.parse(sys.argv[2]).getroot().iter("AccountsCatalog"))
        parent = next(p for p in root.iter() if p.find("AccountsCatalog") is not None)
        old = parent.find("AccountsCatalog")
        parent.insert(list(parent).index(old), lender)
        parent.remove(old)
    sys.stdout.write('<?xml version="1.0" encoding="UTF-8"?>\n')
    sys.stdout.write(ET.tostring(root, encoding="unicode"))


if __name__ == "__main__":
    main()
