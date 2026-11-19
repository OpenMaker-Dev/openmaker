# q07: container fields with data. File: MBS example "Build and send Email"
# (Attachment: Name, Type, Container (binary), Path, EmailID; 2 records.
# EMail: globals SMTP Server/Username/Password and text fields; 1 record.
# Recipient: Type, Name, Email, EmailID; 4 records). No record context.
SELECT ROWID, Name, Type, Path, EmailID FROM Attachment
SELECT Container FROM Attachment
SELECT * FROM Attachment
SELECT GETAS(Container, 'FILE') FROM Attachment
SELECT GETAS(Container, 'FNAM') FROM Attachment
SELECT GETAS(Container, 'JPEG') FROM Attachment
SELECT GETAS(Container, 'PNGf') FROM Attachment
SELECT GETAS(Container, 'GIFf') FROM Attachment
SELECT GETAS(Container, 'PDF ') FROM Attachment
SELECT GETAS(Container, 'snd ') FROM Attachment
SELECT GETAS(Container, 'SIZE') FROM Attachment
SELECT GETAS(Container, 'MAIN') FROM Attachment
SELECT GETAS(Container, 'JPEG') AS j FROM Attachment
SELECT GETAS(Container, DEFAULT) FROM Attachment
SELECT GETAS(Container, 'file') FROM Attachment
SELECT GETAS(Container, 'FIL') FROM Attachment
SELECT CAST(Container AS VARCHAR) FROM Attachment
SELECT STRVAL(Container) FROM Attachment
SELECT ROWID FROM Attachment WHERE Container IS NOT NULL
SELECT COUNT(Container), COUNT(*) FROM Attachment
SELECT Name, Container FROM Attachment ORDER BY Name
SELECT Container, Name FROM Attachment WHERE Name LIKE '%'
SELECT a.Name, e.Subject FROM Attachment a JOIN EMail e ON a.EmailID = e.EmailID
SELECT e.Subject, COUNT(r.ROWID) FROM EMail e LEFT JOIN Recipient r ON r.EmailID = e.EmailID GROUP BY e.Subject
SELECT Type, Name, Email FROM Recipient ORDER BY Type, Name
SELECT Type, COUNT(*) FROM Recipient GROUP BY Type
SELECT "SMTP Server", "SMTP Username" FROM EMail
SELECT "SMTP Server" FROM Recipient r, EMail e
SELECT COUNT(*) FROM Recipient r, EMail e WHERE e."SMTP Server" IS NULL
SELECT r.Name FROM Recipient r WHERE r.EmailID IN (SELECT EmailID FROM EMail)
SELECT LENGTH(PlainText), LENGTH(HTMLText) FROM EMail
SELECT LEFT(HTMLText, 60) FROM EMail
=ExecuteSQL ( "SELECT Container FROM Attachment" ; "|" ; "¶" )
=Length ( ExecuteSQL ( "SELECT Container FROM Attachment" ; "|" ; "¶" ) )
=GetAsText ( Attachment::Container )
=Length ( ExecuteSQL ( "SELECT GETAS(Container, 'FILE') FROM Attachment" ; "|" ; "¶" ) )
=ExecuteSQL ( "SELECT Name FROM Attachment WHERE Container = ?" ; "|" ; "¶" ; "x" )
=ExecuteSQLe ( "SELECT Name FROM Attachment WHERE Name = ?" ; "|" ; "¶" ; Attachment::Container )
@ 03d00001 1
=Get ( RecordID ) & " " & Attachment::Name
=GetAsText ( Attachment::Container )
=ExecuteSQL ( "SELECT Container FROM Attachment" ; "|" ; "¶" )
=ExecuteSQLe ( "SELECT Name FROM Attachment WHERE Container = ?" ; "|" ; "¶" ; Attachment::Container )
