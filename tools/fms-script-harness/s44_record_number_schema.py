# Add auto-entered Get ( RecordNumber ) and Get ( FoundCount ) fields to
# contacts-en's saveAsXML document (field ids 14 and 15), for script
# oracle s44-record-number-ae: what a record a script creates stores.
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src, encoding='utf-8').read()
to = '<TableOccurrenceReference id="1065090" name="Contacts" UUID="06EEB6C5-DA8C-475E-A69F-EA5B69C6A96D"></TableOccurrenceReference>'
def ae_field(fid, name, text):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Normal" datatype="Text" comment="">'
            '<AutoEnter type="Calculated" prohibitModification="False" overwriteExisting="True" alwaysEvaluate="False">'
            f'<Calculated enable="True"><Calculation>{to}<Text><![CDATA[{text}]]></Text></Calculation></Calculated></AutoEnter>'
            '<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="True" notEmpty="False" unique="False" existing="False"></Validation>'
            '<Storage autoIndex="True" index="None" global="False" maxRepetitions="1">'
            '<LanguageReference name="English" id="21"></LanguageReference></Storage>'
            '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>')
new = ae_field(14, "RecNum", "Get ( RecordNumber )") + ae_field(15, "FoundAE", "Get ( FoundCount )")
i = s.find('name="ModificationTimestamp"')
end = s.find('</Field>', i) + len('</Field>')
s = s[:end] + new + s[end:]
k = s.rfind('<key>13</key>', 0, i)
s = s[:k] + '<key>13</key><key>14</key><key>15</key>' + s[k + len('<key>13</key>'):]
open(dst, 'w', encoding='utf-8').write(s)
