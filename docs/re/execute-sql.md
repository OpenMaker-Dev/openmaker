# ExecuteSQL: FileMaker's SQL engine (FQL)

What `ExecuteSQL`, the undocumented `ExecuteSQLe` and the plug-in API's
`ExecuteFileSQL` accept, how they resolve names and types, what they compute
and how they format the result, in enough detail to build the engine over
OpenMaker's tables, fields and indexes and match FileMaker's text byte for
byte. The engine is FileMaker's "FQL" (`FQL::` namespace in
`libDBEngine.so`; architecture.md 4, SQL engine row).

## 0. Evidence and how to read it

Markers, as in calc-values.md:

- **[O-run]** a run of FileMaker Server 26.0.3.309's own engine in the
  `tools/fms-calc-harness` container (`DBExpressionEnv::Evaluate` of an
  `ExecuteSQL`/`ExecuteSQLe` calculation on a copy of a sample file). These
  are the 1,633 rows of `catalogs/execute-sql-vectors.tsv`, from the
  batteries in `tools/fms-sql/batteries` (results in `tools/fms-sql/results`).
  Battery names (`q04` ...) are cited below.
- **[O-code]** symbols, DWARF names and data in `libDBEngine.so`, or values
  returned by exported functions called in the same container
  (`tools/fms-sql/fqlgrammar.cpp`: the grammar, the function table, the
  message tables, the locale).
- **[I]** inference.

The grammar of section 3 was recovered from FileMaker's ANTLR 4.13.1
recognizers: `FQLSQLLexer`, `FQLSQLSemanticAnalyzer` (and an unused
`FQLSQLParser`) export `getRuleNames()`, `getVocabulary()` and
`getSerializedATN()`; `tools/fms-sql/atn_grammar.py` deserializes each ATN
(format version 4 of the public ANTLR runtime) and walks every rule's state
graph back into EBNF (`tools/fms-sql/results/fql-grammar.txt` is the machine
output) [O-code]. The statement forms were also run through `ExecuteSQL`
[O-run].

Sample files (`tools/fms-sql/README.md`): the FileMaker Server sample
`en_FMServer_Sample.fmp12` (Tasks: 12 records, IDs and dates in q04's header;
"Related Tasks" is a second occurrence of Tasks; Assignees and Attachments
are empty), the Contacts template in en/de/sv/ja (no records; queried
through the system tables), and five MBS plug-in examples with dates, times,
timestamps, containers, repeating fields, a summary field and an occurrence
of a missing file.

## 1. The calculation functions

### 1.1 Signature and arguments [O-run]

`ExecuteSQL ( sqlQuery ; fieldSeparator ; rowSeparator { ; arguments... } )`,
and `ExecuteSQLe` with the same parameters (calc-functions.csv, group 140).
Fewer than three parameters is the compile error 1201 and an empty trailing
parameter (`; )`) 1204, as for any function (q02).

- `sqlQuery` is evaluated and its **text** is the statement (`42` is the
  statement `42`, a syntax error; style runs are ignored).
- `fieldSeparator`, `rowSeparator`: the text of the values (`1` gives `1`,
  `Date ( 1 ; 2 ; 2003 )` gives `1/2/2003` in an en-US file, `1/0` gives `?`
  plus calculation error 15 while the query still runs); any length and
  characters (`¶¶`, `😀`, `Char ( 9 )`). An **empty** separator means `,`
  between fields and CR (`¶`) between rows. Styles of the separators are
  dropped: the result is plain text (`GetAsSVG` shows one unstyled run).
- `arguments` bind in order to the `?` parameters of the statement
  (5.7). Extra arguments are ignored; missing ones bind NULL.

### 1.2 Result [O-run]

- Success: a text value holding the rows, each row its column texts (8.2)
  joined by `fieldSeparator`, rows joined by `rowSeparator`, no separator
  after the last row. No rows: the empty value (cached type 0, `IsEmpty` =
  1, error 0). A row of NULLs keeps its separators (`|` for two NULL
  columns).
- Error, `ExecuteSQL`: the calculation error value `?` (like any
  calculation error: number type, `GetAsNumber` gives `?`, `& "|"` gives
  `?|`, `IsValid` gives 0) and an error code (`EvaluationError` = 8309,
  8310, 3, 14, ...; 9.2).
- Error, `ExecuteSQLe`: the **text** `? ERROR: ` followed by the error
  description (9.1), *and* the same error code (`EvaluationError (
  ExecuteSQLe (...) )` = 8309; the text flows on, `Length` of it is 105 with
  error 8309) (q17).
- On success the two functions return the same text (q14).

`FQL::kSQLErrorStr` = `ERROR: ` and `FQL::kSQLeErrorStr` = `? ERROR: ` are
UTF-16 constants at `0x2e5fa70` and `0x2e5fa80` [O-code].

### 1.3 Context [O-run]

The result does not depend on the calculation's table occurrence, record or
layout: the same statements give the same text with no context, on a record
of Tasks and in an occurrence without a record (q14). Names inside the SQL
never refer to the calculation context (`Tasks."TASK ID MATCH FIELD"`
inside the statement is the SQL table's column). Context values reach the
query only as arguments (`?` bound to `Get ( RecordID )` or a field).

### 1.4 Entry points and the plug-in API [O-code]

`FQL::ExecuteSQLCalc(DBFileUser const&, unistring const& sql, unistring
const& fieldSep, unistring const& rowSep, args begin, args end, DBData&
result, unsigned short, bool)` serves both calculation functions (the
`bool` selects the `ExecuteSQLe` text [I]) and calls the file-local
`FQL::ExecuteSQL(DBFileUser const&, unistring const&, Parameters&,
SQLCursors&, unistring const&, unistring const&, bool, DBData&, bool)`.
The plug-in API's `FM_ExprEnv_ExecuteFileSQL` / `ExecuteFileSQLTextResult`
use `FQL::ExecuteFileSQL(DBExpressionEnv const*, Text const& sql, Text
const& fileName, DBDataVect const& params, DBRowVect& result)` and
`FQL::ExecuteFileSQLTextResult(..., DBData& result, char16_t colSep,
char16_t rowSep)`: the same parser and executor, a file found by name
(`DBSession::FindFileUser`), one-unit separators, and a `DBRowVect` of typed
values for the non-text form. Not run (no plug-in host in the harness).

## 2. Processing pipeline [O-code]

1. `FQL::FQLParserDriver(unistring const& sql, DBFileUser const&,
   Parameters*, SQLCursors*)`, then `ParseQuery(bool)`.
2. `CreateTokenStream`: the statement is converted to UTF-8
   (`unistring::ConvertToStdString`), wrapped in `antlr4::ANTLRInputStream`
   (code points), and lexed by `FQLSQLLexer` (ANTLR 4.13.1,
   `lib/libantlr4-runtime.so.4.13.1`) through a `CommonTokenStream`.
3. `FQLSQLSemanticAnalyzer` (an ANTLR parser) parses rule `statement` with
   FileMaker's listener (`FQL::FQLCustomErrorListener`); the first syntax
   error becomes FQL0001 (error 8310) with its position.
4. `FQLSQLSemanticAnalyzerCustomVisitor` walks the tree and calls the
   driver's builders (`AddTable`, `CreateColumnRef`, `CreateExpression`,
   `CreateFunction`, `CreateCastExpression`, `AddOrderSpec`, `AddFetchCount`,
   `ValidateUnion`, ...), which resolve names and types and raise the other
   FQLnnnn errors. Expressions become calculation trees (`FQL::Expr` holds
   `Draco::DBCalc` operands; `Expr::GetCalcOp`).
5. `FQL::Query::Execute(DBTransaction&, Task&)` runs the query (10.1) and
   `FQL::ExecuteSQL` formats the rows.

`FQLSQLParser` is a second, older generated parser of a similar grammar;
its only caller is its own delegating constructor (`0x13fae76`), so it is
dead code (its `statement` is `ddl_statement* EOF | dml_statement* EOF`,
it has no `BOOLEAN` type).

## 3. Grammar

### 3.1 Lexical structure [O-code, O-run]

The lexer sees the statement's code points.

- **White space** (skipped): TAB, LF, CR, SPACE only. NBSP, VT, FF, U+2028
  and any other character that starts no token are lexer errors (FQL0001)
  (q01).
- **Comments** (skipped): `/* ... */`, shortest match, not nested (an
  unterminated `/*` lexes as `/` `*`, a syntax error); and `// ...` **up to
  and including a LF** (`'//' ~[\r\n]* '\r'? '\n'`). A `//` that no LF
  ends is two `/` tokens (a syntax error), also at the end of the text and
  when only CR follows (q01, q02). `--` is not a comment: `3 --1` is
  `3 - (-1)`.
- **Keywords**: 265 words, ASCII letters matched case-insensitively (each
  letter `[Xx]`; `END-EXEC` has a hyphen). Every keyword is **reserved**: an
  unquoted name spelled like one is the keyword and a syntax error where a
  name is expected (`Date`, `Value`, `Size` as aliases; `Action`,
  `TimeStamp`, `Cross` as field names; q01, q06, q09). The list:

  ABSOLUTE ACTION ADD ALL ALLOCATE ALTER AND ANY ARE AS ASC ASSERTION AT
  AUTHORIZATION AVG BEGIN BETWEEN BINARY BIT BIT_LENGTH BLOB BOOLEAN BOTH BY
  CASCADE CASCADED CASE CAST CATALOG CHAR CHARACTER CHAR_LENGTH
  CHARACTER_LENGTH CHR CHECK CLOSE COALESCE COLLATE COLLATION COLUMN COMMIT
  CONNECT CONNECTION CONSTRAINT CONSTRAINTS CONTINUE CONVERT CORRESPONDING
  COUNT CREATE CROSS CURDATE CURRENT CURRENT_DATE CURRENT_TIME
  CURRENT_TIMESTAMP CURRENT_USER CURSOR CURTIME CURTIMESTAMP DATE DATEVAL DAY
  DAYNAME DAYOFWEEK DEALLOCATE DEC DECIMAL DECLARE DEFAULT DEFERRABLE DEFERRED
  DELETE DESC DESCRIBE DESCRIPTOR DIAGNOSTICS DISCONNECT DISTINCT DOMAIN
  DOUBLE DROP ELSE END END-EXEC ESCAPE EVERY EXCEPT EXCEPTION EXEC EXECUTE
  EXISTS EXTERNAL EXTRACT FALSE FETCH FIRST FLOAT FOR FOREIGN FOUND FROM FULL
  GET GETAS GLOBAL GO GOTO GRANT GROUP HAVING HOUR IDENTITY IMMEDIATE IN INDEX
  INDICATOR INITIALLY INNER INPUT INSENSITIVE INSERT INT INTEGER INTERSECT
  INTERVAL INTO IS ISOLATION JOIN KEY LANGUAGE LAST LEADING LEFT LENGTH
  FEWER_FOLDERS LEVEL LIKE LOCAL LONGVARBINARY LOWER LTRIM MATCH MAX MIN MINUTE
  MODULE MONTH MONTHNAME NAMES NATURAL NCHAR NEXT NO NOT NULL NULLIF NUMERIC
  NUMVAL OCTET_LENGTH OF OFFSET ON ONLY OPEN OPTION OR ORDER OUTER OUTPUT
  OVERLAPS PAD PART PARTIAL PERCENT POSITION PRECISION PREPARE PRESERVE
  PRIMARY PRIOR PRIVILEGES PROCEDURE PUBLIC PUTAS RANDOM READ REAL REFERENCES
  RELATIVE RENAME RESTRICT REVOKE RIGHT ROLLBACK ROUND ROW ROWS ROWID RTRIM
  SCHEMA SCROLL SECOND SECTION SECURE SELECT SESSION SESSION_USER SET SIZE
  SMALLINT SOME SPACE SQL SQLCODE SQLERROR SQLSTATE STRVAL SUBSTR SUBSTRING SUM
  SYSTEM_USER TABLE TEMPORARY THEN TIES TIME TIMESTAMP TIMESTAMPVAL TIMEVAL
  TIMEZONE_HOUR TIMEZONE_MINUTE TO TODAY TRAILING TRANSACTION TRANSLATE
  TRANSLATION TRIM TRUNCATE TRUE UNION UNIQUE UNKNOWN UPDATE UPPER USAGE USER
  USERNAME USING VALUE VALUES VARBINARY VARCHAR VARYING VIEW WHEN WHENEVER
  WHERE WITH WORK WRITE YEAR ZONE

  `ROWMODID` is not a keyword (it resolves as a column name, 4.4).
- **IDENTIFIER**: `[A-Za-z][A-Za-z0-9_]*` (ASCII only: a leading `_` or a
  non-ASCII letter is a lexer error, `_default`, `Täsk`), or a quoted name
  `"..."` in which `""` stands for one `"` and every other character is
  allowed (spaces, `|`, `ä`, CR). The quotes are removed; `""` is a valid
  token that never resolves. Exception: `"ROWID"` (the keyword in quotes, any
  case) is the token QUOTED_ROWID, not a name.
- **STRING_CONST**: `'...'` with `''` for one `'`; any characters including
  CR and LF; no backslash escapes, no `N'...'`; adjacent literals are not
  joined (syntax error).
- **UNSIGNED_CONST** `[0-9]+`; **FLOAT_CONST**
  `[0-9]*\.[0-9]+([eE][+-]?[0-9]+)?` or `[0-9]+[eE][+-]?[0-9]+`. `5.` is
  `5` then `.` (a syntax error); `.5` is valid. Both are exact decimals of
  any length (`12345678901234567890123`, `1e400`).
- **BIT_CONST** `[Bb]'[01]+'` and **HEX_CONST** `[Xx]'[0-9A-Fa-f]+'`: bit
  literals and odd-length hex literals fail when the value is built with
  FQL0045 "The value B'0101' is not a valid binary string."; even-length hex
  (`X'41'`) is accepted (its output was not observed) (q01).
- **Operators and punctuation**: `( ) { } [ ] . + - / * , ' " = < <= > >= ?
  || :`, NEQ `<>` or `!=`, RAISE `^` or `**`. No rule uses `{` `}`, so the
  ODBC escapes `{d '...'}` and `{ts '...'}` are syntax errors. `;` and `%`
  start no token (lexer errors; a trailing `;` is an error).

### 3.2 Parser grammar [O-code; statement forms O-run]

ANTLR notation; `'X'` is a keyword or punctuation token, upper-case words
are the token classes of 3.1. Rule names are FileMaker's, a few rules are
merged for reading. Rules unreachable from `statement` (`expression`,
`primary_expression`, `expression_list`, `identifier_list`,
`logical_binary_operator`, `non_logical_binary_operator`,
`unary_operator`) are left out.

```
statement               : ddl_statement EOF | dml_statement EOF ;

dml_statement           : query_statement | update_statement | delete_statement
                        | insert_statement | declare_cursor_statement
                        | deallocate_cursor_statement | open_cursor_statement
                        | close_cursor_statement | fetch_cursor_statement
                        | truncate_table_statement ;

query_statement         : query_expression order_by? result_offset? fetch_first?
                          ( 'FOR' 'UPDATE' ( 'OF' column_reference_list )? )? ;
query_expression        : query_expression_primary query_expression_union* ;
query_expression_union  : 'UNION' query_dup_opt? query_expression_primary ;
query_expression_primary: 'TABLE' IDENTIFIER | query_spec | '(' query_expression ')' ;
query_spec              : 'SELECT' query_dup_opt? select_list from_clause
                          where_clause? group_by_clause? having_clause? ;
query_dup_opt           : 'ALL' | 'DISTINCT' ;
select_list             : '*' | select_list_item ( ',' select_list_item )* ;
select_list_item        : IDENTIFIER '.' '*'
                        | 'GETAS' '(' value_expression ',' ( STRING_CONST | 'DEFAULT' ) ')'
                          ( 'AS'? IDENTIFIER )?
                        | value_expression ( 'AS'? IDENTIFIER )? ;

from_clause             : 'FROM' table_reference ( ',' table_reference )* ;
table_reference         : simple_table_reference table_join_reference* ;
simple_table_reference  : IDENTIFIER ( 'AS'? IDENTIFIER )? | '(' table_reference ')' ;
table_join_reference    : 'INNER'? 'JOIN' simple_table_reference 'ON' search_term
                        | 'LEFT' 'OUTER'? 'JOIN' simple_table_reference 'ON' search_term
                        | 'CROSS' 'JOIN' simple_table_reference ;

where_clause            : 'WHERE' search_term ;
group_by_clause         : 'GROUP' 'BY' column_reference_list ;
having_clause           : 'HAVING' search_term ;
column_reference_list   : column_reference ( ',' column_reference )* ;
order_by                : 'ORDER' 'BY' order_spec ( ',' order_spec )* ;
order_spec              : value_expression ( 'ASC' | 'DESC' )? ;
result_offset           : 'OFFSET' constant ( 'ROW' | 'ROWS' ) ;
fetch_first             : 'FETCH' ( 'FIRST' | 'NEXT' ) ( constant 'PERCENT'? )?
                          ( 'ROW' | 'ROWS' ) ( 'ONLY' | 'WITH' 'TIES' ) ;

search_term             : boolean_term ( 'OR' boolean_term )* ;
boolean_term            : boolean_factor ( 'AND' boolean_factor )* ;
boolean_factor          : 'NOT'? boolean_primary ;
boolean_primary         : value_expression comparison quantifier_op
                          '(' ( value_expression_list | subquery ) ')'
                        | value_expression comparison value_expression
                        | value_expression 'NOT'? 'IN' '(' ( value_expression_list | subquery ) ')'
                        | value_expression 'IS' 'NOT'? ( 'FALSE' | 'NULL' | 'TRUE' | 'UNKNOWN' )
                        | value_expression 'NOT'? 'BETWEEN' value_expression 'AND' value_expression
                        | value_expression 'NOT'? 'LIKE' value_expression
                        | value_expression 'NOT'? 'PART' 'OF' value_expression
                        | value_expression ( '+' | '-' ) 'INTERVAL' value_expression extract_operation
                        | ( 'EXISTS' | 'UNIQUE' ) subquery
                        | '(' search_term ')' ;
comparison              : NEQ | '=' | '<' | '<=' | '>' | '>=' ;
quantifier_op           : 'ALL' | 'ANY' | 'SOME' ;

value_expression        : value_expression ( '+' | '-' ) 'INTERVAL' value_term extract_operation
                        | value_term ( ( '+' | '-' | '||' ) value_term )* ;
value_term              : value_pow ( ( '/' | '*' ) value_pow )* ;
value_pow               : value_factor ( RAISE value_factor )* ;
value_factor            : ( '+' | '-' )? value_primary ;
value_primary           : column_reference | constant | case_expression | cast_expression
                        | aggregation_function | sql_function | '(' value_expression ')'
                        | subquery
                        | 'NULLIF' '(' value_expression ',' value_expression ')'
                        | 'COALESCE' '(' value_expression_list ')' ;
value_expression_list   : common_expression ( ',' common_expression )* ;
common_expression       : search_term | value_expression ;
subquery                : '(' query_expression ')' | query_expression ;

column_reference        : ( IDENTIFIER '.' )? ( 'ROWID' | QUOTED_ROWID )
                        | ( IDENTIFIER '.' )? IDENTIFIER ( '[' value_expression ']' )? ;
constant                : FLOAT_CONST | UNSIGNED_CONST | STRING_CONST | '?' | ':' IDENTIFIER
                        | 'DATE' STRING_CONST | 'TIME' STRING_CONST | 'TIMESTAMP' STRING_CONST
                        | BIT_CONST | HEX_CONST | 'DEFAULT' | 'NULL' | 'TRUE' | 'FALSE' ;

aggregation_function    : 'COUNT' '(' '*' ')'
                        | ( 'AVG' | 'COUNT' | 'EVERY' | 'MAX' | 'MIN' | 'SOME' | 'SUM' )
                          '(' ( 'ALL' | 'DISTINCT' )? value_expression ')' ;
sql_function            : 'EXTRACT' '(' extract_operation 'FROM' value_expression ')'
                        | 'TRIM' '(' trim_option value_expression? 'FROM' value_expression ')'
                        | 'TRIM' '(' value_expression 'FROM' value_expression ')'
                        | 'TRIM' '(' value_expression ')'
                        | function_name ( '(' value_expression_list ')' )?
                        | IDENTIFIER '(' value_expression_list? ')'
                        | ( 'DATE' | 'TIME' | 'TIMESTAMP' ) '(' ')' ;
extract_operation       : 'DAY' | 'HOUR' | 'MINUTE' | 'MONTH' | 'SECOND' | 'YEAR' ;
trim_option             : 'BOTH' | 'LEADING' | 'TRAILING' ;
function_name           : 'CHR' | 'CURDATE' | 'CURRENT_DATE' | 'CURRENT_TIME' | 'CURRENT_TIMESTAMP'
                        | 'CURRENT_USER' | 'CURTIME' | 'CURTIMESTAMP' | 'DATEVAL' | 'DAY'
                        | 'DAYNAME' | 'DAYOFWEEK' | 'HOUR' | 'INT' | 'LEFT' | 'LENGTH' | 'LOWER'
                        | 'LTRIM' | 'MAX' | 'MIN' | 'MINUTE' | 'MONTH' | 'MONTHNAME' | 'NUMVAL'
                        | 'PART' | 'RIGHT' | 'ROUND' | 'RTRIM' | 'SECOND' | 'SPACE' | 'STRVAL'
                        | 'SUBSTR' | 'SUBSTRING' | 'TIMESTAMPVAL' | 'TIMEVAL' | 'TODAY' | 'UPPER'
                        | 'USER' | 'USERNAME' | 'YEAR' ;
case_expression         : 'CASE' case_searched_when_then+ case_else 'END'
                        | 'CASE' value_expression case_simple_when_then+ case_else 'END' ;
case_simple_when_then   : 'WHEN' common_expression 'THEN' value_expression ;
case_searched_when_then : 'WHEN' search_term 'THEN' value_expression ;
case_else               : 'ELSE' ( value_expression | 'NULL' ) | /* empty */ ;
cast_expression         : 'CAST' '(' value_expression 'AS' data_type ')' ;
data_type               : 'INT'
                        | ( 'DECIMAL' | 'NUMERIC' ) ( '(' UNSIGNED_CONST ( ',' UNSIGNED_CONST )? ')' )?
                        | 'BOOLEAN' | 'DATE'
                        | 'TIME' ( '(' UNSIGNED_CONST ')' )?
                        | 'TIMESTAMP' ( '(' UNSIGNED_CONST ')' )?
                        | ( 'VARCHAR' | 'CHARACTER' 'VARYING' ) ( '(' UNSIGNED_CONST ')' )?
                        | ( 'BLOB' | 'VARBINARY' | 'LONGVARBINARY' | 'BINARY' 'VARYING' )
                          ( 'EXTERNAL' STRING_CONST ( 'SECURE' 'FEWER_FOLDERS'? | 'OPEN' STRING_CONST ) )? ;

update_statement        : 'UPDATE' IDENTIFIER 'SET' update_assignment ( ',' update_assignment )*
                          ( where_clause? | where_cursor? ) ;
where_cursor            : 'WHERE' 'CURRENT' 'OF' IDENTIFIER ;
update_assignment       : IDENTIFIER ( '[' value_expression ']' )? '='
                          ( value_expression
                          | 'PUTAS' '(' value_expression ',' STRING_CONST ')'
                          | value_expression 'AS' STRING_CONST ) ;
delete_statement        : 'DELETE' 'FROM' IDENTIFIER ( where_clause? | where_cursor ) ;
insert_statement        : 'INSERT' 'INTO' IDENTIFIER ( '(' insert_columns ')' )? insert_value_source
                        | 'INSERT' 'INTO' IDENTIFIER ( '(' insert_columns ')' )? query_expression order_by?
                        | 'INSERT' 'INTO' IDENTIFIER 'DEFAULT' 'VALUES' ;
insert_value_source     : 'VALUES' row_value_list_entry ( ',' row_value_list_entry )* ;
insert_columns          : insert_column_item ( ',' insert_column_item )* | /* empty */ ;
insert_column_item      : IDENTIFIER ( '[' UNSIGNED_CONST ']' )? ;
row_value_list_entry    : '(' row_value_element ( ',' row_value_element )* ')' ;
row_value_element       : 'PUTAS' '(' value_expression ',' STRING_CONST ')'
                        | value_expression 'AS' STRING_CONST | value_expression ;
declare_cursor_statement    : 'DECLARE' IDENTIFIER 'CURSOR' 'FOR' query_statement ;
open_cursor_statement       : 'OPEN' IDENTIFIER ;
close_cursor_statement      : 'CLOSE' IDENTIFIER ;
deallocate_cursor_statement : 'DEALLOCATE' IDENTIFIER ;
fetch_cursor_statement      : 'FETCH' fetch_type 'FROM'? IDENTIFIER ;
fetch_type              : 'NEXT' | 'PRIOR' | 'FIRST' | 'LAST'
                        | ( 'ABSOLUTE' | 'RELATIVE' ) '-'? UNSIGNED_CONST | /* empty */ ;
truncate_table_statement: 'TRUNCATE' 'TABLE' IDENTIFIER ;

ddl_statement           : create_index_statement | create_table_statement | drop_index_statement
                        | drop_table_statement | alter_table_statement ;
create_table_statement  : 'CREATE' 'TABLE' IDENTIFIER '(' column_definition ( ',' column_definition )* ')' ;
column_definition       : IDENTIFIER data_type ( '[' UNSIGNED_CONST ']' )? default_clause? column_constraint*
                        | IDENTIFIER data_type 'FOREIGN' 'KEY' 'REFERENCES' IDENTIFIER '(' IDENTIFIER ')' ;
default_clause          : 'DEFAULT' ( default_literal | 'CURRENT_USER' | 'USER' | 'USERNAME' | 'NULL'
                        | 'CURDATE' | 'CURRENT_DATE' | 'CURRENT_TIME' | 'CURTIME'
                        | 'CURRENT_TIMESTAMP' | 'CURTIMESTAMP' ) ;
default_literal         : ( '+' | '-' )? ( UNSIGNED_CONST | FLOAT_CONST ) | STRING_CONST | BIT_CONST
                        | HEX_CONST | ( 'TIME' | 'DATE' | 'TIMESTAMP' ) STRING_CONST | 'TRUE' | 'FALSE' ;
column_constraint       : 'NOT' 'NULL' | 'UNIQUE' | 'PRIMARY' 'KEY' | 'GLOBAL' ;
drop_table_statement    : 'DROP' 'TABLE' IDENTIFIER ;
create_index_statement  : 'CREATE' 'INDEX' 'ON'? IDENTIFIER ( '.' IDENTIFIER | '(' IDENTIFIER ')' ) ;
drop_index_statement    : 'DROP' 'INDEX' 'ON'? IDENTIFIER ( '.' IDENTIFIER | '(' IDENTIFIER ')' ) ;
alter_table_statement   : 'ALTER' 'TABLE' IDENTIFIER alter_table_action ;
alter_table_action      : 'ADD' 'COLUMN'? ( column_definition | '(' column_definition ')' )
                        | 'DROP' 'COLUMN'? IDENTIFIER
                        | 'ALTER' 'COLUMN'? IDENTIFIER ( 'SET' default_clause | 'DROP' 'DEFAULT' )
                        | 'RENAME' 'TO' IDENTIFIER | 'RENAME' 'COLUMN' IDENTIFIER 'TO' IDENTIFIER ;
```

Reading notes ([O-run] unless marked):

- `FROM` is mandatory: `SELECT 1` is a syntax error. Any table with a
  `WHERE` that keeps one row is a one-row source; the system tables have
  rows in every file (q13 uses `FROM FileMaker_Tables WHERE TableId =
  (SELECT MIN(TableId) FROM FileMaker_Tables)`).
- The select list is `*` alone or items; `*, 1` is a syntax error, `t.*, 1`
  is fine. Predicates (`1 = 1`, `x IS NULL`, `NOT TRUE`, `a AND b`) are not
  values: they appear only in `WHERE`, `HAVING`, `ON`, `CASE WHEN`, and as a
  `CASE x WHEN` operand (`common_expression`); never in the select list or
  `ORDER BY`.
- One sign per factor (`--1` in a select list is a syntax error, `3 --1` is
  `3 - -1`). `^` and `**` are left-associative and bind tighter than `*`,
  `/` and the sign: `2^3^2` = 64, `-2^2` = 4, `2*3^2` = 18. `AND` binds
  tighter than `OR`; `NOT NOT` is a syntax error.
- `+ INTERVAL` exists in `value_expression` (left-recursive, so interval
  steps chain) and as a `boolean_primary` alternative. `WHERE "Due Date" +
  INTERVAL 1 DAY = DATE '2014-11-25'` returned no rows (q02) [I: it parses
  through the `boolean_primary` alternative]; parenthesize interval
  arithmetic in predicates.
- `function_name` without parentheses is a call without arguments
  (`CURRENT_DATE`, `USER`); with empty parentheses it is a syntax error
  (`CURDATE()`, `CURRENT_TIME()`, `USER()`), except `DATE()`, `TIME()`,
  `TIMESTAMP()`. `IDENTIFIER ( ... )` calls a function of `FunctionTable`
  (5.2) or fails with FQL0030 "\"foo\" is an invalid function.".
- Not in the grammar (syntax errors): `RIGHT` and `FULL` joins, `NATURAL
  JOIN`, `JOIN ... USING`, `JOIN` without `ON`, `EXCEPT`, `INTERSECT`,
  subqueries in `FROM`, `LIMIT`, `TOP`, `OFFSET n` without `ROW(S)`,
  `FETCH` before `OFFSET`, `GROUP BY` an expression or an ordinal,
  `POSITION(... IN ...)`, `SUBSTRING(x FROM a FOR b)`, `LIKE ... ESCAPE`,
  `CHAR_LENGTH`, `OCTET_LENGTH`, `RANDOM`, `VALUE(...)`, `WITH`, `EXPLAIN`,
  `COMMIT`, `GRANT`, `CREATE VIEW`, `CAST` to `FLOAT`, `REAL`, `INTEGER`,
  `SMALLINT`, `DEC`, `DOUBLE`, `CHAR`, negative `OFFSET`/`FETCH` counts
  (q01, q03, q05, q12).
- Accepted by the grammar, rejected later: `FETCH NEXT ...` (FQL0001 at the
  `FETCH` token, 0-based as in 3.3), `TABLE name` (9.3), every non-query
  statement (11).

### 3.3 Error positions [O-run]

`(line:column)` after `FQLnnnn/`. Lines count from 1 and advance at LF
only (a CR starts no line: `Tasks¶¶WHERE x` reports line 1). Columns count
**code points** since the last LF (ANTLR's `charPositionInLine`; `'😀😀'`
counts 2).

- **FQL0001 from the lexer or the parser**: the column + 1 (1-based). It is
  the first syntax error ANTLR 4's default error strategy reports, lexer
  errors included; tokens are read as the parser needs them, so the first
  error in text order wins (`SELEC Task FROM Tasks _` → `(1:1)`, `SELECT
  Task FROM Tasks WHERE _x = 1` → the `_` at `(1:30)`). For a failed
  adaptive prediction the token reported is where the prediction began:
  `SELECT Task FROM Tasks WHERE` → `(1:24)` (the `WHERE`); `SELECT 1` →
  `(1:9)` (end of input); empty or blank statement → `(1:1)` / `(1:2)`.
  A parser generated with ANTLR 4 from section 3.2 gives these positions
  [I]; another parser must match the vectors (q01, q18).
- **Every other error**, FQL0001 raised by the visitor included: the
  token's 0-based column. Which token each check reports, from the vectors:
  the table name (FQL0002, 0048), the repeated name or alias (0003), the
  column or its qualifier (0006-0009, 0024), the string of a date literal
  (0010), the ordinal (0012), the operator of a comparison and the `IN`
  keyword (0017, 0018), the left operand of arithmetic and concatenation
  (0018), the `ELSE` of a `CASE` (0019), the function name (0020, 0021,
  0030), the subquery's `SELECT` (0022), the second `SELECT` of a `UNION`
  (0026, 0027, 0046), the operand of `CAST` (0023), the first select item
  without `GROUP BY` and the `GROUP` keyword with one (0025), the subscript
  (0028), the parameter (0031, 0032), the cursor name (0037), the aggregate
  (0049), the `OFFSET` or `FETCH` keyword (0050-0052), the `(` of a
  `VALUES` row (0014), the literal (0045), the container column of `GETAS`
  (0043).

## 4. Names

### 4.1 Tables [O-run]

- A table name is a **table occurrence** of the file
  (`FileMaker_Tables.TableName`), compared **case-insensitively** and
  otherwise exactly (`" Tasks"` does not exist). Each occurrence is an
  independent table (`"Related Tasks"` reads the Tasks base table);
  relationships play no part. Whether a base table name that no occurrence
  carries is accepted is untested (no sample has one).
- Names with spaces, punctuation, non-ASCII letters or keyword spellings
  must be double-quoted (`"Related Tasks"`, `"Guests | Contacts"`).
- Aliases: `FROM Tasks t`, `FROM Tasks AS t`, quoted `"x y"`, looked up
  case-insensitively. With an alias the table's own name is out of scope
  (`Tasks.Task` with `FROM Tasks t` is FQL0008). The same name or alias twice
  in one `FROM` is FQL0003. Too many tables is FQL0004 (limit not measured).
- An occurrence of a missing file (an external FileMaker data source that
  is not open) has no fields: `COUNT(*)` is 0, `SELECT *` fails with error
  100 and the text `<File Missing>` (string resource 129/56; `ExecuteSQLe`
  gives `? ERROR: <File Missing>`), a named column is FQL0007 (q10).
- Record-level access is checked through `DBAccess::HasRowAccess` [O-code;
  not run: the harness opens files with full access].

### 4.2 System tables [O-run]

Names are case-insensitive like other tables; none of them is listed in
`FileMaker_Tables`.

| Table | Columns (`SELECT *` order) | Rows |
|---|---|---|
| `FileMaker_Tables` | TableName, TableId, BaseTableName, BaseFileName, ModCount | one per table occurrence, ordered by name, `ROWID` 1, 2, ...; BaseFileName is the file name without extension (`work` for the harness's private copy); an occurrence of a missing file has empty BaseTableName and BaseFileName |
| `FileMaker_Fields` | TableName, FieldName, FieldType, FieldId, FieldClass, FieldReps, ModCount | one per field of each occurrence, by occurrence name, then FieldId |
| `FileMaker_BaseTables` | BaseTableName, BaseTableId, Source, ModCount | one per base table; Source `<Internal>` for FileMaker tables |
| `FileMaker_BaseTableFields` | BaseTableName, FieldName, FieldType, FieldId, FieldClass, FieldReps, ModCount | one per field of each base table |
| `FileMaker_ValueLists` | ValueListName, ValueListId, Source, ModCount | one per value list; Source `Custom`, `Field` or `Related` |
| `FileMaker_ValueList_<name>` | FirstValues | the list's values in order; a related list gives the one row `<Valuelist Unsupported>`, an unknown name the one row `<Value List Missing>` (string resource 129/64); neither is an error |

FieldType is `varchar`, `decimal`, `date`, `time`, `timestamp` or `binary`,
with `global ` in front for global fields and `[n]` after it for n
repetitions (`varchar[9]`, `binary[12]`); FieldClass is `Normal`,
`Calculated` or `Summary`. The binary's string list also has `boolean`
(`0x2e4bbac`) [O-code]. Statements that would change a system table fail
with FQL0048 "This statement contains an invalid operation on FileMaker
system table "FileMaker_Tables"." (q12).

### 4.3 Columns [O-run]

- A column is a field of the table, by name, case-insensitive; quoted when
  it is not an IDENTIFIER (`"TASK ID MATCH FIELD"`, `"Attachment |
  Container"`, `"_default"`, `"Date"`).
- Unqualified names search the tables of the current `FROM` and of the
  enclosing queries (correlated subqueries): in two tables of one scope
  FQL0006, in none FQL0007; `x.col` with an unknown `x` FQL0008, a known
  table without that column FQL0009.
- Select-list aliases (`AS name`, bare `name`, quoted) are visible to
  `ORDER BY` (`ORDER BY x`, `ORDER BY n` for `COUNT(*) AS n`), not to
  `GROUP BY` (FQL0007) or `WHERE`.
- `SELECT *` lists every field of every `FROM` table in `FROM` order,
  fields in FieldId (creation) order, globals, calculations and summaries
  included; `t.*` the fields of one table.
- FileMaker 26.1.1 adds the field display-name key `fm_sql`
  (file-metadata.md 1.2): it names the field's column in results and can
  be used in a query in place of the field name (Claris release notes,
  FileMaker Pro 26.1.1). DBEngine 26.1.1 resolves such names in
  `FQL::TableInfo::GetFieldKeyByAliasName` after
  `PrepopulateFieldAliasNameMap` [O: symbols]. Not run: how a display
  name that collides with another field's name, or a missing or non-JSON
  display-names result, resolves; whether ExecuteSQLe's column headings
  (`fieldSeparator` output has none) are affected [U].

### 4.4 ROWID and ROWMODID [O-run]

`ROWID` (keyword, or `"ROWID"` in any case) is the record ID
(`Get ( RecordID )`: 27-38 for the sample's Tasks; 1-n for system tables).
`ROWMODID`, a reserved column name (not a keyword), is the record's
modification count (`Get ( RecordModificationCount )`, 0 for a record never
modified). Both are numbers and can be qualified (`t.ROWID`). With more
than one table in `FROM` an unqualified `ROWID` is FQL0024. In the
null-supplied row of a `LEFT JOIN`, `b.ROWID` is **0**, not NULL, so
`COUNT(b.ROWID)` counts unmatched rows (q05, q06).

### 4.5 Repetitions [O-run]

`field[n]` reads repetition n; without a subscript repetition 1. The
subscript is any numeric value expression, evaluated per row
(`flex_Label[ROWID]` works) and truncated (`[1.5]` = 1). A constant below 1
is FQL0028 "Field repetitions must be numeric and between 1 and 32000."; a
text constant or `?` is FQL0028 with `0` as the bound ("between 1 and 0").
A repetition beyond the field's count is empty. Aggregates, `DISTINCT`,
`GROUP BY` and `ORDER BY` use the subscripted repetition (q08).

## 5. Values and types

### 5.1 SQL types [O-run]

| FileMaker field | FieldType | value in SQL |
|---|---|---|
| Text | varchar | text; empty text is NULL |
| Number | decimal | FixPt decimal (calc-values.md 2); empty is NULL |
| Date | date | date |
| Time | time | time, up to 6 fractional digits |
| Timestamp | timestamp | timestamp |
| Container | binary | container, read as its text form (8.1) |
| Calculation | the result type | the field's value as FileMaker computes it, unstored ones per row, independent of the call's context |
| Summary | the result type | see 8.1 |
| Global | `global ` + type | the session's global value on every row |

Literals: UNSIGNED_CONST and FLOAT_CONST are numbers, STRING_CONST text,
`DATE`/`TIME`/`TIMESTAMP '...'` the typed values (5.4), `TRUE` = 1 and
`FALSE` = 0 (numbers), `NULL` an untyped NULL.

**Empty is NULL.** The empty literal `''`, empty fields and empty values are
NULL: `'' IS NULL` is true, `'' = ''` and `NULL = NULL` are not,
`QuickFind = ''` and `QuickFind <> ''` match nothing, `COALESCE('', 'b')` =
`b`, `LENGTH('')` is empty (q03, q04).

### 5.2 Functions [O-run, O-code]

The scalar functions give exactly the results of the corresponding
calculation functions (q16 compares them digit for digit), because FQL
builds them as calculation operators (`FQL::Expr` over `Draco::DBCalc`)
[O-code]. An implementation can call om-calc:

| SQL | calculation function and notes |
|---|---|
| `ABS(n)` | `Abs` |
| `ATAN(n)`, `ATAN2(y, x)` | `Atan`; `ATAN2(1, 1)` = `.7853981633974483`, `ATAN2(1, 0)` = `1.5707963267948966` |
| `CEIL(n)`, `CEILING(n)`, `FLOOR(n)`, `INT(n)`, `SIGN(n)` | `Ceiling`, `Floor`, `Int`, `Sign` |
| `COS`, `SIN`, `TAN`, `SQRT`, `EXP`, `LN` | `Cos`, `Sin`, `Tan`, `Sqrt`, `Exp`, `Ln` |
| `LOG(n)`, `LG(n)` | `Log` (base 10), `Lg` (base 2) |
| `DEGREES(n)`, `DEG(n)`, `RADIANS(n)`, `PI()` | `Degrees`, `Radians`, `Pi` (`PI` without parentheses is a column name) |
| `MOD(a, b)` | `Mod` (`MOD(-7, 3)` = 2, `MOD(7, -3)` = -2, `MOD(7, 0)` = 7) |
| `ROUND(n, d)` | `Round` (`d` required, truncated) |
| `MAX(a, b)`, `MIN(a, b)` | larger/smaller of two numbers (text or dates: FQL0021) |
| `CHR(n)` | `Char` of n truncated and reduced to 8 bits (`CHR(8364)` = `¬`, `CHR(321)` = `A`, `CHR(256)` = U+0000, `CHR(-1)` = U+0001) |
| `LEFT(s, n)`, `RIGHT(s, n)` | `Left`, `Right` (negative n: error 14) |
| `SUBSTR(s, start [, len])`, `SUBSTRING` | `Middle` (no `len`: to the end; start below 1 behaves as `Middle` does: `SUBSTR(s, 0, 3)` and `SUBSTR(s, -2, 4)` return the first 3 and 4 characters; negative `len`: error 14) |
| `LENGTH(s)` | `Length` (`'👍🏽'` = 2, `'e'` + U+0301 = 1, `'😀'` = 1) |
| `UPPER(s)`, `LOWER(s)` | `Upper`, `Lower` (`ß` stays, final sigma `σας`, `İ` → `i`) |
| `LTRIM(s)`, `RTRIM(s)`, `TRIM(s)` | remove U+0020 only (TAB, CR, NBSP stay) |
| `TRIM(BOTH\|LEADING\|TRAILING c FROM s)`, `TRIM(c FROM s)` | remove repeated occurrences of the **first character** of `c` (`TRIM(BOTH 'xy' FROM 'xyaxy')` = `yaxy`, `TRIM(BOTH 'a' FROM 'aaa')` is NULL); `TRIM(BOTH FROM s)` trims spaces |
| `SPACE(n)` | n spaces (`SPACE(0)` is NULL) |
| `STRVAL(x)` | the text of x as ExecuteSQL prints it (8.2; `STRVAL(TRUE)` = `True`) |
| `NUMVAL(s)` | text to number in the **file's** locale (`'1,234.5'` = 1234.5 in en-US, `'1,5'` = 1.5 in de-DE); `''` is NULL; other non-numeric text is error 502 (`'12abc'`, `'$5'`, `'1 2'`) |
| `DATEVAL(x)`, `TIMEVAL(x)`, `TIMESTAMPVAL(x)` | ISO text (`Y-M-D`, `Y/M/D`; `H:M[:S[.f]]` with optional AM/PM; `Y-M-D H:M:S`, or a date alone), a number (days; seconds; seconds since 0001-01-01) or a typed value; other text is error 500, 501, 501 |
| `DAY`, `MONTH`, `YEAR`, `DAYOFWEEK`, `HOUR`, `MINUTE`, `SECOND` | `Day`, `Month`, `Year`, `DayOfWeek` (Sunday = 1), `Hour`, `Minute`, `Seconds` (fractions kept); the date parts of a time and the time parts of a date are FQL0021 |
| `DAYNAME(d)`, `MONTHNAME(d)` | `DayName`, `MonthName`, in the **file's** language (`Montag`, `måndag`, `月曜日`) |
| `EXTRACT(part FROM x)` | as the functions above (`HOUR` of a date is FQL0018) |
| `CURRENT_DATE`, `CURDATE`, `TODAY`, `DATE()` | today |
| `CURRENT_TIME`, `CURTIME`, `TIME()` | now, whole seconds |
| `CURRENT_TIMESTAMP`, `CURTIMESTAMP`, `TIMESTAMP()` | now, whole seconds (server clock; UTC in the container) |
| `USER`, `USERNAME`, `CURRENT_USER` | the account name |
| `PART(s, t)` | two texts, result a number (`PART(Task, 'a')` = 0); meaning open (12) |
| `COALESCE(a, b, ...)` | first non-NULL; two or more arguments of one type |
| `NULLIF(a, b)` | NULL when `a = b` (6.1: text compared exactly) |
| `CASE` | searched and simple forms; the simple form compares with `=` (case-sensitive) or evaluates a predicate (`CASE 1 WHEN 1 = 1 THEN 'a' ...` = `a`); no match and no `ELSE`: NULL |

Arguments are type-checked when the statement is built: a wrong type is
FQL0021 "Parameter number 1 to the function "ABS" is not of the correct
type." (`ABS('3')`, `UPPER(1)`, `LENGTH(123)`, `DAY('2014-02-17')`,
`HOUR(date)`, `SUM(text)`), a wrong count FQL0020. The untyped `NULL` fails
the numeric and date functions (FQL0021) but not the text ones
(`UPPER(NULL)` is NULL); `CAST(NULL AS INT)` is a typed NULL.
`IDENTIFIER ( ... )` names are the 20 entries of FQL's `FunctionTable`
(`ABS ATAN ATAN2 CEIL CEILING COS DEGREES DEG EXP FLOOR LG LN LOG MOD PI
RADIANS SIGN SIN SQRT TAN`, matched case-insensitively, quoted or not;
function codes 68-85) [O-code].

Domain errors abort the query at run time: `SQRT(-1)`, `LN(0)`,
`(-8)^(1/3)`, `LEFT(s, -1)` give error 14 (`Number out of range`); `1/0`,
`0/0`, `0^-1` error 15 (`Divide by zero`) (q03).

### 5.3 Aggregates [O-run]

`COUNT(*)`, `COUNT(x)` (non-NULL values), `SUM`, `AVG`, `MIN`, `MAX`,
`EVERY`, `SOME`, each with `ALL` (default) or `DISTINCT`. Over no rows
`COUNT` is 0 and the others NULL. `SUM` and `AVG` need numbers (FQL0021 for
text, dates, times); `MIN` and `MAX` take any type (text in the order of
6.1; `MAX` of a container column is NULL). Sums use the calculation's
decimal arithmetic (`SUM(1/3)` over 12 rows = `3.9999999999999996`).
`EVERY(x)` and `SOME(x)` take a value, not a predicate (`EVERY(id > 0)` is
a syntax error) and returned 1 for a number column; their meaning is open.

Aggregates cannot be part of expressions: `COUNT(*) + 1`,
`STRVAL(COUNT(*))`, `SUM(SUM(x))` are FQL0005 "Expressions involving
aggregations are not supported."; an aggregate in `WHERE` is FQL0049. Every
plain column next to an aggregate must be in `GROUP BY` (FQL0025). `HAVING`
without `GROUP BY` filters the one aggregate row.

### 5.4 Dates, times and timestamps [O-run]

- `DATE 'Y-M-D'` (also `Y/M/D`, one- or two-digit month and day, spaces
  around) for years 0001-4000; anything else (`'02/17/2014'`, `'20140217'`,
  `'14-02-17'`, `'2014-02-30'`, year 4001) is FQL0010 (error 8310).
- `TIME 'H:M[:S[.f]]'` with optional `AM`/`PM`; parts carry (`'13:61:00'`
  = 14:01:00), hours may exceed 23 (`'25:00:00'`) or be negative
  (`'-1:00:00'`); fractions keep 6 digits, truncated.
- `TIMESTAMP 'Y-M-D H:M[:S[.f]]'` needs a time; `24:00:00` is the next day;
  no `T`.
- Literals are parsed with `FQL::FQLLocale` (the `DateTime::Set(Text,
  Locale)` calls next to it, `0x137fb77`) [O-code], so they do not depend
  on the file.
- Arithmetic is the calculation's (calc-values.md 4.3): date ± number =
  date (fractions dropped), date − date = days, time ± number = time
  (seconds, no wrap: `23:59:59 + 2` = `24:00:01`), time − time = time,
  timestamp ± number = timestamp, timestamp − timestamp = a **time**
  (`24:00:00`), date + time = timestamp. A date before 0001-01-01 is NULL.
- `x ± INTERVAL n unit` with unit DAY, MONTH, YEAR, HOUR, MINUTE or SECOND
  (n a number or a numeric string); month and year steps overflow like
  `Date ( m + 1 ; d ; y )` (`2014-01-31 + INTERVAL 1 MONTH` = `2014-03-03`).
- A date, time or timestamp compared with **text** converts the text with
  the **file's** locale, not ISO: in an en-US file `"Due Date" =
  '11/24/2014'` matches and `= '2014-11-24'` does not; de-DE takes
  `'17.02.2014'`, sv-SE and ja-JP ISO; times compare with `'1:45:10 PM'` in
  en-US, `'13:45:10'` in the others. Text that does not convert matches
  nothing (no error). A date compared with a timestamp or a time matches
  nothing (q04, q13).

### 5.5 CAST [O-run]

| from \ to | VARCHAR | INT, DECIMAL, NUMERIC | DATE | TIME | TIMESTAMP |
|---|---|---|---|---|---|
| number | its text (8.2) | unchanged, **no truncation or rounding** (`CAST(12.9 AS INT)` = 12.9, `DECIMAL(5,1)` keeps 12.345) | day number (735281 = 2014-02-17) | seconds | seconds since 0001-01-01 00:00 |
| text | unchanged (`VARCHAR(4)` does not truncate) | as `NUMVAL` (error 502) | ISO text only (error 500) | as `TIMEVAL` (501) | as `TIMESTAMPVAL` (501) |
| date | ISO text | day number | | seconds since 0001-01-01 as a time (`17653440:00:00`) | its midnight |
| time | `HH:MM:SS` | seconds | | | 0001-01-01 plus the time |
| timestamp | its text | | its date | its time | |
| `TRUE`/`FALSE` | `True`/`False` | 1/0 | | | |
| container | its text form | | | | |

`BOOLEAN`, `BLOB`, `VARBINARY` and `LONGVARBINARY` targets are FQL0023 "A
CAST expression requested an invalid data type conversion." from any type.
`CAST(NULL AS t)` is a typed NULL. Length, precision and scale are accepted
and ignored.

### 5.6 Operators and type rules [O-run]

Arithmetic is the calculation's (calc-values.md 2.3): exact sums, quotients
at 16 decimal places (`1/3` = `.3333333333333333`, `10/3` =
`3.3333333333333333`, `1/7` = `.1428571428571429`), no integer division
(`7/2` = 3.5).

Type checks when the statement is built (FQL0018 "An expression contains
incompatible data types.", FQL0017 "An expression contains data types that
cannot be compared."), from the vectors:

- Comparisons (`=`, `<>`, `<`, ..., `BETWEEN`, `IN`) need one type on both
  sides, except date, time or timestamp against text (5.4) and anything
  against `?`. Number vs text, text vs date literal, number vs date are
  errors (`"TASK ID MATCH FIELD" = '3'`, `Task = 3`, `"Due Date" = 3`).
  `IN` lists are checked item by item; a `NULL` in an `IN` list is FQL0017.
- `LIKE` needs text on both sides (FQL0018 for a date or number column);
  containers cannot be compared at all.
- `+`: number + number; text + text and text − text concatenate (`'a' +
  'b'` = `'a' - 'b'` = `ab`); number with text gives a number (`1 + '2'` =
  `'1' + 2` = 3, `'a' + 1` = 1); date + text adds nothing (`DATE
  '2014-02-17' + 'x'` = the date). `*` and `/` need numbers.
- `||`: text on the left, anything on the right (`'a' || 1` = `a1`; a date
  column in the file's date format, `Task || "Due Date"` = `...11/24/2014`;
  a date literal as `2-17-2014`; a time literal as `13:45:10`). A number or
  date on the left is FQL0018 (`1 || 'a'`, `DATE '...' || 'x'`,
  `"TASK ID MATCH FIELD" || Task`).
- NULL: `NULL || x` and `x || NULL` are x; `1 + NULL` and `NULL + 1` are
  NULL; `NULL * 2` is FQL0018.
- `CASE` branches need one type (FQL0018), all-NULL branches are FQL0019;
  the arguments of `COALESCE` and `NULLIF` likewise.

### 5.7 Parameters [O-run]

- `?` takes the next argument. Its value keeps the argument's
  **calculation type** (text, number, date, time, timestamp) and is not
  converted to an SQL type first. In the select list it prints as the
  calculation prints it, in the file's locale: a date gives `2/17/2014` in
  en-US, `17.02.2014` in de-DE, `2014/02/17` in ja-JP; a timestamp
  `2/17/2014 1:45:10 PM` in en-US (q11, q13).
- Compared with a column, the argument converts to the column's type:
  numbers from text leniently, but not like `GetAsNumber` (`'3abc'`,
  `'3¶4'`, `'1,2'` match 3, 3 and 12, `'a3b4'` nothing; `GetAsNumber` gives
  3, 34, 12, 34); dates from text in the file's locale (`'11/24/2014'`), not
  from numbers or timestamps; text compares exactly (a number argument
  matches no text). An argument that does not convert matches nothing (q11,
  q18).
- In arithmetic the argument's own type acts first and the result is then
  converted, through its text, to the type the other operand implies: a date
  `? - 1` gives `2162014` (the text `2/16/2014` as a number), a time
  `? + 0` gives `10000`, a date `? * 1` is NULL (q11, q18).
- The type must be inferable: `? + ?` and `? = ?` are FQL0031 "The the
  parameter's type cannot be inferred in this context. At least one query
  parameter must be an expression, a column or a constant." (sic).
- `?` is not allowed as an `OFFSET` or `FETCH` count (FQL0050, FQL0052), as
  a table (syntax error) or as a repetition (FQL0028); in `ORDER BY` it has
  no effect.
- An empty argument is NULL (`? IS NULL` is true). An argument with a
  calculation error gives the result `?` with that error.
- Named parameters `:name` parse and are always NULL: neither arguments nor
  `$`/`$$` variables bind them. Mixing `:name` with `?` is FQL0032 (q11).

## 6. Comparison, ordering, duplicates

### 6.1 Text comparison [O-run, O-code]

FQL converts each value it reads into `FQL::FQLLocale` (`DBData::Translate
(..., FQLLocale, ...)` where columns are read, e.g. `0x136d513`), a
`Draco::Locale` whose collation is the **Unicode raw** `International`
(`Locale::FixedInternationals(2)`, read from the running engine with
`tools/fms-sql`, `locale` mode) [O-code]. So, unlike the calculation
(calc-values.md 5.3, the file's language), SQL text comparison is binary:

- `=` / `<>`: same UTF-16 text (`'a' <> 'A'`, `'é' <> 'e'` + U+0301,
  `'ß' <> 'ss'`, `'a' <> 'a '`).
- `<`, `>`, `BETWEEN`, `MIN`, `MAX`: UTF-16 code unit order (`'B' < 'a'`,
  `'Z' < '_' < 'a'`, `'z' < 'ä'`, `'10' < '9'`, `' a' < 'a'`).
- `LIKE`: case-sensitive; `%` any run, `_` one character, nothing else is
  special (`[a]`, `*`, `?` are literal) and there is no escape. The pattern
  may be a column (`'Checklist' LIKE Task`); `LIKE NULL` and `LIKE ''`
  match nothing.
- `a PART OF b`: `b` contains `a` (case-sensitive; `'New' PART OF Task`
  matches "Hire New Sales Person" and "New Logo Art").
- The same in en-US, de-DE, sv-SE and ja-JP files (q13).

### 6.2 Sort and duplicate key [O-run]

`ORDER BY`, `DISTINCT`, `UNION` (without `ALL`) and `GROUP BY` compare by a
key, not by 6.1 (FQL sorts with `DBRowSet::Sort` and
`DBMetaTree::MakeSortIndexKey` [O-code]):

- Numbers, dates, times, timestamps: by value.
- Text: (1) drop leading U+0020 and U+3000 (not TAB, NBSP, U+2002,
  U+2003, U+200B, U+FEFF); (2) if the rest begins with CR, LF or U+2028 the
  key is empty; (3) otherwise the key is the first **100 UTF-16 code units**
  of the rest, compared as code units. CR, LF and TAB later in the text are
  ordinary units (`'b\t' < 'b\n' < 'b\r' < 'b '`).
- NULL, and any text with an empty key, sorts first ascending, last
  descending.
- So `' b'`, `'  b'`, `'　b'` and `'b'` are duplicates (UNION keeps the
  first in input order), as are `''`, `' '`, `'¶b'`, `'\nb'` and `'¶'`; two
  texts that agree in their first 100 units are one value for `DISTINCT`
  and `UNION` (the second is dropped although they differ later) and tie in
  `ORDER BY` (q13b, q13c). This is not `International::MakeKey` of the
  Unicode raw collation, which is plain UTF-16BE up to 254 bytes and keeps
  leading spaces (`keys` mode) [O-code]; the builder is open (12, S1).
- Ties keep input order in both directions (`DESC` reverses keys, not
  ties). Exceptions on grouped rows: `ORDER BY` an aggregate alias or
  ordinal gave ties in descending group order (`Personal|2`, `Home|2`), and
  `ORDER BY COUNT(*)` (the aggregate itself) did not reorder (q05).

### 6.3 Row order [O-run]

- One table without `ORDER BY`: creation order (`ROWID` order, FileMaker's
  unsorted found-set order), whatever the `WHERE`.
- Joins (`,`, `JOIN`, `CROSS JOIN`, `LEFT JOIN`): nested loops in `FROM`
  order, the first table outermost, each table in creation order; a `LEFT
  JOIN` emits its null-supplied row where the inner rows would be.
- `GROUP BY`, `DISTINCT`, `UNION`: ascending by the 6.2 keys of the output
  columns from left to right (`Business`, `Home`, `Personal`), then any
  `ORDER BY`. `UNION ALL` appends the second input after the first; in
  `a UNION ALL b UNION c` the final `UNION` dedups and sorts the whole.
- `DISTINCT` with `ORDER BY` a column that is not selected returned the
  values in descending order (one vector).
- `OFFSET n ROW|ROWS` skips n rows after ordering; `FETCH FIRST n ROW|ROWS
  ONLY` keeps n (`FETCH FIRST ROW ONLY` keeps 1); `n PERCENT` keeps
  ⌈n × rows / 100⌉ (25% of 12 = 3, 10% = 2, 1% = 1, 150% = all); `WITH
  TIES` adds the rows equal to the last one in the `ORDER BY` keys and needs
  `ORDER BY` (FQL0051, error 8310). Counts must be non-negative integer
  constants (FQL0050, FQL0052; `FETCH FIRST 0` is FQL0052). `OFFSET 1 ROWS`
  on an aggregate-only query gives no rows.

## 7. Queries [O-run]

- **Joins**: `ON` takes any predicate (`a.x = b.y + 1`, `1 = 1`, `a.d >
  b.d + 300`, conjunctions); no FileMaker relationship is needed. `LEFT
  [OUTER] JOIN` null-supplies the right table (columns NULL, `ROWID` 0); a
  condition on the right table inside `ON` keeps unmatched left rows, in
  `WHERE` it removes them. `CROSS JOIN` and `,` are Cartesian products.
  A join may be parenthesized.
- **Subqueries**: scalar subqueries in the select list and in comparisons
  (no row: NULL; several rows: runtime error 8303 "A scalar subquery
  returned multiple rows."; several columns: FQL0022); `IN`/`NOT IN` with
  a subquery or a list; `= ANY`, `< ALL`, `= SOME` with a subquery or a list
  (`> ALL` over no rows is true, `= ANY` false); `EXISTS`, `NOT EXISTS`
  (parentheses optional) and `UNIQUE` [I: true when the subquery has no
  duplicate rows; one vector]; correlated references to outer tables in all
  of them.
- **UNION**: same number of columns (FQL0026) and same types (FQL0027; NULL
  and an empty global combine with text); containers only with `UNION ALL`
  (FQL0046). `ORDER BY` after a union sorts the whole result and names
  columns of the first input.
- **`FOR UPDATE [OF ...]`** is accepted and ignored (q12).

## 8. Field kinds and the text of results

### 8.1 Field kinds [O-run]

- **Global fields**: the same value on every row.
- **Calculations**, stored or unstored: the field's value as FileMaker
  computes it, independent of the call's context.
- **Summary fields** (one sample, q09: a maximum over an empty field): the
  column reads empty, `COUNT` is 0, `MAX` is NULL, but `IS NULL` is false.
  Over data (q19, q20 on the file-metadata.md probe's six records, 12 S3)
  a summary column is the summary over the query's found set as it shows
  on each row's record: a total is the same on every row, a running total
  the total so far in found-set order, a fraction the row's share. Without
  a `WHERE` the found set is the whole table (every row of `SELECT Task,
  S_Total FROM Tasks` reads 15, also under `GROUP BY` and in `COUNT(S_Total)`
  6, `SUM(S_Total)` 90); with one, the records it leaves, whatever its form
  (`Status = 'Done'` 6; `NOT`, `OR`, `N + W > 5`, `UPPER(Task) = 'A' OR N =
  5`, `Task IN (SELECT ...)` alike), in record order (running totals under
  `ORDER BY N DESC` keep record order). A summary in the `WHERE` itself
  reads the whole table (`WHERE S_Total = 15` keeps every row). In a join
  only the first table's own conditions narrow its found set; a later
  table keeps all its records (`b.S_Total` is 15 with `WHERE b.Status =
  'Done'` when b is the second table, 6 when it is the first). A subquery has a found set of its own. `FETCH FIRST`,
  `OFFSET` and `DISTINCT` cut the found set to the result's records
  (`FETCH FIRST 2 ROWS ONLY` totals the two rows, `ORDER BY Task FETCH
  FIRST 2` the two first by Task, `DISTINCT Status, S_Total` the two
  records it keeps), and with a `WHERE` and an `ORDER BY` running totals
  follow the `ORDER BY` (`WHERE N > 1 ORDER BY Task`: b 2, c 5, d 9, e
  14).
- **Containers**: read as their text form, the stored file name
  (`mbs.jpg`, what `GetAsText` gives on the record); empty containers are
  NULL. `GETAS(c, 'TYPE')` with a valid stream name returns the same text
  in ExecuteSQL (`FILE`, `FNAM`, `JPEG`, `PNGf`, `GIFf`, `PDF `, `snd `,
  `SIZE`, `MAIN` and `DEFAULT` accepted; `file`, `FIL`, `XX` are FQL0043
  "The string "file" is not a valid stream name."; a text column is
  FQL0021). Containers cannot be compared (`=`, `LIKE`: FQL0018) or used in
  `UNION` (FQL0046); `IS [NOT] NULL`, `COUNT` and `ORDER BY` (no effect)
  work.
- **Repeating fields**: 4.5.

### 8.2 Text of each type [O-run, O-code]

Every output value is converted with `DBData::Translate(..., FQLLocale)`
[O-code]; none of the forms depends on the file's locale (q13):

| type | text |
|---|---|
| number | calc-values.md 2.7 with `.` as the decimal separator in every locale: no leading `0` (`.5`, `-.25`), no trailing zeros, scientific `1.0E+9` when the integer part is a non-zero multiple of 10^9 and `1.0E-10` when 0 < \|x\| < 10^-9 (`1000000000.5` = `1.0000000005E+9`, `1500000000` stays, `1e-9` = `.000000001`) (q15, q15b, q15c) |
| date | `YYYY-MM-DD`, year zero-padded to four digits (`0001-01-01`) |
| time | `HH:MM:SS`, at least two hour digits (`01:02:03`, `25:00:00`, `100:00:00`, `-01:00:00`, `-00:01:00`), then `.` and up to 6 fractional digits without trailing zeros (`13:45:10.5`) |
| timestamp | `YYYY-MM-DD HH:MM:SS[.ffffff]`, hours 00-23 |
| `TRUE`/`FALSE` | `1`/`0` |
| container | the text form (file name) |
| text | unchanged |
| NULL | empty |
| `?` parameter | the calculation's text of the argument in the file's locale (5.7) |

### 8.3 Separators in data [O-run]

Values are not quoted or escaped: a value that contains a separator is
indistinguishable from a boundary (`SELECT 'a¶b', 'c'` over two rows with
`|` and `¶` gives `a¶b|c¶a¶b|c`).

## 9. Errors

### 9.1 Error text [O-run, O-code]

`ExecuteSQLe` returns `? ERROR: ` and then:

- for an FQL error: `FQL`, the four-digit code, `/`, `(line:column)` (3.3),
  `: ` and the message with its placeholders filled (`? ERROR:
  FQL0002/(1:14): The table named "Foo" does not exist.`). The messages are
  `FQL::NonLocalizedStrings(n)`, English in every locale, n the code (index
  0 is the `(^0:^1)` position pattern; the placeholders are U+007F and a
  digit in the binary): `catalogs/execute-sql-errors.tsv`. Placeholders take
  the name or literal as written, unquoted (`Task"` for `"Task"""`;
  `B'0101'` keeps its prefix and quotes). FQL0050 and FQL0052 end with a
  space.
- for an engine error: ` (code): ` and the engine message
  (`FQL::ErrorToString(int)`; note the two spaces after `ERROR:`): `?
  ERROR:  (14): Number out of range`, `? ERROR:  (3): Unavailable command`,
  `? ERROR:  (8303): A scalar subquery returned multiple rows.`. The 176
  codes with their own message are in the same catalog; every other code
  gives `FileMaker error`.
- error 100 (an occurrence of a missing file) shows its extended text
  instead: `? ERROR: <File Missing>`.

### 9.2 Calculation error codes [O-run]

| `EvaluationError` | cause |
|---|---|
| 8310 | FQL0001 (lexer, parser, `FETCH NEXT`), FQL0010 (bad date literal), FQL0051 (`WITH TIES` without `ORDER BY`) |
| 8309 | every other FQLnnnn (names, types, functions, aggregates, ...) |
| 8303 | a scalar subquery with several rows (at run time) |
| 3 | a non-query statement after its checks (11) |
| 14, 15 | domain error, division by zero (5.2) |
| 500, 501, 502 | text to date, time or timestamp, number failed (`DATEVAL`, `TIMEVAL`, `TIMESTAMPVAL`, `NUMVAL`, `CAST`) |
| 100 | an occurrence of a missing file |
| others | errors of the calculation's own arguments (1201, 1204; 15 from `1/0`; 1224 for a field argument without a record) |

The first error stops the query and no partial result is returned (`SELECT
1/0, Task` gives just the error).

### 9.3 `TABLE name` [O-run]

`TABLE Tasks` passes the grammar but the visitor builds no query: the
result is error 8310 with an uninitialized error record (`? ERROR:
FQL-1538144528/(21846:-246688): ` in one run). OpenMaker cannot match that;
it should give FQL0001 at the `TABLE` token with error 8310. The vector is
marked `depends=uninitialized`.

## 10. Execution

### 10.1 How a query runs [O-code, I]

The executor is `FQL::Query` with iterators named for their job (symbols):
`ScanIter` (every record of a table), `RidListResultIter` (the records of a
record-ID list, i.e. a found set), `INLJoinIter` (index nested-loop join:
`LookupValues(DBField::LookupDef::LookupType, DBRidList const*, ...)` finds
the matching records through the join field's index), `NullSupplyNLIter` and
`NullSupplyINLIter` (left outer joins, plain and indexed),
`FastCountStarIter` (`COUNT(*)` from the record count), `DirectSelectIter`,
`ProjectionIter`, `SubQueryExecuteIter`, `ResultIter`. Predicates are
analysed by `PredInfo` (`Setup`, `FindAllTables`, `FieldOfTable`) with
`CanBeIndexed` and `HasIndex`. Engine services called from the FQL code
range (call sites in `0x1300000`-`0x1560000`): `DBFind::Find` with
`DBFindRow` requests and `DBFinder` (finds), `DBIndexCursor` and
`DBIndexRange::Find` (index ranges), `DBRelation` and
`Query::ExecuteCreateRelation` (temporary relationships for joins),
`DBTempTableMgr::CreateTempTable`/`DestroyTempTable` with
`DBRecordCursor::Create`/`PutData` (temporary result tables),
`DBRowSet::Sort` and `DBMetaTree::MakeSortIndexKey` (ordering, 6.2),
`DBResultSet::Include`/`Exclude` (`FQL::ExcludeDups`),
`DBAccess::HasRowAccess`, `DBTransaction` (the DML path),
`RemoteTaskHandler`.

Inference from these names and the results: `WHERE` conjuncts on indexed
fields of one table become FileMaker finds (a found set as a record-ID
list); joins on indexed fields look the other table up through its index;
everything else is evaluated row by row with the calculation engine;
results go to a temporary table and are sorted there. Index use is only an
optimization: every result observed follows the exact rules of 6.1 (a
FileMaker find on an English-indexed text field would match `checklist` to
`Checklist`, FQL's `=` does not), so rows an index proposes must be checked
again with the SQL comparison [I].

### 10.2 Hosted files [I]

The client links the whole FQL engine (architecture.md 7) and runs
`ExecuteSQL` in its own `libDBEngine`: parsing, planning and evaluation
happen on the client. On a hosted file its finds go through the remote
query machinery FileMaker finds use (`DBRemoteQueryProgram`, op86,
query-program.md), record data comes through the client's record cache, and
the temporary tables live on the client. No remote operation is specific to
SQL (no `RC*SQL*` operation among the symbols; `FMODBC::RCODBC*` serve ODBC
data sources). Not observed on the wire.

### 10.3 Consequences for OpenMaker

- Parse with the grammar of 3.2 (an ANTLR 4 grammar rebuilt from it gives
  the syntax error positions of 3.3 most simply); resolve names through the
  file's occurrences and fields; build expressions from om-calc values and
  functions (5.2, 5.6), with the Unicode-raw text comparison of 6.1 in place
  of the file's collation.
- Execute with a record-ID list per table (om-query's finds give them from
  indexes for local and hosted files, with the 6.1 recheck), nested-loop
  joins in `FROM` order, a stable sort with the key of 6.2, then `OFFSET`
  and `FETCH`, then the text of 8.2.
- OpenMaker Server needs nothing SQL-specific for `ExecuteSQL`; a hosted
  OpenMaker client runs it over its record cache and op86 programs.

## 11. Statements other than queries [O-run]

`INSERT`, `UPDATE`, `DELETE`, `CREATE TABLE`, `DROP TABLE`, `ALTER TABLE`,
`CREATE INDEX`, `DROP INDEX`, `TRUNCATE TABLE` and `DECLARE ... CURSOR` are
parsed and checked (unknown table FQL0002, unknown column FQL0007, `VALUES`
count FQL0014, system table FQL0048) and then fail with error 3
`Unavailable command`; nothing changes (counts and values are unchanged
after each, q12). `OPEN`, `FETCH`, `CLOSE`, `DEALLOCATE` and `WHERE CURRENT
OF` fail with FQL0037 "There is no cursor with the name "c"." (ExecuteSQL
has no cursors). The DML and DDL code exists (`Query::ExecuteInsertValues`,
`PerformUpdate`, `PerformDelete`, `ExecuteCreate`, `ExecuteAlter`,
`ExecuteDrop`, `ExecuteTruncate`, `ExecuteCursorOp`) for other callers (ODBC
and JDBC sharing, the plug-in API) [I]; which of them `ExecuteFileSQL` runs
is open (S8).

## 12. Unknowns and the probes that would settle them

| # | Question | Probe |
|---|---|---|
| S1 | The text sort key builder (6.2): why only U+0020 and U+3000 are dropped, why a leading line break empties the key, and how the 100-unit cut treats a split surrogate pair | Static: `DBMetaTree::MakeSortIndexKey` and the text key it builds; harness: `'  ¶b'`, U+2029, a surrogate pair at units 100/101 |
| S2 | Number fields that hold text (`12abc`, `1,5`): value, comparison, output | A file with such data (FileMaker Pro oracle) |
| S3 | Summary fields over data: per-row value or table total | Settled (8.1): the summary over the query's found set (q19, q20) |
| S4 | `EVERY`, `SOME`, `PART(a, b)`; text value of a hex literal | Vectors over mixed values |
| S5 | A base table name without a same-named occurrence in `FROM` | A file with such a base table |
| S6 | Record access (`HasRowAccess`): hidden rows or an error | Harness session with a restricted account (it now opens with security bypass) |
| S7 | Hosted execution: which finds and sorts go to the host | Capture of a hosted `ExecuteSQL` from FileMaker Pro (op86 traffic) |
| S8 | `ExecuteFileSQL` / `ExecuteFileSQLTextResult`: allowed statements (DML?), typed results | A harness mode calling `FQL::ExecuteFileSQL` |
| S9 | Tie order of `ORDER BY` over grouped rows and of `DISTINCT ... ORDER BY` a non-selected column; `ORDER BY` an aggregate | More q05 rows with ties |
| S10 | The table limit of FQL0004 | A generated query with many joins |
| S11 | Container text form of references and images, `GETAS` in `ExecuteFileSQL` (binary results) | MBS container examples with references |
| S12 | Whether FileMaker Pro (client; system formats on) prints parameters and compares dates with text the same way | FileMaker Pro oracle: the en-US rows of q11 and q13 |
| S13 | How deep a statement may nest (parentheses, subqueries, operator chains, joins) before it fails, and with which error (OpenMaker refuses past 500 levels with FQL0001, "Implementation notes") | Harness, on a disposable server since deep text may crash it: `ExecuteSQLe` with 50, 100, 200 and 1,000 nested parentheses, subqueries and `+` operators |

## 13. Implementation checklist

1. Lexer and parser of section 3 (case-insensitive reserved keywords,
   `"..."` names, `//` comments ended by LF only) and the error positions of
   3.3.
2. Name resolution of section 4 with the system tables, `ROWID`,
   `ROWMODID`, repetitions and missing files.
3. Type assignment and the build-time checks of 5.2, 5.6 and 5.7 with their
   FQL codes and positions.
4. Values: Unicode-raw text comparison, empty = NULL, the calculation's
   arithmetic and functions, typed parameters.
5. Execution: filters, joins, aggregates, `GROUP BY`/`HAVING`,
   `DISTINCT`, `UNION`, the sort key of 6.2, `OFFSET`/`FETCH`.
6. Output text of 8.2 and the result conventions of 1.2; error texts of 9.1
   from `catalogs/execute-sql-errors.tsv`.
7. Non-query statements: check, then error 3.
8. Test with `catalogs/execute-sql-vectors.tsv` (skip `depends=clock`,
   `session` and `uninitialized`; the `file` column names the sample).

## 14. Files

- `tools/fms-sql/` (README): `fqlgrammar.cpp`, `run.sh`, `atn_grammar.py`,
  `make_battery.py`, `runq.sh`, `view.sh`, `make_sql_vectors.py`,
  `make_error_catalog.py`, `batteries/` (23 sources and the generated
  batteries), `results/` (run results, `fql-grammar.txt`,
  `errors-dump.txt`).
- `catalogs/execute-sql-vectors.tsv`: expression, file, context, expected
  result, expected error, evidence (battery, section, result type,
  `depends=`); calceval escapes `\r \n \t \\ \uXXXX` in expression and
  result.
- `catalogs/execute-sql-errors.tsv`: the FQL messages with the
  `EvaluationError` they came with, and the engine error texts.

### Implementation notes (om-sql)

`crates/om-sql` implements this spec. What it settled beyond the sections
above, from the vectors (all [O-run] against `catalogs/execute-sql-vectors.tsv`
unless marked):

**Parsing (3).** The lexer follows 3.1 (longest match, keywords before
names, fallback to the longest accepted prefix). The parser builds an ATN
from the text of 3.2 the way ANTLR's `ParserATNFactory` does (rule start
and stop states, block, loop and optional decisions, follow links from rule
ends to every caller, blocks of single tokens reduced to sets) and walks it
like ANTLR's `ParserInterpreter`, with ANTLR 4's ALL(*) prediction: SLL
with the empty outer context and a per-decision DFA cache, full LL with the
real call stack on an SLL conflict, graph-structured contexts with ANTLR's
merge rules, and `getSynValidOrSemInvalidAltThatFinishedDecisionEntryRule`
when every alternative dies. The left-recursive `value_expression` is its
primary alternative followed by a loop of its `INTERVAL` suffix; the
precedence predicate is taken as true and the unreachable rules of 3.2 are
not built. This reproduces all 118 lexer and parser positions of the
catalog. The reported token is the offending token of the first failure
(a `match`, or the token at which prediction found no alternative), not the
token where prediction began as 3.3 suggests: `FROM Tasks AS Date` reports
`AS` because prediction of the alias, failing at `Date`, falls back to the
alternative that leaves the rule (`AS 'str'` can follow a bare subquery in
`VALUES`), and the parser then fails at `match(EOF)` on `AS`; `WHERE <EOF>`
reports `WHERE` the same way. Lexer errors come first when the lexer
produced the bad character's token before the parser failed. Statements
such as eight `UNION ALL` parts with a parenthesized subquery each make full
LL scan to the end of the text for every subquery (the bare-subquery
alternative can absorb the following `UNION`), as ANTLR does; about a second
for a 5 KB statement in a release build.

**Nesting (OpenMaker's limit; FileMaker's is unmeasured, S13).** A
statement may nest 500 levels (`om_sql::parse::MAX_DEPTH`): the height of
its parse tree, one level for each rule of 3.2 on the way from `statement`
down to a token and one for each operator of a chain of `+ - || * / ^` (an
`INTERVAL` suffix included) and each join, since om-sql builds those chains
left-deep. SLL prediction holds its contexts to 500 rule invocations below
the decision. Deeper text is FQL0001 at the token where the parser or
prediction passed the limit: with 2,000 parentheses around `1=1` after
`WHERE a = 'x' OR`, at the 99th. That allows (release build, measured) 97
parentheses around a value, 98 around a condition (120 after `NOT`), 41
nested function calls, 45 `COALESCE`, 68 `CASE`, 81 `CAST`, 24 to 44
nested subqueries (fewest with `IN`), 245 parenthesized tables, and chains
of 486 operators or about 485 joins. Full LL contexts are not bounded:
they grow with flat statements too, since a bare subquery can take the
`UNION` parts after it into itself (above), so for each subquery of q13's
52-part `UNION ALL` statements full LL follows an alternative nested about
ten rules deeper per part. Building, planning and running a statement, and
merging, comparing and dropping prediction contexts, continue on a heap
segment when less than 256 KiB of the thread's stack is left (`stacker`,
as om-calc's evaluator does) or do not recurse, so a statement within the
limit runs on any thread from any call depth. Before, 2,000 nested
parentheses (4 KB) or a chain of about 1,000 operators overflowed the
2 MiB stack of an OpenMaker Server script thread and aborted the server.
Parse time grows about linearly with parentheses (0.45 ms a level) and
quadratically with nested subqueries, for which full LL scans to the end
of the text; the slowest statement within the limit, 44 nested `EXISTS`
subqueries, takes about 0.6 s. Flat statements are not limited and can be
slower: 52 parts of q13's shape take about 2.3 s and 104 parts about 16 s
(release build, 2026-11-19), so the parser stops at its source's abort or
time limit as the executor does (Interruption, below). Each thread keeps
its SLL prediction cache (the DFA states, as ANTLR keeps one per
recognizer) between statements; it only grows, so a parse starts it
afresh once it holds 100,000 DFA states and configurations
(`om_sql::parse`, about 25 MB). Before, 40,000 random statements grew one
thread's cache past 1.5 GB, on each OpenMaker Server thread that ran them
([#122](https://github.com/OpenMaker-Dev/openmaker/issues/122)).

**Checks (4-7).**

- FQL0025 counts only select-list and `HAVING` columns (`ON` columns of a
  `LEFT JOIN` do not).
- Any literal in `ORDER BY` is an ordinal reference: `ORDER BY 'x'` and
  `ORDER BY 1.5` are FQL0012 with the literal as written.
- `IN`, `ANY`, `ALL`, `SOME` over a list of one subquery use the subquery
  (the grammar parses it as a list).
- A date, time or timestamp **literal** beside a text column is FQL0018;
  a date column beside a text column or a text literal, and a date literal
  beside a text literal, are allowed. A container beside `?` is allowed (no
  row matches).
- A text or `?` subscript is FQL0028 with the placeholder left unfilled
  (the message ends `between 1 and ` U+007F `0.`); a negative constant
  gives `32000`.
- `GETAS` accepts `SND ` beside the stream names of 8.1; FQL0043 is at the
  `GETAS` operand.
- `NUMVAL` takes numbers as well as text; `DATEVAL`, `TIMEVAL`,
  `TIMESTAMPVAL` take dates, times and timestamps.

**Values and results.**

- `LEFT`/`RIGHT` with a negative count and `SUBSTR` with a negative length
  are error 14 (om-calc's `Left`, `Right`, `Middle` return empty there).
  `CHR(n)` is |n| mod 256.
- `SUM` and `AVG` add their values at the display precision
  (`SUM(1/3)` over 12 rows is `3.9999999999999996`).
- A date beside a time or timestamp compares their numbers (days against
  seconds): `=` never matches, `SomeTimeStamp > DATE '2000-01-01'` does.
  Text compared with a date converts with the file's locale and never
  matches when it has a time part (`'9/5/2015 0:00'`).
- `date ± INTERVAL n unit` is a timestamp (midnight-based); the select list
  shows it as the date column it is typed as, and comparisons see the
  timestamp (`"Due Date" + INTERVAL 1 DAY = DATE '2014-11-25'` matches
  nothing).
- `GROUP BY` rows come out in group key order, not ordered by the output
  columns (6.3 holds for `DISTINCT` and `UNION`). `ORDER BY` an aggregate
  column of grouped rows (by alias or ordinal) reverses the groups before
  the stable sort, which gives the descending tie order of 6.2 in both
  directions.
- Calculation fields read through a cursor over their table's found set:
  the records left by the `WHERE` conjuncts that read only that table
  (comparisons, `BETWEEN`, value lists, `LIKE`, `IS NULL`, and their
  `AND`, `OR` and `NOT`; not on the null-supplied side of a `LEFT JOIN`).
  In a join only the first table's cursor is narrowed; the other tables'
  conjuncts narrow their candidates only, and their cursors see every
  record (8.1, q20). `Get ( FoundCount )` is its size and `Get (
  RecordNumber )` the record's position: the sample's `If ( Get (
  FoundCount ) = 1 ; "Result" ; "Results" )` reads `Result` for `WHERE id
  = 3` and `Results` for `WHERE id < 3` and without a `WHERE`. A
  calculation that is `?` reads as empty. A summary field's value is asked
  of the source like any field (empty means `Val::Summary`, empty but not
  NULL, as q09); a source computes it over the cursor's found set.
  For one ungrouped table, not `DISTINCT` and without `UNION`, the
  columns that read a summary field are read again once the rows are
  sorted and cut. The found set is then the records the query keeps:
  `FETCH` and `OFFSET` cut it, in `ORDER BY` order with a `WHERE` and in
  record order otherwise. That matches q20's `FETCH FIRST 2`, `ORDER BY
  Task FETCH FIRST 2`, `OFFSET`, and `WHERE N > 1 ORDER BY Task` running
  totals.
  - Inferred, not measured: a running total's order under `ORDER BY`
    together with `FETCH` or `OFFSET`; the two observed rules are
    combined.
  - Only plain reads of the table qualify: fields, constants, operators,
    functions and casts. A column with `CASE`, a subquery or an
    aggregate, an `ORDER BY` that reads a summary, joins and grouped
    queries keep the cursor fixed before the rows are made.

  Deviations from q19/q20:
  - a conjunct with a subquery does not narrow (`Task IN (SELECT ...)`
    sees the whole table);
  - `DISTINCT` does not cut the found set;
  - outside the cases above, `FETCH` and `OFFSET` do not cut it, and
    running totals keep record order under `ORDER BY` with a `WHERE`.

**Reading `.fmp12` files** (the `fmp12` feature; file-format.md,
schema-path.md, containers.md, calc-encoding.md; [O] from the sample files):
the field definition's option word (bytes 8-11) has `0x10000` for global
storage, `0x20000` for an unstored calculation and `0x20` for "evaluate even
if all referenced fields are empty"; without `0x20` a calculation is empty
when every field it references is, container fields not counting as empty
(the Contacts `If ( IsEmpty ( Photo ) ; ... )` placeholder evaluates).
Stored calculations are read, never recomputed. Repetition n > 1 of field F
is stored under key F * 256 + n; globals under `<T>/8/<F>`; modification
counts (`252`) as a length byte and a serial key. `TableId` and
`BaseTableId` are the serial-key values of the occurrence and base table
keys (`d0 00 02` = 1065090, `80 01` = 129). Value lists `33/5/<V>`: `16`
name, `3` the custom values (CR-separated), `2` a definition whose first
byte 1 marks a field list (third byte 1: related), followed by the
occurrence and field key strings. An older container reference list in
`FNAM` (`filemac`) has path components with the file name last, length
prefixed. An occurrence whose definition is not `02 01 00` + a local base
table (`02 01 01` for another file) is a missing file.

**Calculation hook.** `om_calc::eval::CalcHost::call_reporting` returns a
value and an error together; the evaluator records the error and keeps the
value, which `ExecuteSQLe` needs (`? ERROR: ...` with 8309).
`om_sql::calc::host_call` answers both functions over any `SqlSource`.

**Open.** The order of U+0131 (`ı`) and U+0152 (`Œ`) against ASCII letters:
`'i' > 'ı'` and `'Œ' < 'p'` in all four files, which UTF-16 order does not
give while every other probe of 6.1 follows it (8 rows of q13). FQL0004
(too many tables) is not raised. `EVERY`/`SOME` are the truth of all/any
non-NULL values.

**Vectors.** 1,625 of the 1,633 rows ran (the 8 `depends=clock`,
`session` and `uninitialized` rows are skipped) and 1,617 pass; the 8
others are the open text order above. Per battery (rows run/passed): q01
182/182, q02 122/122, q03 128/128, q03b 143/143, q04 157/157, q05
190/190, q06 52/52, q07 42/42, q08 31/31, q09 11/11, q10 13/13, q11
82/82, q12 43/43, q13 180/172, q13b 38/38, q13c 14/14, q14 40/40, q15
64/64, q15b 22/22, q15c 23/23, q16 16/16, q17 10/10, q18 22/22.
`cargo test -p om-sql --features fmp12 --test vectors -- --ignored` runs
them (`OM_SQL_RE` locates `re/`).

**Hosts.** `om_data::host::sql::FileSql` is the `SqlSource` over a host's
calculation schema and `CalcSource` (10.3). `om_data::host::FileHost`
(layout calculations and record pages on the desktop client), the desktop
client's script host and OpenMaker Server's script sessions answer both
functions through it; record rules still refuse them when they are
planned (`om_data::rules`). What it settled:

- Catalog (`SqlCatalog`, part of the calculation schema), read from `[3]`,
  each `[T, 3]` and the value lists `[21]` (file-metadata.md 6):
  occurrences in lower-cased name order, those of other files without a
  base table; modification counts as stored under `fc` (occurrence `[3, 11,
  5, O]`, table `[T, 3, 1]`, field `[T, 3, 5, F]`, value list `[21, 5,
  V]`); option `0x20`; value lists of type 0 or without a definition are
  `Custom` (CR-separated lines, dividers kept), type 1 `Field` (the first
  field spec) or, with option `0x1`, `Related`; type 2 reads `External`
  with the row `<Valuelist Unsupported>` [I: no corpus file has one]. A
  field list's `FirstValues` are the first field's non-empty values once
  each, text in UTF-16 order (as the local source) and other types by
  value (6.4); the second-field options are not applied [open].
  `TableId` and `BaseTableId` are serial values; `FieldId` is still the
  raw key, equal below 128 (file-metadata.md 1 gives the serial value)
  [open].
- Reading: a table's records are its record list. `Cursor` carries its
  found set (`Cursor::found`); the first value read from a record the
  source has not downloaded reads that found set ahead from the record
  (`CalcSource::prefetch`). The desktop provider downloads a scan in op92
  batches of 500 records; a client without the file asks for the whole
  found set in one `calc_data` request (a layout calculation, which never
  waits, needs two rounds: the record list, then the records).
- Lookups (`SqlSource::lookup`, which may now fail) only where a find
  proposes every record SQL's `=` keeps (6.1): a stored, value-indexed
  (`0x80000000`) text field with an English index (21, 77) or a date
  field, a constant of the field's own type, text of one line without
  surrounding white space or `* @ # " ~ ?`, at most 100 characters. The
  desktop provider sends an op86 SimpleFind for `==value`
  (query-program.md 6.4), after a GetKey of the field's value-index counts
  `[T, 01, F, 10]` in the same program (calc-values.md, "Indexed related
  sets"); a find the host refuses, or one the index did not exist for,
  scans instead. A server
  session runs the same find with om-query over the file and its
  transaction's changes. The record being edited (the window's unsaved
  draft, the session's open record) is always a candidate. Number fields
  are scanned: om-collate's number keys cover digit text only, so
  OpenMaker Server's index would miss a stored `3.0` that `= 3` matches.
- Calculation fields evaluate with om-data's calculation host on the
  cursor: the cursor's found set is the source's found set of that base
  table (`Get ( FoundCount )`, `Get ( RecordNumber )`, summaries and
  `GetNthRecord`), the session's variables, empty when every non-container
  reference is empty unless `0x20` is set, empty when the calculation
  fails. A query inside a calculation field a query reads may nest 16
  deep; deeper is error 2 [I], which that calculation reads as empty.
- Summary columns: `om_data::summary` over the cursor's found set, read
  once per found set and query (`SummaryRun`), the value on each row's
  record; a summary that fails reads as empty. `om-data/tests/metadata.rs`
  replays the q19 and q20 rows the cursor models.
- What a query sees: the desktop script host's queries see the window's
  unsaved edits on the current record [I: Claris documents nothing; the
  host's other calculations see them too]; a server session's queries see
  its open record and its open transaction's changes. A server session
  without [Full Access] (privilege sets are not decoded) gets error 9 for
  every query before it is parsed, system tables included (`? ERROR:  (9):
  Access denied` from `ExecuteSQLe`); steps of a script set to run with
  full access privileges query.
- Containers in hosted files: OpenMaker Server's script sessions read a
  container column as the `FNAM` file name of the library member the
  record names (8.1, containers.md "Implementation notes"), as the local
  source does, without loading the data for each row; the desktop client
  still reads the record's library reference (`hex(M)` + U+E000) [open].
- Locale: the calculation schema's (en-US on the desktop client until the
  file's locale record is read, file-metadata.md 2; OpenMaker Server sets
  its stored date order).
- Interruption (2026-10-29, review fix): every 256 rows a scan, join,
  `WHERE` filter, group, aggregate or projection visits, the executor asks
  its source whether to stop (`SqlSource::interrupted`; `FileSql` asks its
  `CalcSource`, a server script session answers from its script's abort
  flag and time limit) and ends the query with engine error 1, `Operation
  aborted` (FQL's own text; FileMaker's behavior when a script is aborted
  inside a query is not observed). Before, a join over three tables of
  300 records ran for minutes after its script was aborted. The parser
  asks too, every 1,024 steps and every 16 tokens prediction reads, and
  ends with the same error 1 before anything runs
  ([#122](https://github.com/OpenMaker-Dev/openmaker/issues/122)): a
  statement of 208 parts of q13's shape stops about 3 ms past a 200 ms
  deadline. Before, parsing it could not be stopped.
- Memory (2026-10-29): the `FROM` references (comma-separated, and chains
  of CROSS JOINs or joins without a condition, which are the same product)
  are the factors of one Cartesian product, made row by row in their
  order (the first reference outermost, as before) and tested with the
  `WHERE` as they are made; only the rows it keeps are held, so a
  selective `WHERE` over a large product takes the memory of its result.
  Before, the whole product was built first. Every intermediate result (a
  table's rows, a join's, the rows the `WHERE` keeps) is also bounded by
  `SqlSource::memory_budget` (`MEMORY_BUDGET`, 256 MiB of row slots by
  default, OpenMaker's: FileMaker keeps results in temporary tables on
  disk, 10.1): past it the query ends with engine error 2, `Memory
  allocation failure`, at once, instead of growing until its script's time
  limit or the server's memory runs out. The abort and time-limit checks
  stay every 256 rows, inside the product and join loops. Row order,
  results and the first error are unchanged (1,617 of the 1,625 vectors
  match, as before); `tests/sql.rs` and `om-server/tests/psos_sql_memory.rs`
  (a filtered three-way product of 8 million rows in a server script stays
  within 128 MB until its time limit; an unfiltered one is error 2 before
  it).

**Vectors (hosted).** An ignored test in `crates/om-provider/tests/sql.rs`
(`the_hosted_sample_answers_as_filemaker_server_and_the_local_file`)
hosts a copy of the FileMaker Server sample on an in-process OpenMaker
Server (with an account record added; the sample has only Guest) and runs
its 1,244 runnable rows through the desktop client's path (calculation
schema and `calc_data` over op90, op92 and op86): all 1,244 equal
FileMaker Server's answers and the local source's, with 33 fetches (44
needs) in 0.6 s in a debug build.
