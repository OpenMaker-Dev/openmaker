# q11: ? parameters bound from ExecuteSQL's arguments. File: FileMaker Server
# sample (en), no record context. Arguments are calculation expressions.
# --- binding by type
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; 3
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "3"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "3abc"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; "abc"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; 3.0
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; ""
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; Date ( 1 ; 3 ; 0001 )
SELECT Task FROM Tasks WHERE Task = ? ;; "Checklist"
SELECT Task FROM Tasks WHERE Task = ? ;; "checklist"
SELECT Task FROM Tasks WHERE Task = ? ;; 3
SELECT Task FROM Tasks WHERE Task LIKE ? ;; "Check%"
SELECT Task FROM Tasks WHERE Task LIKE ? || '%' ;; "Check"
SELECT Task FROM Tasks WHERE Task LIKE '%' || ? || '%' ;; "Art"
SELECT Task FROM Tasks WHERE "Due Date" = ? ;; Date ( 11 ; 24 ; 2014 )
SELECT Task FROM Tasks WHERE "Due Date" = ? ;; "11/24/2014"
SELECT Task FROM Tasks WHERE "Due Date" = ? ;; "2014-11-24"
SELECT Task FROM Tasks WHERE "Due Date" = ? ;; 735561
SELECT Task FROM Tasks WHERE "Due Date" = ? ;; Timestamp ( Date ( 11 ; 24 ; 2014 ) ; 0 )
SELECT Task FROM Tasks WHERE "Due Date" > ? ;; Date ( 9 ; 12 ; 2015 )
SELECT Task FROM Tasks WHERE "Due Date" BETWEEN ? AND ? ;; Date ( 1 ; 1 ; 2014 ) ;; Date ( 12 ; 31 ; 2014 )
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" IN (?, ?, ?) ;; 1 ;; 2 ;; 3
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? OR "TASK ID MATCH FIELD" = ? ;; 1 ;; 2
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? OR "TASK ID MATCH FIELD" = ? ;; 2 ;; 1
# --- parameters as values
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "x"
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 12.50
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Date ( 2 ; 17 ; 2014 )
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Time ( 13 ; 45 ; 10 )
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Timestamp ( Date ( 2 ; 17 ; 2014 ) ; Time ( 13 ; 45 ; 10 ) )
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; ""
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1/3
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 10^20
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "a¶b"
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; TextColor ( "red" ; RGB ( 255 ; 0 ; 0 ) )
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1 = 1
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1/0
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; GetAsNumber ( "12abc" )
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "12abc" + 0
SELECT ?, ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1 ;; "a"
SELECT ? + 1 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 2
SELECT ? + 1 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "2"
SELECT ? || 'x' FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 2
SELECT ? + ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1 ;; 2
SELECT ? + ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "a" ;; "b"
SELECT UPPER(?) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "abc"
SELECT UPPER(?) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 12
SELECT ABS(?) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; -2
SELECT ABS(?) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "-2"
SELECT LENGTH(?) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "abc"
SELECT CAST(? AS INT) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "12"
SELECT CAST(? AS DATE) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "2014-02-17"
SELECT YEAR(?) FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Date ( 2 ; 17 ; 2014 )
SELECT ? - 1 FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; Date ( 2 ; 17 ; 2014 )
SELECT COALESCE(?, 'none') FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; ""
SELECT CASE WHEN ? = 1 THEN 'one' ELSE 'other' END FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1
SELECT Task FROM Tasks WHERE ? = ? ;; 1 ;; 1
SELECT Task FROM Tasks WHERE ? IS NULL ;; ""
SELECT Task FROM Tasks WHERE ? IS NOT NULL ;; "x"
# --- count of arguments
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ?
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? OR "TASK ID MATCH FIELD" = ? ;; 1
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; 1 ;; 2 ;; 3
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; 1
SELECT ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
# --- named parameters
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = :id ;; 3
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = :id
SELECT :x FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "a"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = :id AND Task = ? ;; 3 ;; "x"
=Let ( $id = 3 ; ExecuteSQLe ( "SELECT Task FROM Tasks WHERE \"TASK ID MATCH FIELD\" = :id" ; "|" ; "¶" ) )
=Let ( $$id = 3 ; ExecuteSQLe ( "SELECT Task FROM Tasks WHERE \"TASK ID MATCH FIELD\" = :id" ; "|" ; "¶" ) )
=Let ( $$id = 3 ; ExecuteSQLe ( "SELECT Task FROM Tasks WHERE \"TASK ID MATCH FIELD\" = :\"$$id\"" ; "|" ; "¶" ) )
# --- parameters elsewhere
SELECT Task FROM Tasks ORDER BY ? ;; 1
SELECT "TASK ID MATCH FIELD" FROM Tasks FETCH FIRST ? ROWS ONLY ;; 2
SELECT "TASK ID MATCH FIELD" FROM Tasks OFFSET ? ROWS ;; 10
SELECT Task FROM ? ;; "Tasks"
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = ? ;; List ( 3 ; 4 )
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" IN (?) ;; "3,4"
SELECT Task FROM Tasks WHERE Task = '?' ;; "Checklist"
SELECT Task FROM Tasks WHERE Task = "?" ;; "Checklist"
SELECT Task FROM Tasks WHERE Task = ?? ;; "Checklist"
SELECT '?', ? FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 ;; "x"
=ExecuteSQL ( "SELECT Task FROM Tasks WHERE \"TASK ID MATCH FIELD\" = ?" ; "|" ; "¶" ; "" ; 3 )
=ExecuteSQL ( "SELECT ? FROM Tasks WHERE \"TASK ID MATCH FIELD\" = 3" ; "|" ; "¶" ; Tasks::Task )
=ExecuteSQL ( "SELECT ? FROM Tasks WHERE \"TASK ID MATCH FIELD\" = 3" ; "|" ; "¶" ; Tasks::QuickFind )
=ExecuteSQL ( "SELECT ? FROM Tasks WHERE \"TASK ID MATCH FIELD\" = 3" ; "|" ; "¶" ; Get ( ApplicationVersion ) )
