# q09: summary field and global containers. File: MBS example "Swiss
# QR-Code for invoices ISO 20022" (base_test: ID, id_stat_max (summary, maximum
# of an empty field per calc-values.md U14), Swiss_QR (binary), QR_code_js and
# QR_code_index_html (global text), QR_code_filename (calculated); 1 record.
# "Muster QR Zahlteil": 20 records). No record context.
SELECT ID, id_stat_max FROM base_test
SELECT id_stat_max FROM base_test
SELECT COUNT(id_stat_max), MAX(id_stat_max) FROM base_test
SELECT ID FROM base_test WHERE id_stat_max IS NULL
SELECT * FROM base_test
SELECT LENGTH(QR_code_js), LENGTH(QR_code_index_html) FROM base_test
SELECT QR_code_filename, QR_code_size, Swiss_QR_version FROM base_test
SELECT Swiss_QR, textfile_container, QR_code_container, Cross FROM base_test
SELECT COUNT(*) FROM "Muster QR Zahlteil"
=ExecuteSQL ( "SELECT FieldName, FieldType FROM FileMaker_Fields WHERE TableName = 'Muster QR Zahlteil'" ; "|" ; "¶" )
SELECT * FROM "Muster QR Zahlteil" FETCH FIRST 3 ROWS ONLY
