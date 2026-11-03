select
    o.order_id,
    i.line_id,
    o.ordered_at,
    i.quantity * i.price as line_amount
from {{ ref('stg_orders') }} o
join {{ source('shop', 'order_items') }} i using (order_id)
