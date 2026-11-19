# q12: statements other than SELECT through ExecuteSQL. File: FileMaker
# Server sample (en), no record context; the harness works on a private copy
# of the file, so a statement that did change data would show in the COUNT
# and SELECT probes after it.
SELECT COUNT(*) FROM Tasks
INSERT INTO Tasks (Task) VALUES ('x')
SELECT COUNT(*) FROM Tasks
INSERT INTO Tasks VALUES (99, 'x')
INSERT INTO Tasks DEFAULT VALUES
INSERT INTO Tasks (Task) SELECT Task FROM Tasks
UPDATE Tasks SET Task = 'x' WHERE "TASK ID MATCH FIELD" = 3
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
UPDATE Tasks SET Taskz = 'x'
DELETE FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
DELETE FROM Tasks
SELECT COUNT(*) FROM Tasks
CREATE TABLE NewT (a VARCHAR(10), b INT)
SELECT COUNT(*) FROM NewT
DROP TABLE Assignees
SELECT COUNT(*) FROM Assignees
ALTER TABLE Tasks ADD Extra VARCHAR
ALTER TABLE Tasks DROP Task
CREATE INDEX ON Tasks.Task
DROP INDEX ON Tasks.Task
TRUNCATE TABLE Assignees
DECLARE c CURSOR FOR SELECT * FROM Tasks
OPEN c
FETCH NEXT FROM c
CLOSE c
DEALLOCATE c
SELECT * FROM Tasks FOR UPDATE
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = 3 FOR UPDATE OF Task
UPDATE Tasks SET Task = 'x' WHERE CURRENT OF c
DELETE FROM FileMaker_Tables
INSERT INTO FileMaker_Tables (TableName) VALUES ('x')
COMMIT
ROLLBACK
GRANT SELECT ON Tasks TO PUBLIC
CREATE VIEW v AS SELECT * FROM Tasks
WITH x AS (SELECT 1 FROM Tasks) SELECT * FROM x
EXPLAIN SELECT * FROM Tasks
SELECT COUNT(*) FROM Tasks
SELECT Task FROM Tasks WHERE "TASK ID MATCH FIELD" = 3
=EvaluationError ( ExecuteSQL ( "DELETE FROM Tasks" ; "" ; "" ) )
=EvaluationError ( ExecuteSQL ( "INSERT INTO Tasks (Task) VALUES ('x')" ; "" ; "" ) )
=EvaluationError ( ExecuteSQL ( "CREATE TABLE NewT (a VARCHAR(10))" ; "" ; "" ) )
=ExecuteSQL ( "DELETE FROM Tasks" ; "" ; "" )
