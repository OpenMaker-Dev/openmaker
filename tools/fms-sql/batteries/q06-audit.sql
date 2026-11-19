# q06: field types in the MBS example "Audit" (Audit: SomeText, SomeNumber,
# SomeDate, SomeTime, SomeTimeStamp, SomeContainer (binary), AuditTimeStamp,
# AuditState; 3 records. AuditLog: 18 text fields; 26 records). No record context.
SELECT * FROM Audit
SELECT ROWID, SomeText, SomeNumber, SomeDate, SomeTime, SomeTimeStamp FROM Audit
SELECT SomeContainer FROM Audit
SELECT LENGTH(SomeContainer) FROM Audit
SELECT SomeContainer IS NULL FROM Audit
SELECT ROWID FROM Audit WHERE SomeContainer IS NULL
SELECT ROWID FROM Audit WHERE SomeContainer IS NOT NULL
SELECT CAST(SomeContainer AS VARCHAR) FROM Audit
SELECT STRVAL(SomeContainer) FROM Audit
SELECT GETAS(SomeContainer, 'FILE') FROM Audit
SELECT GETAS(SomeContainer, 'JPEG') FROM Audit
SELECT GETAS(SomeContainer, 'PNGf') FROM Audit
SELECT GETAS(SomeContainer, 'SND ') FROM Audit
SELECT GETAS(SomeContainer, DEFAULT) FROM Audit
SELECT GETAS(SomeText, 'FILE') FROM Audit
SELECT GETAS(SomeContainer, 'XX') FROM Audit
SELECT COUNT(SomeContainer) FROM Audit
SELECT MAX(SomeContainer) FROM Audit
SELECT DISTINCT SomeContainer FROM Audit
SELECT SomeContainer FROM Audit UNION SELECT SomeContainer FROM Audit
SELECT SomeContainer FROM Audit UNION ALL SELECT SomeContainer FROM Audit
SELECT ROWID FROM Audit ORDER BY SomeContainer
SELECT ROWID FROM Audit WHERE SomeContainer = 'x'
SELECT ROWID FROM Audit WHERE SomeContainer LIKE '%'
SELECT SomeTime, SomeTime + 1, HOUR(SomeTime), CAST(SomeTime AS VARCHAR), SomeTime - TIME '00:00:00' FROM Audit
SELECT SomeTimeStamp, SomeTimeStamp + 1, CAST(SomeTimeStamp AS DATE), CAST(SomeTimeStamp AS TIME), YEAR(SomeTimeStamp) FROM Audit
SELECT SomeNumber, SomeNumber * 2, SomeNumber / 3, -SomeNumber, ROUND(SomeNumber, 0) FROM Audit
SELECT SomeDate, SomeDate + 1, MONTHNAME(SomeDate), DAYNAME(SomeDate) FROM Audit
SELECT AuditTimeStamp, AuditState FROM Audit
SELECT ROWID FROM Audit WHERE SomeTime > TIME '12:00:00'
SELECT ROWID FROM Audit WHERE SomeTime = '1:00:00 PM'
SELECT ROWID FROM Audit WHERE SomeTimeStamp > TIMESTAMP '2000-01-01 00:00:00'
SELECT ROWID FROM Audit WHERE SomeTimeStamp > '1/1/2000 12:00:00 AM'
SELECT ROWID FROM Audit WHERE SomeTimeStamp > DATE '2000-01-01'
SELECT MIN(SomeTime), MAX(SomeTimeStamp), SUM(SomeNumber), AVG(SomeNumber), COUNT(SomeTimeStamp) FROM Audit
SELECT SUM(SomeTime) FROM Audit
SELECT ROWID, SomeText FROM Audit ORDER BY SomeText
SELECT ROWID FROM Audit ORDER BY SomeTime DESC
SELECT ROWID FROM Audit ORDER BY SomeTimeStamp
SELECT ROWID FROM Audit ORDER BY SomeNumber
SELECT * FROM AuditLog FETCH FIRST 3 ROWS ONLY
SELECT FieldName, COUNT(*) FROM AuditLog GROUP BY FieldName
SELECT Action, COUNT(*) FROM AuditLog GROUP BY Action
SELECT DISTINCT FieldType FROM AuditLog
SELECT FieldValue FROM AuditLog WHERE FieldType = 'Number'
SELECT FieldValue FROM AuditLog ORDER BY FieldValue
SELECT TimeStamp FROM AuditLog ORDER BY TimeStamp DESC FETCH FIRST 5 ROWS ONLY
SELECT a.SomeText, COUNT(l.ROWID) FROM Audit a LEFT JOIN AuditLog l ON l.RecordID = STRVAL(a.ROWID) GROUP BY a.SomeText
SELECT a.ROWID, l.Action FROM Audit a JOIN AuditLog l ON l.RecordID = CAST(a.ROWID AS VARCHAR) ORDER BY 1, 2
=ExecuteSQL ( "SELECT SomeContainer FROM Audit" ; "|" ; "¶" )
=GetAsText ( ExecuteSQL ( "SELECT SomeContainer FROM Audit" ; "|" ; "¶" ) )
=ExecuteSQL ( "SELECT ROWID, SomeTime, SomeNumber FROM Audit WHERE SomeTime = ?" ; "|" ; "¶" ; Time ( 13 ; 0 ; 0 ) )
