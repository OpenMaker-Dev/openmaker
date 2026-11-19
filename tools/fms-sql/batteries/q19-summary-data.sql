# q19: summary fields over data (execute-sql.md 12, S3). File: sumprobe, the
# file-metadata.md probe (tools/file_metadata_probe.py --no-triggers
# --find-fields on the Tasks template) after the setup script of
# tools/fms-script-harness s16-client-finds ran: Tasks has six records in
# creation order (Task, N, W, Status, Category): e 5 1 Pending Work,
# a 1 2 Pending Home, d 4 3 Done Work, b 2 1 Done Home, (empty) - 1 Pending
# Home, c 3 2 Pending Work. Summaries of file-metadata.md 5.2 (S_Total ...
# S_TotalReps), unstored U = N * 2 and UT = Task & " " & Status. No record
# context.
SELECT Task, N, S_Total FROM Tasks
SELECT Task, S_Total FROM Tasks WHERE Status = 'Done'
SELECT Task, S_Total FROM Tasks WHERE N > 2
SELECT Task, S_RunTotal, S_RunCount FROM Tasks
SELECT Task, S_RunTotal, S_RunCount FROM Tasks ORDER BY N DESC
SELECT Task, S_RunTotal, S_RunCount FROM Tasks WHERE Category = 'Work'
SELECT Task, S_Frac, S_FracSub FROM Tasks
SELECT Task, S_Frac FROM Tasks WHERE Status = 'Done'
SELECT S_Avg, S_WAvg, S_Count, S_Min, S_Max, S_SD, S_SDP FROM Tasks FETCH FIRST 1 ROWS ONLY
SELECT S_Avg, S_WAvg, S_Count, S_Min, S_Max FROM Tasks WHERE Status = 'Pending' FETCH FIRST 1 ROWS ONLY
SELECT S_List FROM Tasks FETCH FIRST 1 ROWS ONLY
SELECT S_List FROM Tasks WHERE Category = 'Home' FETCH FIRST 1 ROWS ONLY
SELECT Task, S_TotalReps FROM Tasks
SELECT COUNT(S_Total), MAX(S_Total), SUM(S_Total) FROM Tasks
SELECT Task FROM Tasks WHERE S_Total = 15
SELECT Task FROM Tasks WHERE S_Total IS NULL
SELECT Task, S_Total FROM Tasks WHERE S_Total = 6
SELECT a.Task, b.S_Total FROM Tasks a JOIN P_A b ON a.N = b.N
SELECT a.Task, b.S_Total FROM Tasks a JOIN P_A b ON a.N = b.N WHERE b.Status = 'Done'
SELECT Status, MAX(S_Total) FROM Tasks GROUP BY Status
SELECT Task, S_Total FROM Tasks WHERE Task IN ('a', 'b') OR N = 5
SELECT Task, U, UT FROM Tasks
SELECT Task FROM Tasks WHERE U > 5
