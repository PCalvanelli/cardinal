-- Source tables for the fixture. Run by `cargo xtask fixtures` before dbt.
create schema if not exists shop;
create or replace table shop.customers (customer_id integer, name varchar, updated_at timestamp);
create or replace table shop.addresses (address_id integer, customer_id integer, city varchar);
create or replace table shop.orders (order_id integer, customer_id integer, amount decimal(10,2), _fivetran_deleted boolean, _fivetran_synced timestamp);
create or replace table shop.payments (payment_id integer, customer_id integer, amount decimal(10,2));
create or replace table shop.refunds (refund_id integer, customer_id integer, amount decimal(10,2));
insert into shop.customers values (1, 'Ada', '2026-01-01'), (2, 'Lin', '2026-01-01'), (3, 'Sam', '2026-01-01');
insert into shop.addresses values (1, 1, 'Paris'), (2, 1, 'Lyon'), (3, 2, 'Oslo');
insert into shop.orders values (10, 1, 5.00, false, '2026-01-02'), (11, 1, 7.50, true, '2026-01-02'), (12, 2, 3.00, false, '2026-01-03');
insert into shop.payments values (100, 1, 10.00), (101, 1, 10.00), (102, 2, 3.00);
insert into shop.refunds values (200, 1, 10.00);
