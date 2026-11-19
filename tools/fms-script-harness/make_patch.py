#!/usr/bin/env python3
"""Write an FMUpgradeTool patch that adds probe scripts to a scratch file.

    python3 -I make_patch.py <probes.txt> <schema.xml> > patch.xml
    python3 -I make_patch.py --generate <probes.txt> <schema.xml> > file.xml

`schema.xml` is FMDeveloperTool --saveAsXML output of the base file; it
resolves table occurrence, field, layout and existing script names to the
ids the patch must use. The patch adds one script per `== Name` block, with
the step XML shapes FileMaker's own Save as XML writes (FMSaveAsXML 2.3,
taken from the shipped templates and the MBS example files), and is applied
with `FMUpgradeTool --update` (run.sh -p). Nothing here encodes the stored
step format: FileMaker's XML importer writes the steps.

`--generate` writes the whole schema document instead, with the probe
scripts added and the "@trigger" lines' triggers on their layouts, for
`FMUpgradeTool --generateDBFile` (run.sh -g): FMUpgradeTool's patches do not
replace layouts (a ReplaceAction of a layout is accepted and ignored), so a
battery with triggers needs a generated file.

Probe file syntax, one step per line in FileMaker's display style:

    == Script name                 starts a script ("== Name [full access]"
                                   sets "Run script with full access")
    Set Variable [ $x ; 1 + 1 ]    arguments split at top-level ";"
    # text                         comment step
    // Beep                        disabled step
    Step 32                        a step with no parameters, by id
    @trigger <TAB> layout <TAB> event <TAB> script [<TAB> modes [<TAB> parameter]]
                                   a layout trigger (anywhere before the run
                                   section): event as FileMaker's XML names
                                   it (OnLayoutExit), modes B, F, P (default
                                   B), parameter a calculation

Schema directives (tab-separated, anywhere before the run section; each
makes the battery a generated file):

    @generate                      no change (a generated file only)
    @layout <TAB> name <TAB> TO <TAB> layout
                                   a new layout on occurrence TO: a copy of
                                   layout without objects, table view
                                   columns or triggers, appended last
    @field <TAB> TO <TAB> name <TAB> field
                                   a new field of TO's table: a copy of field
                                   (its type, options and rules)
    @filetrigger <TAB> event <TAB> script [<TAB> field name]
                                   a file trigger (OnWindowTransaction's
                                   "field name" option as the third field);
                                   the file gets exactly these
    @validate <TAB> TO::Field <TAB> Always|DataEntry <TAB> calculation
                                   a strict validation rule by calculation
                                   replaces the field's rules
    @relationship <TAB> TO <TAB> other TO <TAB> attr=value ...
                                   options of TO's side of the relationship
                                   (cascadeCreate, cascadeDelete: True/False)
    @portal <TAB> layout <TAB> TO <TAB> word
                                   the options word of layout's portal on TO
    @name <TAB> layout <TAB> portal:TO | TO::Field <TAB> name
                                   names the first such object of layout

Lines before the first "==" and blank lines are ignored. Calculations are
copied verbatim (FileMaker syntax, `;` separators). Supported steps are the
keys of STEPS below. A layout with "@trigger" lines gets exactly those
triggers (its own are dropped).
"""
import re
import sys
import uuid
import xml.etree.ElementTree as ET


def split_args(s):
    """Split at ';' outside quotes, brackets and comments."""
    out, cur, depth, i, q = [], "", 0, 0, False
    while i < len(s):
        c = s[i]
        if q:
            cur += c
            if c == "\\" and i + 1 < len(s):
                cur += s[i + 1]
                i += 1
            elif c == '"':
                q = False
        elif c == '"':
            q = True
            cur += c
        elif s.startswith("/*", i):
            j = s.find("*/", i + 2)
            j = len(s) if j < 0 else j + 2
            cur += s[i:j]
            i = j
            continue
        elif c in "([{":
            depth += 1
            cur += c
        elif c in ")]}":
            depth -= 1
            cur += c
        elif c == ";" and depth == 0:
            out.append(cur.strip())
            cur = ""
        else:
            cur += c
        i += 1
    if cur.strip() or out:
        out.append(cur.strip())
    return out


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def calc(text, datatype, position):
    return (f'<Calculation datatype="{datatype}" position="{position}"><Calculation>'
            f'<Text><![CDATA[{text}]]></Text></Calculation></Calculation>')


def boolean(label, bit, value):
    t = f' type="{label}"' if label else ""
    return f'<Parameter type="Boolean"><Boolean{t} id="{bit}" value="{value}"></Boolean></Parameter>'


def onoff(a):
    v = a.strip().lower()
    if v not in ("on", "off", "true", "false"):
        raise ValueError(f"expected On or Off, got {a!r}")
    return v in ("on", "true")


def keyword(args, key):
    """Pop 'Key: value' from args (case-insensitive); None when absent."""
    for i, a in enumerate(args):
        m = re.match(r"\s*" + re.escape(key) + r"\s*:\s*(.*)$", a, re.I | re.S)
        if m:
            del args[i]
            return m.group(1).strip()
    return None


class Schema:
    def __init__(self, path):
        root = self.root = ET.parse(path).getroot()
        self.tos, self.fields, self.layouts, self.scripts = {}, {}, {}, {}
        base_of = {}
        for to in root.iter("TableOccurrence"):
            b = to.find(".//BaseTableReference")
            if to.get("name") and b is not None:
                self.tos[to.get("name")] = to.get("id")
                base_of[to.get("name")] = b.get("name")
        for cat in root.iter("FieldCatalog"):
            bt = cat.find("BaseTableReference")
            if bt is None:
                continue
            fl = cat.find("ObjectList")
            for f in (fl if fl is not None else []):
                if f.tag == "Field":
                    self.fields[(bt.get("name"), f.get("name"))] = f.get("id")
        self.base_of = base_of
        self.layout_elements = {}
        for lc in root.iter("LayoutCatalog"):
            for l in lc.iter("Layout"):
                if l.get("name") and l.get("isFolder") is None:
                    self.layouts.setdefault(l.get("name"), l.get("id"))
                    self.layout_elements.setdefault(l.get("name"), l)
        self.script_catalog = next(root.iter("ScriptCatalog"), None)
        self.steps_for_scripts = next(root.iter("StepsForScripts"), None)
        for sc in root.iter("ScriptCatalog"):
            for s in sc.iter("Script"):
                if s.get("name") and s.get("isFolder") is None:
                    u = s.find("UUID")
                    self.scripts.setdefault(s.get("name"), (s.get("id"), u.text if u is not None else ""))

    def field(self, ref, rep_position=10, default_rep=None):
        m = re.match(r"\s*(.+?)::(.+?)\s*(?:\[(.+)\])?\s*$", ref)
        if not m:
            raise ValueError(f"field reference {ref!r} is not Table::Field")
        to, name, rep = m.groups()
        rep = rep or default_rep
        fid = self.fields.get((self.base_of.get(to), name))
        if to not in self.tos or fid is None:
            raise ValueError(f"unknown field {ref!r}")
        r = f"<repetition>{calc(rep, 1, rep_position)}</repetition>" if rep else ""
        return (f'<FieldReference id="{fid}" name="{esc(name)}" UUID="">{r}'
                f'<TableOccurrenceReference id="{self.tos[to]}" name="{esc(to)}" UUID=""></TableOccurrenceReference></FieldReference>')

    def layout(self, a):
        name = unquote(a)
        lid = self.layouts.get(name)
        if lid is None:
            raise ValueError(f"unknown layout {name!r}")
        return f'<LayoutReference id="{lid}" name="{esc(name)}" UUID=""></LayoutReference>'

    def occurrence(self, a):
        name = a.strip().strip('"')
        if name not in self.tos:
            raise ValueError(f"unknown table occurrence {name!r}")
        return f'<TableOccurrenceReference id="{self.tos[name]}" name="{esc(name)}" UUID=""></TableOccurrenceReference>'


def unquote(a):
    a = a.strip()
    if len(a) >= 2 and a[0] == '"' and a[-1] == '"':
        return a[1:-1]
    raise ValueError(f"expected a quoted name, got {a!r}")


def script_ref(ctx, a):
    """'"Name"' (from list) or 'By name: calc'. Returns (options, list xml)."""
    byname = re.match(r"\s*by name\s*:\s*(.*)$", a, re.I | re.S)
    if byname:
        return 0x2004800, f'<Parameter type="List"><List name="By name" value="2">{calc(byname.group(1), 1, 2)}</List></Parameter>'
    name = unquote(a)
    sid, su = ctx["scripts"].get(name) or ctx["schema"].scripts.get(name) or (None, None)
    if sid is None:
        raise ValueError(f"unknown script {name!r}")
    return 0x40, (f'<Parameter type="List"><List name="From list" value="1"><ScriptReference id="{sid}" '
                  f'name="{esc(name)}" UUID="{su}"></ScriptReference></List></Parameter>')


def parameter(args):
    p = keyword(args, "Parameter")
    if p is None and args:
        p = args.pop(0)
    if p:
        return 0x4000, f'<Parameter type="Parameter"><Parameter>{calc(p, 1, 0)}</Parameter></Parameter>'
    return 0, '<Parameter type="Parameter"><Parameter></Parameter></Parameter>'


COLLAPSED = boolean("Collapsed", 33554432, "False")


def st_set_variable(ctx, args):
    m = re.match(r"\s*([^\[\]]+?)\s*(?:\[(.+)\])?\s*$", args[0])  # any name: FileMaker validates it
    if not m:
        raise ValueError(f"bad variable {args[0]!r}")
    value = keyword(args[1:], "Value") if len(args) > 1 else None
    value = value if value is not None else (args[1] if len(args) > 1 else "")
    rep = f"<repetition>{calc(m.group(2), 1, 2)}</repetition>" if m.group(2) else "<repetition></repetition>"
    return 0x4004, (f'<Parameter type="Variable"><value>{calc(value, 1, 1)}</value>'
                    f'<Name value="{esc(m.group(1))}"></Name>{rep}</Parameter>')


def st_condition(collapsed):
    def f(ctx, args):
        return 0x4000, (COLLAPSED if collapsed else "") + f'<Parameter type="Calculation">{calc(args[0], 7, 0)}</Parameter>'
    return f


def st_else(ctx, args):
    return 0, COLLAPSED + COLLAPSED


def st_loop(ctx, args):
    flush = keyword(args, "Flush") or "Always"
    values = {"always": 1, "minimum": 2, "defer": 3}
    v = values[flush.lower()]
    return (0 if v == 1 else 0x100), COLLAPSED + f'<Parameter type="List"><List name="{flush.title()}" value="{v}"></List></Parameter>'


def st_exit_script(ctx, args):
    if args and args[0]:
        return 0x4000, f'<Parameter type="Calculation">{calc(args[0], 1, 0)}</Parameter>'
    return 0, ""


def st_onoff(ctx, args):
    on = onoff(args[0])
    return (0x20000 if on else 0), boolean("", 131072, "True" if on else "False")


def st_perform_script(ctx, args):
    o1, ref = script_ref(ctx, args.pop(0))
    o2, par = parameter(args)
    return o1 | o2, ref + par


def st_psos(ctx, args):
    wait = keyword(args, "Wait")
    wait = True if wait is None else onoff(wait)
    o1, ref = script_ref(ctx, args.pop(0))
    o2, par = parameter(args)
    return o1 | o2 | (0 if wait else 0x100), ref + par + boolean("Wait for completion", 256, "True" if wait else "False")


def st_open_transaction(ctx, args):
    opts = {a.strip().lower() for a in args}
    o, x = 0, COLLAPSED
    for label, bit, key in (("Skip auto-enter options", 4096, "skip auto-enter"), ("Skip data entry validation", 256, "skip validation"),
                            ("Override ESS locking conflicts", 512, "override ess")):
        on = key in opts
        o |= bit if on else 0
        x += boolean(label, bit, "True" if on else "False")
    return o, x


def st_revert_transaction(ctx, args):
    cond, code, msg = keyword(args, "Condition"), keyword(args, "Error"), keyword(args, "Message")
    o, x = 0, boolean("Condition", 256, "True" if cond is not None else "False")
    if cond is not None:
        o |= 0x4100
        x += f'<Parameter type="Condition">{calc(cond, 7, 0)}</Parameter>'
    x += boolean("Error Code", 512, "True" if code is not None else "False")
    if code is not None:
        o |= 0x4200
        x += f'<Parameter type="ErrorCode">{calc(code, 2, 1)}</Parameter>'
        x += f'<Parameter type="ErrorMessage">{calc(msg or "", 1, 2)}</Parameter>'
    return o, x


def st_set_field(ctx, args):
    return 0x4001, f'<Parameter type="FieldReference">{ctx["schema"].field(args[0])}</Parameter><Parameter type="Calculation">{calc(args[1], 1, 0)}</Parameter>'


def st_go_to_layout(ctx, args):
    a = args[0].strip() if args else "original"
    if a.lower() == "original":
        return 0, '<Parameter type="LayoutReferenceContainer"><LayoutReferenceContainer value="1"></LayoutReferenceContainer></Parameter><Parameter type="Animation"><Animation name="None" value="0"></Animation></Parameter>'
    name = unquote(a)
    lid = ctx["schema"].layouts.get(name)
    if lid is None:
        raise ValueError(f"unknown layout {name!r}")
    return 8, (f'<Parameter type="LayoutReferenceContainer"><LayoutReferenceContainer value="5"><LayoutReference id="{lid}" '
               f'name="{esc(name)}" UUID=""></LayoutReference></LayoutReferenceContainer></Parameter>'
               f'<Parameter type="Animation"><Animation name="None" value="0"></Animation></Parameter>')


def by_calculation(c):
    return (f'<List name="By Calculation…" value="5"><Boolean type="With dialog" position="159" value="False"></Boolean>'
            f'{calc(c, 1, 0)}</List>')


def st_go_to_record(ctx, args):
    c = keyword(args, "By Calculation")
    if c is not None:
        return 0x4082, f'<Parameter type="Records">{by_calculation(c)}</Parameter>'
    exit_last = keyword(args, "Exit after last")
    exit_last = exit_last is not None and onoff(exit_last)
    which = args[0].strip().title()
    v = {"First": 1, "Last": 2, "Previous": 3, "Next": 4}[which]
    return 2 | (0x2000 if exit_last else 0), (f'<Parameter type="Records"><List name="{which}" value="{v}">'
                                             f'<Boolean type="Exit after last" position="187" value="{exit_last}"></Boolean></List></Parameter>')


def st_with_dialog(extra):
    def f(ctx, args):
        dialog = keyword(args, "With dialog")
        dialog = dialog is not None and onoff(dialog)
        skip = any(a.strip().lower() == "skip validation" for a in args)
        o, x = (0 if dialog else 0x80), ""
        if extra:
            o |= 0x100 if skip else 0
            x += boolean("Skip data entry validation", 256, "True" if skip else "False")
        x += boolean("With dialog", 128, "True" if dialog else "False")
        if extra:
            x += boolean("Force Commit", 512, "False")
        return o, x
    return f


def st_pause(ctx, args):
    d = keyword(args, "Duration")
    if d is None:
        return 0, '<Parameter type="Options"><Options type="Indefinitely"></Options></Parameter>'
    return 0x4000, f'<Parameter type="Options"><Options type="Duration (seconds): ">{calc(d, 1, 0)}</Options></Parameter>'


def st_custom_dialog(ctx, args):
    x = f'<Parameter type="Title">{calc(args[0], 1, 0)}</Parameter>'
    if len(args) > 1:
        x += f'<Parameter type="Message">{calc(args[1], 1, 1)}</Parameter>'
    x += ('<Parameter type="Button1" value="OK"><Boolean type="Commit" value="True"></Boolean></Parameter>'
          '<Parameter type="Button2"><Boolean type="Commit" value="False"></Boolean></Parameter>'
          '<Parameter type="Button3"><Boolean type="Commit" value="False"></Boolean></Parameter>')
    return 0x4004, x


def st_plain(ctx, args):
    return 0, ""


# Import/export character sets by stored value: export (TExpCharacterSet)
# and import (FMImportOptions), with the names FileMaker's XML writes.
EXPORT_CHARSETS = {0: "Windows (ANSI)", 1: "ASCII (DOS)", 2: "Macintosh", 3: "Unicode (UTF-16)", 4: "Unicode (UTF-8)",
                   6: "Japanese (Shift-JIS)", 8: "Simplified Chinese (GB)", 9: "Traditional Chinese (Big-5)",
                   10: "Korean (EUC-KR)"}
IMPORT_CHARSETS = {0: "Windows ANSI", 1: "DOS", 2: "Mac Roman", 3: "Unicode", 4: "Unicode (big endian)", 5: "UTF-8",
                   6: "Shift JIS", 7: "Simplified Chinese (GB)", 8: "Traditional Chinese (Big-5)", 9: "Korean (EUC-KR)"}
GRAMMARS = {"FMPXMLRESULT": 1, "FMPDSORESULT": 2}


def path_list(args, file_type):
    """The quoted path arguments as FileMaker's XML lists them."""
    paths = [unquote(a) for a in list(args) if a.strip().startswith('"')]
    for p in paths:
        args.remove(f'"{p}"')
    locs = "".join(f"<Location><![CDATA[{p}]]></Location>" for p in paths)
    t = f' fileType="{file_type}"' if file_type else ""
    return paths, f'<UniversalPathList{t} membercount="{len(paths)}"><ObjectList>{locs}</ObjectList></UniversalPathList>'


def field_list(ctx, text):
    """Fields separated by "|"; "T::Summary > T::Break" is a summary field
    grouped by a break field (export order)."""
    out = []
    for f in (text or "").split("|"):
        if not f.strip():
            continue
        f, _, by = f.partition(">")
        x = ctx["schema"].field(f)
        if by.strip():
            x += f"<SummaryField>{ctx['schema'].field(by)}</SummaryField>"
        out.append(x)
    return out


def st_export(ctx, args):
    """Export Records [ "file:x.tab" ; Type: TABS ; Charset: 4 ; Formatting: On ;
    Fields: T::A | T::B ; Group: T::C ; Grammar: FMPXMLRESULT ; With dialog: Off ;
    Create folders: Off ]"""
    dialog = keyword(args, "With dialog")
    dialog = dialog is not None and onoff(dialog)
    folders = keyword(args, "Create folders")
    folders = folders is not None and onoff(folders)
    file_type = (keyword(args, "Type") or "TABS").ljust(4)
    charset = int(keyword(args, "Charset") or 4)
    formatting = keyword(args, "Formatting")
    formatting = formatting is not None and onoff(formatting)
    fields = field_list(ctx, keyword(args, "Fields"))
    groups = field_list(ctx, keyword(args, "Group"))
    grammar = keyword(args, "Grammar") or "FMPXMLRESULT"
    paths, upl = path_list(args, file_type)
    o = (0 if dialog else 0x80) | (0x200 if folders else 0) | (0x20 if paths else 0) | 0x8000
    o |= 0x2000000 if fields else 0
    x = boolean("With dialog", 128, "True" if dialog else "False")
    x += boolean("Create folders", 512, "True" if folders else "False")
    x += f'<Parameter type="UniversalPathList">{upl}</Parameter>'
    if file_type == "XMLE":
        x += (f'<Parameter type="DataSourceReference"><DataSourceReference fileType="XMLE" isPredefined="False" '
              f'delimiter="0" value="0"><XSL index="0" grammar="{grammar}"></XSL></DataSourceReference></Parameter>')
    x += (f'<Parameter type="Export"><Export><Options name="{EXPORT_CHARSETS[charset]}" value="{charset}" '
          f'Formatting="{formatting}"></Options><Order type="Group">'
          + "".join(f'<Group value="{i}">{g}</Group>' for i, g in enumerate(groups, 1)) + '</Order><Order type="Field">'
          + "".join(f'<Field value="{i}">{f}</Field>' for i, f in enumerate(fields, 1)) + "</Order></Export></Parameter>")
    return o, x


IMPORT_ACTIONS = {"add": (0, "Add"), "replace": (1, "Replace"), "update": (2, "Update")}


def st_import(ctx, args):
    """Import Records [ "file:in.tab" ; Type: TABS ; Target: Contacts ; Action: Add|Replace|Update ;
    Charset: 5 ; Names: 1 ; Delimiter: 0 ; Auto-enter: On ; Match names: On ;
    Split repetitions: On ; Add remaining: On ; Map: T::A | - | =T::B | !T::C | T::D +ae ;
    With dialog: Off ]
    Map entries in source column order: a field (import), "-" (no target),
    "=field" (match field), "!field" (don't import); "+ae" sets the field's
    own auto-enter option. Names is the 1-based row holding field names (0:
    none); FileMaker stores it 0-based (-1 none) and imports the rows after
    it. Delimiter is the stored delimiter text (default: a tab, a comma for
    COMS)."""
    dialog = keyword(args, "With dialog")
    dialog = dialog is not None and onoff(dialog)
    file_type = (keyword(args, "Type") or "TABS").ljust(4)
    target = keyword(args, "Target") or "Contacts"
    action, action_name = IMPORT_ACTIONS[(keyword(args, "Action") or "add").lower()]
    charset = int(keyword(args, "Charset") or 5)
    names = int(keyword(args, "Names") or 0)
    # FileMaker Pro stores the field delimiter as text (a tab for tab files);
    # FileMaker's XML importer stores this attribute literally, while its
    # writer shows a tab as "0".
    delimiter = keyword(args, "Delimiter") or {"COMS": ","}.get(file_type, "\t")
    opts = {}
    for label, attr in (("Auto-enter", "doAutoEntry"), ("Match names", "matchFieldNames"),
                        ("Split repetitions", "splitRepetitions"), ("Add remaining", "addasnew")):
        v = keyword(args, label)
        opts[attr] = v is not None and onoff(v)
    entries = [e.strip() for e in (keyword(args, "Map") or "").split("|") if e.strip()]
    paths, upl = path_list(args, file_type)
    if target not in ctx["schema"].tos:
        raise ValueError(f"unknown table occurrence {target!r}")
    maps = ""
    for i, e in enumerate(entries, 1):
        kind, ae = 0, e.endswith("+ae")
        e = e[:-3].strip() if ae else e
        if e == "-":
            maps += (f'<Map New="False" name="" index="{i}" id="0" kind="1" Options="0"></Map>')
            continue
        if e[0] in "=!":
            kind, e = (2 if e[0] == "=" else 1), e[1:].strip()
        ref = ctx["schema"].field(e)
        fid = re.search(r'id="(\d+)"', ref).group(1)
        maps += f'<Map New="False" name="" index="{i}" id="{fid}" kind="{kind}" Options="{2 if ae else 0}">{ref}</Map>'
    o = (0 if dialog else 0x80) | 0x2 | (0x20 if paths else 0) | 0x8000 | (0x2000000 if entries else 0)
    x = boolean("Verify SSL Certificates", 268435456, "False")
    x += boolean("With dialog", 128, "True" if dialog else "False")
    if file_type == "XML ":
        # an XML data source (step value 4): the file, no XSL style sheet
        x += (f'<Parameter type="DataSourceReference"><DataSourceReference fileType="XML " isPredefined="False" '
              f'delimiter="4" value="4"><XML index="1">{upl}</XML><XSL index="3"><Calculation></Calculation></XSL>'
              '</DataSourceReference></Parameter>')
    else:
        x += (f'<Parameter type="DataSourceReference"><DataSourceReference fileType="{file_type}" isPredefined="False" '
              f'delimiter="{int(names > 0)}" value="1">{upl}</DataSourceReference></Parameter>')
    x += (f'<Parameter type="ImportField"><ImportField addasnew="{opts["addasnew"]}" isPredefined="False" '
          f'delimiter="{esc(delimiter).replace(chr(9), "&#9;")}" fieldNameRow="{names - 1}"><Options keepOriginalData="False" copyContainersAsIs="False" '
          f'matchFieldNames="{opts["matchFieldNames"]}" doAutoEntry="{opts["doAutoEntry"]}" '
          f'splitRepetitions="{opts["splitRepetitions"]}"></Options><Target><TableOccurrenceReference '
          f'id="{ctx["schema"].tos[target]}" name="{esc(target)}" UUID=""></TableOccurrenceReference></Target>'
          f'<Field membercount="{len(entries)}">{maps}</Field><CharacterSet name="{IMPORT_CHARSETS[charset]}" '
          f'value="{charset}"></CharacterSet><action name="{action_name}" value="{action}"></action>'
          f'</ImportField></Parameter>')
    return o, x


def st_sort(ctx, args):
    """Sort Records [ Contacts::A ; Contacts::B desc ] (restored, no dialog)."""
    sorts = ""
    for a in args:
        m = re.match(r"(.+?)\s+(asc|desc)\s*$", a.strip(), re.I)
        ref, kind = (m.group(1), m.group(2).lower()) if m else (a, "asc")
        sorts += (f'<Sort type="{"Descending" if kind == "desc" else "Ascending"}"><PrimaryField>'
                  f'{ctx["schema"].field(ref)}</PrimaryField></Sort>')
    return 0x2008080, (boolean("With dialog", 128, "False") + '<Parameter type="Restore"><Restore value="True"></Restore></Parameter>'
                       f'<Parameter type="SortSpecification"><SortSpecification value="True" blanksLast="False" maintain="True">'
                       f'<SortList membercount="{len(args)}">{sorts}</SortList></SortSpecification></Parameter>')


def flag(args, key, default=False):
    """Pop 'Key: On|Off'; default when absent."""
    v = keyword(args, key)
    return default if v is None else onoff(v)


def st_set_field_by_name(ctx, args):
    """Set Field By Name [ target name calc ; value calc ] (the shape FileMaker's
    XML writes for the step: "Specify target field", target slot 1, value slot 0)."""
    return 0x8004000, (boolean("Specify target field", 134217728, "True") + f'<Parameter type="Calculation">{calc(args[0], 1, 1)}</Parameter>'
                       f'<Parameter type="Calculation">{calc(args[1], 1, 0)}</Parameter>')


def st_go_to_portal_row(ctx, args):
    """Go to Portal Row [ First|Last|Previous|Next|By calculation: n ; Select: On ;
    Exit after last: On ; With dialog: On ] (With dialog Off unless given, so
    By calculation asks nothing in FileMaker Pro)."""
    select, exit_last, dialog = flag(args, "Select"), flag(args, "Exit after last"), flag(args, "With dialog")
    n = keyword(args, "By calculation")
    o, x = 2 | (0x1000 if select else 0), boolean("Select", 4096, str(select))
    if n is not None:
        o |= 0x4000 | (0 if dialog else 0x80)
        return o, x + (f'<Parameter type="Portal"><List name="By Calculation…" value="5"><Boolean type="With dialog" '
                       f'position="159" value="{dialog}"></Boolean>{calc(n, 1, 0)}</List></Parameter>')
    which = args[0].strip().title()
    v = {"First": 1, "Last": 2, "Previous": 3, "Next": 4}[which]
    inner = f'<Boolean type="Exit after last" position="187" value="{exit_last}"></Boolean>' if v in (3, 4) else ""
    return o | (0x2000 if exit_last else 0), x + f'<Parameter type="Portal"><List name="{which}" value="{v}">{inner}</List></Parameter>'


def st_go_to_object(ctx, args):
    """Go to Object [ Object Name: calc ; Repetition: calc ]"""
    rep = keyword(args, "Repetition")
    name = keyword(args, "Object Name") or args[0]
    r = f"<repetition>{calc(rep, 1, 1)}</repetition>" if rep else "<repetition></repetition>"
    return 0x4000, f'<Parameter type="Object"><Name>{calc(name, 1, 0)}</Name>{r}</Parameter>'


def st_go_to_field(ctx, args):
    """Go to Field [ Select: On ; T::F[rep] ] (or "Select/perform"), or Go to
    Field [ ] (leave the field)."""
    select = flag(args, "Select") or any(a.strip().lower() == "select/perform" for a in args)
    refs = [a for a in args if "::" in a]
    x = boolean("Select/perform", 4096, str(select))
    if refs:
        x += f'<Parameter type="FieldReference">{ctx["schema"].field(refs[0], 10, "1")}</Parameter>'
    return (1 if refs else 0) | (0x1000 if select else 0), x


def st_go_to_related(ctx, args):
    """Go to Related Record [ Show only related records: On ; From table: "TO" ;
    Using layout: "Layout" ]"""
    only = flag(args, "Show only related records")
    to = ctx["schema"].occurrence(keyword(args, "From table"))
    layout = ctx["schema"].layout(keyword(args, "Using layout"))
    shown = ' ShowRelated="True"' if only else ""
    return 8 | (0x2000000 if only else 0), (f'<Parameter type="Related">{to}<LayoutReferenceContainer value="5">{layout}'
                                            '</LayoutReferenceContainer><Animation name="None" value="0"></Animation>'
                                            f'<Options{shown}></Options></Parameter>')


# New Window's Document style (the option word FileMaker's XML writes with it).
DOCUMENT_STYLE = 983554


def st_new_window(ctx, args):
    """New Window [ Name: calc ; Layout: "L" ; Height: n ; Width: n ; Top: n ; Left: n ]
    (Document style; without Layout the original layout)."""
    name, layout = keyword(args, "Name"), keyword(args, "Layout")
    bounds = {k: keyword(args, k.title()) for k in ("height", "width", "top", "left")}
    o = (0x4000 if name or any(bounds.values()) else 0) | (8 if layout else 0)
    lx = (f'<LayoutReferenceContainer value="5">{ctx["schema"].layout(layout)}</LayoutReferenceContainer>' if layout
          else '<LayoutReferenceContainer value="1"><Label>original layout</Label></LayoutReferenceContainer>')
    bx = "".join(f"<{k}>{calc(v, 1, i)}</{k}>" if v else f"<{k}></{k}>" for i, (k, v) in enumerate(bounds.items(), 1))
    return o, (f'<Parameter type="WindowReference"><WindowReference><Style name="Document" value="{DOCUMENT_STYLE}"></Style>'
               f'<Name>{calc(name, 1, 0) if name else ""}</Name>{lx}<Bounds>{bx}</Bounds><Options value="{DOCUMENT_STYLE}">'
               '<Close>True</Close><Minimize>True</Minimize><Maximize>True</Maximize><Resize>True</Resize>'
               '<MenuBar>False</MenuBar><Toolbar>False</Toolbar><DimParentWindow>False</DimParentWindow></Options>'
               '</WindowReference></Parameter>')


def st_window_ref(ctx, args):
    """Select Window / Close Window [ Current Window ] or [ Name: calc ; Current file ]"""
    name = keyword(args, "Name")
    current_file = any(a.strip().lower() == "current file" for a in args)
    o = -0x80000000 if current_file else 0
    if name is None:
        return o, '<Parameter type="WindowReference"><WindowReference><Select kind="0" type="current"></Select></WindowReference></Parameter>'
    return o | 0x4002, (f'<Parameter type="WindowReference"><WindowReference><Select kind="1" type="Calculated"><Name current="{current_file}">'
                        f'{calc(name, 1, 0)}</Name></Select></WindowReference></Parameter>')


def st_enter_find_mode(ctx, args):
    """Enter Find Mode [ Pause: Off ] (no stored requests)."""
    pause = flag(args, "Pause")
    return (0x1000000 if pause else 0), boolean("Pause", 16777216, str(pause))


def select_option(args):
    """Pop 'Select: On|Off' (default Off): (option bit 0x1000, its value)."""
    sel = keyword(args, "Select")
    sel = sel is not None and onoff(sel)
    return (0x1000 if sel else 0), ("True" if sel else "False")


def st_insert_text(ctx, args):
    """Insert Text [ Select: On ; T::F ; "text" ] (target optional: the active field)."""
    o, sel = select_option(args)
    text = unquote(args.pop())
    o |= 4
    x = boolean("Select", 4096, sel)
    if args:
        o |= 1
        x += f'<Parameter type="Target">{ctx["schema"].field(args[0], 32, "1")}</Parameter>'
    return o, x + f'<Parameter type="Text"><Text value="{esc(text)}"></Text></Parameter>'


def st_set_selection(ctx, args):
    """Set Selection [ T::F ; start ; end ]"""
    return 0x4001, (f'<Parameter type="FieldReference">{ctx["schema"].field(args[0], 10, "1")}</Parameter>'
                    f'<Parameter type="Select"><Start>{calc(args[1], 1, 0)}</Start><End>{calc(args[2], 1, 1)}</End></Parameter>')


def st_mode(ctx, args):
    """Enter Browse/Find/Preview Mode [ Pause: Off ]"""
    pause = keyword(args, "Pause")
    if pause is not None and onoff(pause):
        raise ValueError("only Pause: Off is supported")
    return 0, boolean("Pause", 16777216, "False")


def st_calculation(ctx, args):
    return 0x4000, f'<Parameter type="Calculation">{calc(args[0], 1, 0)}</Parameter>'


def st_move_resize_window(ctx, args):
    """Move/Resize Window [ Current ; Height: h ; Width: w ; Top: t ; Left: l ]"""
    bounds = ""
    for i, key in enumerate(("Height", "Width", "Top", "Left"), 1):
        v = keyword(args, key)
        bounds += f"<{key.lower()}>{calc(v, 1, i) if v else ''}</{key.lower()}>"
    if [a for a in args if a.strip()] != ["Current"]:
        raise ValueError("only the current window is supported")
    return -2147467262, (f'<Parameter type="WindowReference"><WindowReference><Select kind="0" type="current"></Select>'
                         f'<Bounds>{bounds}</Bounds></WindowReference></Parameter>')


ZOOM = {"25%": 1, "50%": 2, "75%": 3, "100%": 4, "150%": 5, "200%": 6, "300%": 7, "400%": 8}
TOOLBARS = {"Show": 1, "Hide": 2, "Toggle": 3}
VIEWS = {"Form": 1, "List": 2, "Table": 3}


def st_zoom(ctx, args):
    z = args[0].strip()
    return 2, boolean("Lock", 524288, "False") + f'<Parameter type="List"><List name="{z}" value="{ZOOM[z]}"></List></Parameter>'


def st_toolbars(ctx, args):
    t = args[0].strip().title()
    return 2, (boolean("Lock", 524288, "False") + boolean("Include Edit Record Toolbar", 256, "False")
               + f'<Parameter type="List"><List name="{t}" value="{TOOLBARS[t]}"></List></Parameter>')


def st_view_as(ctx, args):
    v = args[0].strip().title()
    return 2, f'<Parameter type="List"><List name="View as {v}" value="{VIEWS[v]}"></List></Parameter>'


def st_set_web_viewer(ctx, args):
    """Set Web Viewer [ Object: "name" ; Go to URL: calc ]"""
    obj, url = keyword(args, "Object"), keyword(args, "Go to URL")
    return 0x4000, (f'<Parameter type="Calculation">{calc(obj, 1, 0)}</Parameter><Parameter type="action">'
                    f'<List name="Go to URL..." value="5">{calc(url, 1, 1)}</List></Parameter>')


def st_save_pdf(ctx, args):
    """Save Records as PDF [ $path ] (records being browsed, no dialog)."""
    return 0x40A0, ('<Parameter type="Restore"><Restore value="False"></Restore></Parameter>'
                    '<Parameter type="Options"><Options></Options></Parameter>'
                    + boolean("With dialog", 128, "False") + boolean("Append to existing PDF", 256, "False")
                    + f'<Parameter type="UniversalPathList"><UniversalPathList membercount="1"><ObjectList>'
                    f'<Location><![CDATA[{args[0].strip()}]]></Location></ObjectList></UniversalPathList></Parameter>'
                    + boolean("Create folders", 512, "False")
                    + '<Parameter type="SaveResult"><List name="File" value="0"></List></Parameter>')


# name -> (step id, builder)
def st_insert_from_url(ctx, args):
    """Insert from URL [ Select: On ; Verify SSL: Off ; T::F ; url calc ]: a
    field target only (shape of the MBS ATB sample's step, With dialog
    Off)."""
    select = keyword(args, "Select")
    select = select is None or onoff(select)
    verify = keyword(args, "Verify SSL")
    verify = verify is not None and onoff(verify)
    target, url = args[0], args[1]
    o = 0x4000 | 0x80 | 0x1 | (0x1000 if select else 0) | (0x10000000 if verify else 0)
    f = ctx["schema"].field(target)
    if "<repetition>" not in f:
        f = f.replace('UUID="">', f'UUID=""><repetition>{calc("1", 1, 32)}</repetition>', 1)
    x = boolean("Verify SSL Certificates", 268435456, "True" if verify else "False")
    x += boolean("Select", 4096, "True" if select else "False")
    x += boolean("With dialog", 128, "False")
    x += f'<Parameter type="Target">{f}</Parameter>'
    x += f'<Parameter type="URL"><URL autoEncode="True">{calc(url, 1, 0)}</URL></Parameter>'
    return o, x


STEPS = {
    "Perform Script": (1, st_perform_script),
    "Go to Layout": (6, st_go_to_layout),
    "New Record/Request": (7, st_plain),
    "Delete Record/Request": (9, st_with_dialog(False)),
    "Delete All Records": (10, st_with_dialog(False)),
    "Show All Records": (23, st_plain),
    "Omit Record": (25, st_plain),
    "Show Omitted Only": (27, st_plain),
    "Import Records": (35, st_import),
    "Export Records": (36, st_export),
    "Sort Records": (39, st_sort),
    "Go to Record/Request/Page": (16, st_go_to_record),
    "Go to Field": (17, st_go_to_field),
    "Unsort Records": (21, st_plain),
    "Enter Find Mode": (22, st_enter_find_mode),
    "Perform Find": (28, st_plain),
    "Show/Hide Toolbars": (29, st_toolbars),
    "View As": (30, st_view_as),
    "Enter Preview Mode": (41, st_mode),
    "Revert Record/Request": (51, st_with_dialog(False)),
    "Enter Browse Mode": (55, st_mode),
    "Insert Text": (61, st_insert_text),
    "Set Zoom Level": (97, st_zoom),
    "Move/Resize Window": (119, st_move_resize_window),
    "Set Selection": (130, st_set_selection),
    "Save Records as PDF": (144, st_save_pdf),
    "Set Web Viewer": (146, st_set_web_viewer),
    "Perform Quick Find": (150, st_calculation),
    "Pause/Resume Script": (62, st_pause),
    "If": (68, st_condition(True)),
    "Else": (69, st_else),
    "End If": (70, st_plain),
    "Loop": (71, st_loop),
    "Exit Loop If": (72, st_condition(False)),
    "End Loop": (73, st_plain),
    "Go to Related Record": (74, st_go_to_related),
    "Commit Records/Requests": (75, st_with_dialog(True)),
    "Set Field": (76, st_set_field),
    "Freeze Window": (79, st_plain),
    "Allow User Abort": (85, st_onoff),
    "Set Error Capture": (86, st_onoff),
    "Show Custom Dialog": (87, st_custom_dialog),
    "Halt Script": (90, st_plain),
    "Beep": (93, st_plain),
    "Go to Portal Row": (99, st_go_to_portal_row),
    "Exit Script": (103, st_exit_script),
    "Delete Portal Row": (104, st_with_dialog(False)),
    "Close Window": (121, st_window_ref),
    "New Window": (122, st_new_window),
    "Select Window": (123, st_window_ref),
    "Else If": (125, st_condition(True)),
    "Set Variable": (141, st_set_variable),
    "Go to Object": (145, st_go_to_object),
    "Set Field By Name": (147, st_set_field_by_name),
    "Perform Script on Server": (164, st_psos),
    "Open Transaction": (205, st_open_transaction),
    "Commit Transaction": (206, st_plain),
    "Revert Transaction": (207, st_revert_transaction),
    "Set Revert Transaction on Error": (223, st_onoff),
    "Insert from URL": (160, st_insert_from_url),
}


# FileMaker's XML names of the layout events (script-runtime.md 7.1).
LAYOUT_EVENTS = {
    "OnLayoutEnter": 101, "OnLayoutKeystroke": 102, "OnRecordLoad": 103, "OnRecordCommit": 104,
    "OnRecordRevert": 105, "OnModeEnter": 106, "OnModeExit": 107, "OnLayoutExit": 109,
    "OnViewChange": 110, "OnLayoutSizeChange": 111, "OnGestureTap": 112, "OnExternalCommandReceived": 113,
}


# FileMaker's XML names of the file events (script-runtime.md 7.1).
FILE_EVENTS = {"OnFirstWindowOpen": 201, "OnLastWindowClose": 202, "OnWindowOpen": 204, "OnWindowClose": 205,
               "OnFileAVPlayerChange": 206, "OnWindowTransaction": 209}

# Schema directives (generate only) and their minimum number of fields.
SCHEMA_DIRECTIVES = {"@generate": 0, "@layout": 3, "@field": 3, "@filetrigger": 2, "@validate": 3, "@relationship": 3,
                     "@portal": 3, "@name": 3}


def parse(path):
    """The probe scripts, the "@trigger" lines and the schema directives."""
    scripts, triggers, directives, cur = [], [], [], None
    for n, raw in enumerate(open(path, encoding="utf-8"), 1):
        line = raw.rstrip("\n").strip()
        if line.startswith("@trigger"):
            fields = raw.rstrip("\n").split("\t")[1:]
            if len(fields) < 3 or fields[1] not in LAYOUT_EVENTS:
                raise ValueError(f"line {n}: @trigger <TAB> layout <TAB> event <TAB> script [<TAB> modes [<TAB> parameter]]")
            triggers.append(fields + [""] * (5 - len(fields)))
            continue
        if line.split("\t")[0] in SCHEMA_DIRECTIVES:
            fields = raw.rstrip("\n").split("\t")
            if len(fields) - 1 < SCHEMA_DIRECTIVES[fields[0]]:
                raise ValueError(f"line {n}: {fields[0]} needs {SCHEMA_DIRECTIVES[fields[0]]} tab-separated fields")
            directives.append(fields)
            continue
        if line.startswith("=="):
            m = re.match(r"==\s*(.+?)\s*(\[full access\])?\s*$", line)
            cur = {"name": m.group(1), "full": bool(m.group(2)), "steps": []}
            scripts.append(cur)
            continue
        if cur is None or not line:
            continue
        cur["steps"].append((n, line))
    return scripts, triggers, directives


def set_triggers(ctx, triggers):
    """Replace the ScriptTriggers of the schema's layouts named by triggers,
    in the shape FileMaker's Save as XML writes them."""
    holders = {}
    for layout, event, script, modes, param in triggers:
        if layout not in holders:
            element = ctx["schema"].layout_elements.get(layout)
            if element is None:
                raise ValueError(f"unknown layout {layout!r}")
            for old in element.findall("ScriptTriggers"):
                element.remove(old)
            holders[layout] = ET.Element("ScriptTriggers")
            element.insert(list(element).index(element.find("Options")) + 1, holders[layout])
        holder = holders[layout]
        sid, su = ctx["scripts"].get(script) or ctx["schema"].scripts.get(script) or (None, None)
        if sid is None:
            raise ValueError(f"unknown script {script!r}")
        attrs = {"id": str(LAYOUT_EVENTS[event]), "action": event}
        for flag, attr in (("B", "browseMode"), ("F", "findMode"), ("P", "previewMode")):
            if flag in (modes or "B").upper():
                attrs[attr] = "True"
        trigger = ET.SubElement(holder, "ScriptTrigger", attrs)
        ref = ET.SubElement(trigger, "ScriptReference", {"id": sid, "name": script, "UUID": su})
        c = ET.SubElement(ref, "Calculation")
        if param:
            ET.SubElement(c, "Text").text = param
        holder.set("membercount", str(len(holder)))


def add_layout(schema, name, to, source):
    """A copy of layout `source` named `name` on occurrence `to`, without
    layout objects, table view columns or triggers (appended to the
    catalog)."""
    import copy
    src = schema.layout_elements.get(source)
    if src is None or to not in schema.tos:
        raise ValueError(f"@layout: unknown layout {source!r} or occurrence {to!r}")
    lay = copy.deepcopy(src)
    lid = str(max(int(l.get("id")) for l in schema.root.iter("Layout")) + 1)
    lay.set("id", lid)
    lay.set("name", name)
    ref = lay.find("TableOccurrenceReference")
    ref.attrib = {"id": schema.tos[to], "name": to, "UUID": ""}
    for ol in lay.iter("ObjectList"):
        for child in list(ol):
            ol.remove(child)
        ol.set("membercount", "0")
    for old in lay.findall("ScriptTriggers"):
        lay.remove(old)
    lay.find("UUID").text = str(uuid.uuid5(uuid.NAMESPACE_URL, "om-probe-layout:" + name)).upper()
    catalog = next(schema.root.iter("LayoutCatalog"))
    catalog.append(lay)
    catalog.set("membercount", str(int(catalog.get("membercount", "0")) + 1))
    schema.layouts[name], schema.layout_elements[name] = lid, lay


def add_field(schema, to, name, source):
    """A copy of field `source` of TO's base table named `name` (next id)."""
    import copy
    base = schema.base_of.get(to)
    for cat in schema.root.iter("FieldCatalog"):
        if cat.find("BaseTableReference").get("name") != base:
            continue
        ol = cat.find("ObjectList")
        src = next((x for x in ol if x.tag == "Field" and x.get("name") == source), None)
        if src is None:
            raise ValueError(f"@field: no field {source!r} in {base!r}")
        f = copy.deepcopy(src)
        fid = str(max(int(x.get("id")) for x in ol if x.tag == "Field") + 1)
        f.set("id", fid)
        f.set("name", name)
        f.find("UUID").text = str(uuid.uuid5(uuid.NAMESPACE_URL, f"om-probe-field:{base}:{name}")).upper()
        ol.append(f)
        ol.set("membercount", str(len(ol)))
        order = cat.find("CustomOrderList")
        if order is not None:
            ET.SubElement(order, "key").text = fid
        schema.fields[(base, name)] = fid
        return
    raise ValueError(f"@field: unknown occurrence {to!r}")


def find_object(schema, layout, target):
    """The first layout object of `layout` matching `portal:TO` or `TO::Field`."""
    lay = schema.layout_elements.get(layout)
    if lay is None:
        raise ValueError(f"unknown layout {layout!r}")
    for o in lay.iter("LayoutObject"):
        if target.startswith("portal:"):
            r = o.find("Portal/TableOccurrenceReference")
            if o.get("type") == "Portal" and r is not None and r.get("name") == target[7:]:
                return o
        else:
            f = o.find("Field/FieldReference")
            if f is not None and f"{f.find('TableOccurrenceReference').get('name')}::{f.get('name')}" == target:
                return o
    raise ValueError(f"no object {target!r} on layout {layout!r}")


def apply_directives(ctx, directives):
    """The schema directives other than @layout (applied before the steps
    are built, so that probes can name the new layouts)."""
    schema = ctx["schema"]
    file_triggers = []
    for d in directives:
        kind, f = d[0], d[1:]
        if kind == "@filetrigger":
            if f[0] not in FILE_EVENTS:
                raise ValueError(f"unknown file event {f[0]!r}")
            file_triggers.append(f + [""] * (3 - len(f)))
        elif kind == "@validate":
            # Table::Field, Always|DataEntry, calculation: a strict rule by
            # calculation, in the shape FileMaker's XML writes it.
            m = re.match(r"(.+?)::(.+)$", f[0])
            to, name = m.groups()
            base = schema.base_of.get(to)
            for cat in schema.root.iter("FieldCatalog"):
                if cat.find("BaseTableReference").get("name") != base:
                    continue
                field = next(x for x in cat.find("ObjectList") if x.tag == "Field" and x.get("name") == name)
                old = field.find("Validation")
                v = ET.fromstring(
                    f'<Validation alwaysValidate="False" type="{"Always" if f[1] == "Always" else "OnlyDuringDataEntry"}" '
                    'allowOverride="False" notEmpty="False" unique="False" existing="False"><Calculated enable="True">'
                    f'<Calculation><TableOccurrenceReference id="{schema.tos[to]}" name="{esc(to)}" UUID=""></TableOccurrenceReference>'
                    f'<Text><![CDATA[{f[2]}]]></Text></Calculation></Calculated></Validation>')
                field.insert(list(field).index(old), v)
                field.remove(old)
        elif kind == "@relationship":
            # TO, other TO, attr=value ...: the options of TO's side.
            done = False
            for rel in schema.root.iter("Relationship"):
                sides = {s.find("TableOccurrenceReference").get("name"): s for s in (rel.find("LeftTable"), rel.find("RightTable"))}
                if set(sides) == {f[0], f[1]}:
                    for a in f[2:]:
                        k, _, v = a.partition("=")
                        sides[f[0]].set(k.strip(), v.strip())
                    done = True
            if not done:
                raise ValueError(f"no relationship {f[0]!r} - {f[1]!r}")
        elif kind == "@portal":
            # layout, portal TO, portal options word
            find_object(schema, f[0], "portal:" + f[1]).find("Portal/Options").text = f[2]
        elif kind == "@name":
            # layout, portal:TO or TO::Field, object name
            find_object(schema, f[0], f[1]).set("name", f[2])
    if file_triggers:
        md = schema.root.find("Metadata/AddAction")
        for old in md.findall("ScriptTriggers"):
            md.remove(old)
        holder = ET.Element("ScriptTriggers", {"membercount": str(len(file_triggers))})
        md.insert(list(md).index(md.find("UseDefaultFields")) + 1, holder)
        for event, script, field_name in file_triggers:
            sid, su = ctx["scripts"].get(script) or schema.scripts.get(script) or (None, None)
            if sid is None:
                raise ValueError(f"unknown script {script!r}")
            attrs = {"id": str(FILE_EVENTS[event]), "action": event, "browseMode": "True"}
            if field_name:
                attrs["scriptParameterFieldName"] = field_name
            t = ET.SubElement(holder, "ScriptTrigger", attrs)
            ET.SubElement(t, "ScriptReference", {"id": sid, "name": script, "UUID": su})


def append(catalog, items):
    """Add XML items to a catalog element and count them."""
    for item in items:
        catalog.append(ET.fromstring(item))
    catalog.set("membercount", str(int(catalog.get("membercount", "0")) + len(items)))


def step_xml(ctx, index, n, line):
    enable = "True"
    if line.startswith("//"):
        enable, line = "False", line[2:].strip()
    if line.startswith("#"):
        text = line[1:].strip()
        return (f'<Step index="{index}" id="89" name="# (comment)" enable="{enable}"><Options>4</Options>'
                f'<ParameterValues membercount="1"><Parameter type="Comment"><Comment value="{esc(text)}"></Comment></Parameter></ParameterValues></Step>')
    m = re.match(r"Step\s+(\d+)\s*$", line)
    if m:
        return f'<Step index="{index}" id="{m.group(1)}" name="" enable="{enable}"><Options>0</Options></Step>'
    m = re.match(r"(.+?)\s*\[(.*)\]\s*$", line, re.S)
    name, args = (m.group(1), split_args(m.group(2))) if m else (line, [])
    if name not in STEPS:
        raise ValueError(f"line {n}: unknown step {name!r}")
    sid, build = STEPS[name]
    try:
        options, params = build(ctx, args)
    except (ValueError, IndexError, KeyError) as e:
        raise ValueError(f"line {n}: {line}: {e}") from None
    count = params.count('<Parameter type=')
    pv = f'<ParameterValues membercount="{count}">{params}</ParameterValues>' if params else ""
    return f'<Step index="{index}" id="{sid}" name="{esc(name)}" enable="{enable}"><Options>{options}</Options>{pv}</Step>'


def build(schema, scripts, directives=(), generate=True):
    """The ScriptCatalog and StepsForScripts items of the probe scripts
    (dicts with name, full and steps as (line number, line)), and the context
    that resolves their names. Schema directives (`@layout`, `@field`, ...)
    change the schema first; they need a generated file."""
    ids = {}
    for i, s in enumerate(scripts):
        ids[s["name"]] = (str(1000 + i), str(uuid.uuid5(uuid.NAMESPACE_URL, "om-probe:" + s["name"])).upper())
    ctx = {"schema": schema, "scripts": ids}
    if directives and not generate:
        raise ValueError("schema directives need --generate")
    for d in directives:
        if d[0] == "@layout":
            add_layout(schema, *d[1:4])
        elif d[0] == "@field":
            add_field(schema, *d[1:4])
    apply_directives(ctx, directives)
    cat, steps = [], []
    for s in scripts:
        sid, su = ids[s["name"]]
        cat.append(f'<Script id="{sid}" name="{esc(s["name"])}"><UUID>{su}</UUID>'
                   f'<Options hidden="False" access="ReadWrite" SiriShortcutVisible="False" '
                   f'runwithfullaccess="{s["full"]}" compatibility="0">0</Options><TagList></TagList></Script>')
        body = [step_xml(ctx, i, n, line) for i, (n, line) in enumerate(s["steps"])]
        steps.append(f'<Script><ScriptReference id="{sid}" name="{esc(s["name"])}" UUID="{su}"></ScriptReference>'
                     f'<ObjectList membercount="{len(body)}">' + "\n".join(body) + "</ObjectList></Script>")
    return ctx, cat, steps


def main():
    generate = "--generate" in sys.argv
    probes, schema_path = [a for a in sys.argv[1:] if a != "--generate"][:2]
    schema = Schema(schema_path)
    scripts, triggers, directives = parse(probes)
    ctx, cat, steps = build(schema, scripts, directives, generate)
    if generate:
        if schema.script_catalog is None or schema.steps_for_scripts is None:
            raise ValueError("the schema has no ScriptCatalog or StepsForScripts to add to")
        append(schema.script_catalog, cat)
        append(schema.steps_for_scripts, steps)
        set_triggers(ctx, triggers)
        sys.stdout.write('<?xml version="1.0" encoding="UTF-8"?>\n')
        sys.stdout.write(ET.tostring(schema.root, encoding="unicode"))
        return
    if triggers:
        raise ValueError("@trigger needs --generate: FMUpgradeTool's patches do not replace layouts")
    print('<?xml version="1.0" encoding="UTF-8"?>')
    print('<FMUpgradeToolPatch version="2.3.0.0" Source="26.0.3" locale="English"><Structure membercount="1">'
          '<AddAction membercount="2">')
    print(f'<ScriptCatalog membercount="{len(cat)}">' + "\n".join(cat) + "</ScriptCatalog>")
    print(f'<StepsForScripts membercount="{len(steps)}">' + "\n".join(steps) + "</StepsForScripts>")
    print("</AddAction></Structure></FMUpgradeToolPatch>")


if __name__ == "__main__":
    main()
