-- no finding: an expression unique_key is not judged
{{ config(materialized='incremental', unique_key="order_id || '-' || payment_id") }}
select order_id, payment_id, amount
from {{ source('ledger', 'payments') }}
