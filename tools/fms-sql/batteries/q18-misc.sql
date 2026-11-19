# q18: lexer errors against parser errors, text arguments against number
# columns. File: FileMaker Server sample (en), no record context.
SELEC Task FROM Tasks _
SELECT Task FROM Tasks WHERE _x = 1
SELECT Task FROM Taskz WHERE Task = 'ä' _
SELECT Task, FROM Tasks _
=GetAsNumber ( "3¶4" ) & "|" & GetAsNumber ( "3abc" ) & "|" & GetAsNumber ( "a3b4" ) & "|" & GetAsNumber ( "1,234.5" )
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "3¶4"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "a3b4"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "34"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "1,2"
SELECT ? + 0 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "3¶4"
SELECT ? + 0 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "a3b4"
SELECT ? * 1 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Date ( 2 ; 17 ; 2014 )
SELECT ? + 0 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Time ( 1 ; 0 ; 0 )
SELECT 'a' || 1, 'a' || DATE '2014-02-17', 'a' || TIME '13:45:10' FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT 1 + NULL FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT NULL + 1 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT 'a' - 'b', 'a' + 1 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT 'ab' / 'a' FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT DATE '2014-02-17' + 'x' FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT DATE '2014-02-17' || 'x' FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT Task || "TASK ID MATCH FIELD" FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
SELECT Task || "Due Date" FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
