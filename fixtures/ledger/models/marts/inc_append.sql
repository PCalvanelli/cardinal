-- no finding: append ignores unique_key
{{ config(materialized='incremental', unique_key='order_id', incremental_strategy='append') }}
select order_id, payment_id, amount
from {{ source('ledger', 'payments') }}
