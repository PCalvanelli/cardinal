-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
create schema if not exists shop;
create or replace table shop.orders (order_id integer, customer_id integer, order_date date, status varchar, shipping_fee decimal(10,2));
create or replace table shop.order_items (order_id integer, line_id integer, product_id integer, quantity integer, unit_price decimal(10,2));
create or replace table shop.customers (customer_id integer, name varchar, region varchar, updated_at timestamp);
create or replace table shop.promos (customer_id integer, promo_code varchar, granted_at timestamp);
create or replace table shop.refunds (refund_id integer, order_id integer, amount decimal(10,2));
create or replace table shop.events (order_id integer, kind varchar, occurred_at timestamp);
insert into shop.orders values (1, 10, '2026-01-01', 'complete', 5.00), (2, 10, '2026-01-02', 'complete', 7.50), (3, 11, '2026-01-02', 'pending', 0);
insert into shop.order_items values (1, 1, 100, 2, 3.00), (1, 2, 101, 1, 9.00), (2, 1, 100, 1, 3.00), (3, 1, 102, 4, 1.25);
insert into shop.customers values (10, 'Ada', 'eu', '2026-01-01'), (10, 'Ada L.', 'eu', '2026-02-01'), (11, 'Lin', 'us', '2026-01-01');
insert into shop.promos values (10, 'WELCOME', '2026-01-01'), (10, 'SPRING', '2026-03-01'), (11, 'WELCOME', '2026-01-01');
insert into shop.refunds values (1, 2, 3.00);
insert into shop.events values (1, 'shipped', '2026-01-02'), (1, 'delivered', '2026-01-04');
