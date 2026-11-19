# Add a self-join and an auto-entered witness over it to contacts-en's
# saveAsXML document, for script oracle s47-self-join-ae: what a record's
# auto-entry reads through a self-join it belongs to. The occurrence Mates
# (Contacts) is joined on Contacts::Company = Mates::Company; Contacts::SJ
# (field 14, text) auto-enters, replacing, a witness of the record's own
# Company and First Name and of Mates, and appends each evaluation to $$ae.
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src, encoding='utf-8').read()

contacts = ('<TableOccurrenceReference id="1065090" name="Contacts" '
            'UUID="06EEB6C5-DA8C-475E-A69F-EA5B69C6A96D"></TableOccurrenceReference>')

# The occurrence, last in the graph.
to = ('<TableOccurrence View="Related" height="110" id="1065094" name="Mates" type="Local">'
      '<BaseTableSourceReference type="BaseTableReference">'
      '<BaseTableReference id="130" name="Contacts" UUID="ACCBC2A6-10D2-43EF-8C84-E1402E94E9A7">'
      '</BaseTableReference></BaseTableSourceReference>'
      '<CoordRect top="280" left="20" bottom="398" right="151"></CoordRect>'
      '<Color red="120" green="120" blue="120" alpha="1.00"></Color><TagList></TagList>'
      '</TableOccurrence>')
s = s.replace('<TableOccurrenceCatalog membercount="4">', '<TableOccurrenceCatalog membercount="5">', 1)
s = s.replace('<key>1065093</key>', '<key>1065093</key><key>1065094</key>', 1)
end = s.index('</TableOccurrenceCatalog>')
s = s[:end] + to + s[end:]

# Contacts::Company = Mates::Company.
def company(occurrence, oid):
    return (f'<FieldReference id="5" name="Company">'
            f'<TableOccurrenceReference id="{oid}" name="{occurrence}"></TableOccurrenceReference>'
            '</FieldReference>')
rel = ('<Relationship id="4">'
       f'<LeftTable cascadeCreate="False" cascadeDelete="False" type="Local">{contacts}</LeftTable>'
       '<RightTable cascadeCreate="False" cascadeDelete="False" type="Local">'
       '<TableOccurrenceReference id="1065094" name="Mates"></TableOccurrenceReference></RightTable>'
       '<JoinPredicateList membercount="1"><JoinPredicate type="Equal">'
       f'<LeftField>{company("Contacts", 1065090)}</LeftField>'
       f'<RightField>{company("Mates", 1065094)}</RightField>'
       '</JoinPredicate></JoinPredicateList><TagList></TagList></Relationship>')
s = s.replace('<RelationshipCatalog membercount="3">', '<RelationshipCatalog membercount="4">', 1)
end = s.index('</RelationshipCatalog>')
s = s[:end] + rel + s[end:]

# The witness.
witness = ('Let ( [ w = Company & "|" & First Name & "|" & Count ( Mates::PrimaryKey ) & "|" & '
           'Substitute ( List ( Mates::First Name ) ; ¶ ; "," ) & "|" & Mates::First Name ; '
           '$$ae = $$ae & "[" & w & "]" ] ; w )')
field = ('<Field id="14" name="SJ" fieldtype="Normal" datatype="Text" comment="">'
         '<AutoEnter type="Calculated" prohibitModification="False" overwriteExisting="True" alwaysEvaluate="False">'
         f'<Calculated enable="True"><Calculation>{contacts}<Text><![CDATA[{witness}]]></Text></Calculation></Calculated></AutoEnter>'
         '<Validation alwaysValidate="False" type="OnlyDuringDataEntry" allowOverride="True" notEmpty="False" unique="False" existing="False"></Validation>'
         '<Storage autoIndex="True" index="None" global="False" maxRepetitions="1">'
         '<LanguageReference name="English" id="21"></LanguageReference></Storage>'
         '<TagList></TagList><Annotation></Annotation><DisplayNames enable="False"></DisplayNames></Field>')
i = s.find('name="ModificationTimestamp"')
end = s.find('</Field>', i) + len('</Field>')
s = s[:end] + field + s[end:]
k = s.rfind('<key>13</key>', 0, i)
s = s[:k] + '<key>13</key><key>14</key>' + s[k + len('<key>13</key>'):]
open(dst, 'w', encoding='utf-8').write(s)
