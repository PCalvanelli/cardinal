-- G002 only: the unique test on order_id already reports it
{{ config(materialized='incremental', unique_key='order_id') }}
select order_id, payment_id, amount
from {{ source('ledger', 'payments') }}
