{{ config(materialized='ephemeral') }}
select
    order_id,
    line_id,
    product_id,
    quantity * unit_price as line_amount
from {{ source('shop', 'order_items') }}
