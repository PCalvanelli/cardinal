-- G006: one row per payment, merged on order_id
{{ config(materialized='incremental', unique_key='order_id') }}
select o.order_id, o.updated_at, p.payment_id, p.amount
from {{ source('ledger', 'orders') }} o
join {{ source('ledger', 'payments') }} p on p.order_id = o.order_id
