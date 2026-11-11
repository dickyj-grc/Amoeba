-- Demo data for the finance-db example package.
CREATE TABLE customers (
  id int PRIMARY KEY,
  name text NOT NULL,
  nik text,
  balance numeric(14, 2),
  org_id text NOT NULL
);
CREATE TABLE loans (
  id serial PRIMARY KEY,
  customer_id int REFERENCES customers (id),
  branch text NOT NULL,
  status text NOT NULL,
  amount numeric(14, 2) NOT NULL,
  due_date date NOT NULL
);
INSERT INTO customers VALUES
  (1, 'Ani Wijaya', '3174012345678901', 15000000.00, 'org_bank_a'),
  (2, 'Budi Santoso', '3174019876543210', 2500000.00, 'org_bank_a'),
  (3, 'Citra Lestari', '3275011111222233', 990000.00, 'org_bank_b');
INSERT INTO loans (customer_id, branch, status, amount, due_date) VALUES
  (1, 'JKT-01', 'overdue', 12500000, '2026-09-30'),
  (2, 'JKT-01', 'overdue', 4000000, '2026-10-15'),
  (2, 'JKT-01', 'current', 1000000, '2026-12-31'),
  (3, 'JKT-01', 'overdue', 7000000, '2026-10-01');

-- Accounts: the service reads with a read-only role and writes with one that
-- can only update customers. Put each in its own DATABASE_URL_READ/_WRITE, or
-- use one DATABASE_URL for a quick demo.
-- CREATE ROLE finance_reader LOGIN PASSWORD '...';
-- GRANT SELECT ON customers, loans TO finance_reader;
-- CREATE ROLE finance_writer LOGIN PASSWORD '...';
-- GRANT SELECT, UPDATE ON customers TO finance_writer;
