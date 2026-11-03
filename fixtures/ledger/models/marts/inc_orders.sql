-- no finding: one row per order, merged on order_id
{{ config(materialized='incremental', unique_key='order_id') }}
select order_id, customer_id, status, updated_at
from {{ source('ledger', 'orders') }}
