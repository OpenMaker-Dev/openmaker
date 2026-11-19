# q08: repeating fields. File: MBS example AppleTouchBar (ATB: 4 records;
# flex_Function, flex_Script, flex_Icon, flex_Label varchar[9];
# _default varchar[12]; _flex_Add calculated varchar[9]; TouchBar calculated
# binary[12]). No record context.
=ExecuteSQL ( "SELECT TableName, FieldName, FieldType, FieldClass, FieldReps FROM FileMaker_Fields WHERE TableName = 'ATB'" ; "|" ; "¶" )
SELECT ROWID, flex_Label FROM ATB
SELECT ROWID, flex_Label[1], flex_Label[2], flex_Label[3], flex_Label[9] FROM ATB
SELECT ROWID, _default[1], _default[2], _default[3], _default[12] FROM ATB
SELECT flex_Label[0] FROM ATB
SELECT flex_Label[10] FROM ATB
SELECT flex_Label[-1] FROM ATB
SELECT flex_Label[1.5] FROM ATB
SELECT flex_Label[1+1] FROM ATB
SELECT flex_Label['2'] FROM ATB
SELECT flex_Label[?] FROM ATB ;; 2
SELECT flex_Label[ROWID] FROM ATB
SELECT ATB.flex_Label[2] FROM ATB
SELECT a.flex_Label[2] FROM ATB a
SELECT "flex_Label"[2] FROM ATB
SELECT flex_Label [2] FROM ATB
SELECT ROWID FROM ATB WHERE flex_Label[2] IS NOT NULL
SELECT ROWID FROM ATB WHERE flex_Label = 'x' OR flex_Label[2] LIKE '%'
SELECT COUNT(flex_Label), COUNT(flex_Label[2]), MAX(flex_Label[2]) FROM ATB
SELECT DISTINCT flex_Label[2] FROM ATB
SELECT flex_Label[2], COUNT(*) FROM ATB GROUP BY flex_Label[2]
SELECT ROWID FROM ATB ORDER BY flex_Label[2], ROWID
SELECT * FROM ATB
SELECT _flex_Add, _flex_Add[2], _flex_Add[9] FROM ATB
SELECT TouchBar, TouchBar[2] FROM ATB
SELECT ROWID, LENGTH(_default[1]) FROM ATB
SELECT COUNT(*) FROM ATB_Icons
SELECT * FROM ATB_Icons
SELECT COUNT(*) FROM ATB_NSImageTemplates
SELECT * FROM ATB_NSImageTemplates FETCH FIRST 5 ROWS ONLY
SELECT ROWID, ROWMODID FROM ATB
