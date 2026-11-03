-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
create schema if not exists shop;
create or replace table shop.customers (customer_id integer, name varchar, updated_at timestamp);
create or replace table shop.orders (order_id integer, customer_id integer, status varchar, ordered_at timestamp);
create or replace table shop.order_items (order_id integer, line_id integer, quantity integer, price decimal(10,2));
create or replace table shop.payments (payment_id integer, order_id integer, amount decimal(10,2));
insert into shop.customers values (10, 'Ada', '2026-01-01'), (10, 'Ada L.', '2026-02-01'), (11, 'Lin', '2026-01-01');
insert into shop.orders values (1, 10, 'completed', '2026-01-01 09:00'), (2, 10, 'pending', '2026-01-02 10:00'), (3, 11, 'completed', '2026-01-02 11:00');
insert into shop.order_items values (1, 1, 2, 3.00), (1, 2, 1, 9.00), (2, 1, 1, 3.00), (3, 1, 4, 1.25);
insert into shop.payments values (100, 1, 15.00), (101, 3, 2.50), (102, 3, 2.50);
