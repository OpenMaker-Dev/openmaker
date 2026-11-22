# Add to contacts-en's saveAsXML document, in Contacts (field ids 14-19),
# formulas over an unstored calculation and a summary field, for script
# oracle s44-unstored-reads:
#   U   unstored calculation, text: First Name & "!"
#   A1  text, auto-entered U & "?", replacing the value
#   A2  text, auto-entered U & "?", "Do not replace existing value"
#   V1  text, validated Always by U ≠ "x!", also when not modified
#   S   summary: Count of First Name
#   A3  text, auto-entered S & "#", replacing the value
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src, encoding='utf-8').read()
to = '<TableOccurrenceReference id="1065090" name="Contacts" UUID=""></TableOccurrenceReference>'
storage = ('<Storage autoIndex="True" index="None" global="False" maxRepetitions="1">'
           '<LanguageReference name="English" id="21"></LanguageReference></Storage>')
tail = '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>'
no_rule = ('<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="True" '
           'notEmpty="False" unique="False" existing="False"></Validation>')


def unstored(fid, name, text):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Calculated" datatype="Text" comment="">'
            '<AutoEnter alwaysEvaluate="False"></AutoEnter>'
            '<Storage storeCalculationResults="False" global="False" maxRepetitions="1">'
            '<LanguageReference name="English" id="21"></LanguageReference></Storage>'
            f'<Calculation>{to}<Text><![CDATA[{text}]]></Text></Calculation>' + tail)


def auto(fid, name, text, replace):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Normal" datatype="Text" comment="">'
            f'<AutoEnter type="Calculated" prohibitModification="False" overwriteExisting="{replace}" alwaysEvaluate="False">'
            f'<Calculated enable="True"><Calculation>{to}<Text><![CDATA[{text}]]></Text></Calculation></Calculated></AutoEnter>'
            + no_rule + storage + tail)


def validated(fid, name, text):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Normal" datatype="Text" comment="">'
            '<AutoEnter type="" prohibitModification="False"></AutoEnter>'
            '<Validation alwaysValidate="True" type="Always" allowOverride="False" notEmpty="False" '
            'unique="False" existing="False"><Calculated enable="True">'
            f'<Calculation>{to}<Text><![CDATA[{text}]]></Text></Calculation></Calculated></Validation>'
            + storage + tail)


def summary(fid, name, of_id, of_name):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Summary" datatype="Number" comment="">'
            '<AutoEnter type="" prohibitModification="False"></AutoEnter>'
            '<SummaryInfo restartEachGroup="False" summarizeRepetition="Together" operation="Count">'
            f'<SummaryField><FieldReference id="{of_id}" name="{of_name}" UUID="">'
            '<BaseTableReference id="130" name="Contacts" UUID=""></BaseTableReference>'
            '</FieldReference></SummaryField></SummaryInfo>' + tail)


new = (unstored(14, "U", 'First Name & "!"')
       + auto(15, "A1", 'U & "?"', "True")
       + auto(16, "A2", 'U & "?"', "False")
       + validated(17, "V1", 'U ≠ "x!"')
       + summary(18, "S", 1, "First Name")
       + auto(19, "A3", 'S & "#"', "True"))
i = s.find('name="ModificationTimestamp"')
end = s.find('</Field>', i) + len('</Field>')
s = s[:end] + new + s[end:]
k = s.rfind('<key>13</key>', 0, i)
s = s[:k] + ''.join(f'<key>{n}</key>' for n in range(13, 20)) + s[k + len('<key>13</key>'):]
m = s.rfind('<ObjectList membercount="13">', 0, i)
s = s[:m] + '<ObjectList membercount="19">' + s[m + len('<ObjectList membercount="13">'):]
open(dst, 'w', encoding='utf-8').write(s)
