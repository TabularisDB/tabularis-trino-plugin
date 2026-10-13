CREATE SCHEMA IF NOT EXISTS memory.demo;

DROP VIEW IF EXISTS memory.demo.v_types;
DROP TABLE IF EXISTS memory.demo.types_test;
DROP TABLE IF EXISTS memory.demo.many_rows;

CREATE TABLE memory.demo.types_test AS
SELECT
  CAST('1234567890.123456789' AS DECIMAL(38,9)) AS dec_col,
  CAST(9223372036854775807 AS BIGINT) AS big_col,
  ARRAY[1, 2, 3] AS arr_col,
  MAP(ARRAY['a', 'b'], ARRAY[1, 2]) AS map_col,
  CAST(ROW(1, 'x') AS ROW(id INTEGER, name VARCHAR)) AS row_col,
  CAST(NULL AS VARCHAR) AS null_col;

CREATE TABLE memory.demo.many_rows AS
SELECT CAST(a * 1000 + b + 1 AS BIGINT) AS n
FROM UNNEST(sequence(0, 99)) AS x(a)
CROSS JOIN UNNEST(sequence(0, 999)) AS y(b);

CREATE VIEW memory.demo.v_types AS
SELECT dec_col, big_col FROM memory.demo.types_test;