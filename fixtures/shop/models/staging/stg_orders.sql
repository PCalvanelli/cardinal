select
    order_id,
    customer_id,
    order_date,
    status,
    shipping_fee
from {{ source('shop', 'orders') }}
