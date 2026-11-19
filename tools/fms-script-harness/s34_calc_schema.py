# Add calculations reading Contacts::Photo to contacts-en's saveAsXML
# document: stored calculations and an auto-enter calculation (field ids
# 14-18), for script oracle s34-containers-calc.
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src, encoding='utf-8').read()
to = '<TableOccurrenceReference id="1065090" name="Contacts" UUID="06EEB6C5-DA8C-475E-A69F-EA5B69C6A96D"></TableOccurrenceReference>'
def calc_field(fid, name, datatype, text):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Calculated" datatype="{datatype}" comment="">'
            '<AutoEnter alwaysEvaluate="False"></AutoEnter>'
            '<Storage storeCalculationResults="True" autoIndex="True" index="None" global="False" maxRepetitions="1">'
            '<LanguageReference name="English" id="21"></LanguageReference></Storage>'
            f'<Calculation>{to}<Text><![CDATA[{text}]]></Text></Calculation>'
            '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>')
def ae_field(fid, name, text):
    return (f'<Field id="{fid}" name="{name}" fieldtype="Normal" datatype="Text" comment="">'
            '<AutoEnter type="Calculated" prohibitModification="False" overwriteExisting="True" alwaysEvaluate="False">'
            f'<Calculated enable="True"><Calculation>{to}<Text><![CDATA[{text}]]></Text></Calculation></Calculated></AutoEnter>'
            '<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="True" notEmpty="False" unique="False" existing="False"></Validation>'
            '<Storage autoIndex="True" index="None" global="False" maxRepetitions="1">'
            '<LanguageReference name="English" id="21"></LanguageReference></Storage>'
            '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>')
new = (calc_field(14, "PhotoName", "Text", 'GetContainerAttribute ( Photo ; "filename" )')
       + calc_field(15, "PhotoLength", "Number", "Length ( Photo )")
       + calc_field(16, "PhotoB64", "Text", "Base64Encode ( Photo )")
       + ae_field(17, "PhotoAE", 'Length ( Photo ) & "|" & GetAsText ( Photo ) & "|" & GetWidth ( Photo )')
       + calc_field(18, "PhotoText", "Text", "GetAsText ( Photo ) & \"|\" & GetHeight ( Photo )"))
i = s.find('name="ModificationTimestamp"')
end = s.find('</Field>', i) + len('</Field>')
s = s[:end] + new + s[end:]
k = s.rfind('<key>13</key>', 0, i)
s = s[:k] + '<key>13</key><key>14</key><key>15</key><key>16</key><key>17</key><key>18</key>' + s[k + len('<key>13</key>'):]
open(dst, 'w', encoding='utf-8').write(s)
