-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
-- app.customers and app.events declare no key; `cardinal guess` proposes one for each.
-- customers is unique on customer_id, so that guess passes; events repeats an event_id
-- (the feed delivers some events twice), so that guess fails.
create schema if not exists app;
create or replace table app.orders (order_id integer, customer_id integer, amount decimal(10,2), ordered_at date);
create or replace table app.customers (customer_id integer, name varchar, region varchar);
create or replace table app.events (event_id integer, order_id integer, kind varchar, occurred_at timestamp);
insert into app.orders values (1, 10, 20.00, '2026-01-01'), (2, 10, 7.50, '2026-01-02'), (3, 11, 12.00, '2026-01-02');
insert into app.customers values (10, 'Ada', 'eu'), (11, 'Lin', 'us'), (12, 'Sam', 'us');
insert into app.events values (100, 1, 'shipped', '2026-01-02'), (101, 1, 'delivered', '2026-01-04'), (101, 1, 'delivered', '2026-01-04'), (102, 2, 'shipped', '2026-01-03');
