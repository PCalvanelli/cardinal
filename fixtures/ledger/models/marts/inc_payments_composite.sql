-- no finding: a unique_key wider than the grain is still a key
{{ config(materialized='incremental', unique_key=['payment_id', 'order_id']) }}
select payment_id, order_id, amount
from {{ source('ledger', 'payments') }}
