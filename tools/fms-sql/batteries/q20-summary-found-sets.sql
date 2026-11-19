# q20: which found set a summary column sees (execute-sql.md 12, S3; after
# q19): joins with conditions on either table, FETCH and OFFSET, OR and NOT,
# DISTINCT, subqueries. File and records as q19 (sumprobe; P_A is another
# occurrence of Tasks). No record context.
SELECT a.Task, a.S_Total, b.S_Total FROM Tasks a JOIN P_A b ON a.N = b.N WHERE a.Status = 'Done'
SELECT a.Task, a.S_Total, b.S_Total FROM Tasks a, P_A b WHERE a.N = b.N AND a.Status = 'Done' AND b.Category = 'Home'
SELECT b.Task, b.S_Total, a.S_Total FROM P_A b JOIN Tasks a ON a.N = b.N WHERE b.Status = 'Done'
SELECT a.Task, a.S_Total, b.S_Total FROM Tasks a, P_A b WHERE a.Status = 'Done' AND b.Status = 'Done'
SELECT a.Task, a.S_Total, b.Task FROM Tasks a LEFT JOIN P_A b ON a.N = b.W WHERE a.Category = 'Home'
SELECT Task, S_Total FROM Tasks FETCH FIRST 2 ROWS ONLY
SELECT Task, S_Total FROM Tasks ORDER BY Task FETCH FIRST 2 ROWS ONLY
SELECT Task, S_Total FROM Tasks OFFSET 2 ROWS FETCH FIRST 2 ROWS ONLY
SELECT Task, S_Total FROM Tasks OFFSET 4 ROWS
SELECT Task, S_Total FROM Tasks WHERE Status = 'Pending' FETCH FIRST 2 ROWS ONLY
SELECT Task, S_RunTotal FROM Tasks FETCH FIRST 3 ROWS ONLY
SELECT Task, S_Total FROM Tasks WHERE NOT (Status = 'Pending')
SELECT Task, S_Total FROM Tasks WHERE Status = 'Done' OR Category = 'Work'
SELECT Task, S_Total FROM Tasks WHERE N > 1 AND W = 1
SELECT Task, S_Total FROM Tasks WHERE N > 1 AND (W = 1 OR Category = 'Home')
SELECT Task, S_Total FROM Tasks WHERE N + W > 5
SELECT Task, S_Total FROM Tasks WHERE UPPER(Task) = 'A' OR N = 5
SELECT Task, S_RunTotal FROM Tasks WHERE N > 1 ORDER BY Task
SELECT DISTINCT Status, S_Total FROM Tasks
SELECT COUNT(*), MAX(S_Total) FROM Tasks WHERE Status = 'Done'
SELECT Task, (SELECT MAX(S_Total) FROM P_A) FROM Tasks WHERE Status = 'Done'
SELECT Task, (SELECT MAX(p.S_Total) FROM P_A p WHERE p.Category = 'Home') FROM Tasks WHERE Status = 'Done'
SELECT Task, S_Total FROM Tasks WHERE Task IN (SELECT Task FROM P_A WHERE N > 3)
