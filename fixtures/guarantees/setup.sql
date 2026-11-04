-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
create schema if not exists shop;
create or replace table shop.customers (customer_id integer, name varchar);
create or replace table shop.orders (order_id integer, customer_id integer, status varchar, ordered_at timestamp);
create or replace table shop.order_items (order_id integer, line_id integer, product_id integer, quantity integer);
create or replace table shop.products (product_id integer, name varchar);
create or replace table shop.payments (payment_id integer, order_id integer, amount decimal(10,2));
create or replace table shop.promos (customer_id integer, promo_code varchar);
insert into shop.customers values (10, 'Ada'), (11, 'Lin');
insert into shop.orders values (1, 10, 'completed', '2026-01-01 09:00'), (2, 10, 'cancelled', '2026-01-02 10:00'), (3, 11, 'completed', '2026-01-02 11:00');
insert into shop.order_items values (1, 1, 100, 2), (1, 2, 101, 1), (2, 1, 100, 1), (3, 1, 102, 4);
insert into shop.products values (100, 'mug'), (101, 'tea');
insert into shop.payments values (500, 1, 10.00), (501, 1, 5.00), (502, 3, 2.50);
insert into shop.promos values (10, 'WELCOME'), (10, 'SPRING'), (11, 'WELCOME');
