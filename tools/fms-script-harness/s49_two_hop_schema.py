# Battery s49 (#41): contacts-en's saveAsXML with a second hop. Phone
# Emails, an occurrence of Email Addresses, related from Phone Numbers by
# PrimaryKey = ForeignKey, creation allowed on the Phone Emails side:
# Contacts -> Phone Numbers -> Phone Emails, both hops allowing creation.
#   python3 -I tools/fms-script-harness/s49_two_hop_schema.py "$WORK/contacts-en.xml" s49.xml
#   SCHEMA=s49.xml tools/fms-script-harness/runbat.sh tools/fms-script-harness/batteries/s49-two-hops-both.txt contacts-en.fmp12
import sys
src, dst = sys.argv[1], sys.argv[2]
s = open(src, encoding='utf-8').read()
to = ('<TableOccurrence View="Related" height="74" id="1065094" name="Phone Emails" type="Local">'
      '<BaseTableSourceReference type="BaseTableReference">'
      '<BaseTableReference id="133" name="Email Addresses" UUID="56509246-7D8F-44AB-B4B1-013334F98282"></BaseTableReference>'
      '</BaseTableSourceReference><CoordRect top="260" left="400" bottom="378" right="531"></CoordRect>'
      '<Color red="120" green="120" blue="120" alpha="1.00"></Color><TagList></TagList></TableOccurrence>')
phone = '<TableOccurrenceReference id="1065092" name="Phone Numbers" UUID="CDAD3858-E47E-4442-8723-A98EEBD00A15"></TableOccurrenceReference>'
emails = '<TableOccurrenceReference id="1065094" name="Phone Emails"></TableOccurrenceReference>'
rel = ('<Relationship id="4"><LeftTable cascadeCreate="False" cascadeDelete="False" type="Local">' + phone + '</LeftTable>'
       '<RightTable cascadeCreate="True" cascadeDelete="False" type="Local">' + emails + '</RightTable>'
       '<JoinPredicateList membercount="1"><JoinPredicate type="Equal">'
       '<LeftField><FieldReference id="6" name="PrimaryKey">' + phone + '</FieldReference></LeftField>'
       '<RightField><FieldReference id="5" name="ForeignKey">' + emails + '</FieldReference></RightField>'
       '</JoinPredicate></JoinPredicateList><TagList></TagList></Relationship>')
end = s.find('</TableOccurrenceCatalog>')
s = s[:end] + to + s[end:]
s = s.replace('<TableOccurrenceCatalog membercount="4">', '<TableOccurrenceCatalog membercount="5">', 1)
k = s.find('<key>1065093</key>')
s = s[:k] + '<key>1065093</key><key>1065094</key>' + s[k + len('<key>1065093</key>'):]
end = s.find('</RelationshipCatalog>')
s = s[:end] + rel + s[end:]
s = s.replace('<RelationshipCatalog membercount="3">', '<RelationshipCatalog membercount="4">', 1)
open(dst, 'w', encoding='utf-8').write(s)
