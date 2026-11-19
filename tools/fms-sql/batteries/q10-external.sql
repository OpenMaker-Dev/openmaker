# q10: occurrences of another file and calculated fields. File: MBS example
# "Event Management with Audit": AuditLog is an occurrence of a table in the
# file Event_Management_AuditLog, which is not open; the other occurrences
# hold one record each. No record context.
=ExecuteSQL ( "SELECT TableName, BaseTableName, BaseFileName FROM FileMaker_Tables" ; "|" ; "¶" )
=ExecuteSQL ( "SELECT FieldName, FieldType FROM FileMaker_Fields WHERE TableName = 'AuditLog'" ; "|" ; "¶" )
SELECT COUNT(*) FROM AuditLog
SELECT * FROM AuditLog
SELECT Taskz FROM AuditLog
SELECT * FROM Events
SELECT Event, "Date", "Task Number | All", "Guest Number | All", "Time Frame" FROM Events
SELECT * FROM Tasks
SELECT * FROM "Venue Looked Up"
SELECT COUNT(*) FROM "Guests | Contacts"
SELECT * FROM Contacts
SELECT "Sort Selection", "Sort List Key", "Address Short", Initial FROM Contacts
SELECT e.Event, t.Task FROM Events e JOIN Tasks t ON t."EVENT ID MATCH FIELD" = e."EVENT ID MATCH FIELD"
SELECT COUNT(*) FROM Events e, Tasks t, Guests g, Contacts c, Contributors x
