-- G006 (error): unique_key is not an output column
{{ config(materialized='incremental', unique_key='payment_key') }}
select payment_id, amount
from {{ source('ledger', 'payments') }}
