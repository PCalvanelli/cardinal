-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
create schema if not exists ledger;
create or replace table ledger.orders (order_id integer, customer_id integer, status varchar, updated_at timestamp);
create or replace table ledger.payments (payment_id integer, order_id integer, amount decimal(10,2), paid_at timestamp);
insert into ledger.orders values (1, 10, 'complete', '2026-01-01'), (2, 10, 'pending', '2026-01-02');
insert into ledger.payments values (100, 1, 5.00, '2026-01-01'), (101, 1, 2.50, '2026-01-01'), (102, 2, 9.00, '2026-01-02');
