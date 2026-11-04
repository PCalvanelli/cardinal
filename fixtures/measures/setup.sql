-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
create schema if not exists metrics;
create or replace table metrics.orders (order_id integer, customer_id integer, order_date date, status varchar, amount decimal(10,2), currency varchar);
create or replace table metrics.customers (customer_id integer, region varchar);
create or replace table metrics.balances (account_id integer, snapshot_date date, balance decimal(12,2));
insert into metrics.orders values (1, 10, '2026-01-01', 'complete', 20.00, 'USD'), (2, 10, '2026-01-02', 'pending', 15.00, 'EUR'), (3, 11, '2026-01-02', 'complete', 3000, 'JPY');
insert into metrics.customers values (10, 'eu'), (11, 'apac');
insert into metrics.balances values (1, '2026-01-01', 100.00), (1, '2026-01-02', 120.00), (2, '2026-01-01', 50.00);
