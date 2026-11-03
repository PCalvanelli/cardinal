select
    o.order_id,
    i.line_id,
    o.customer_id,
    o.order_date,
    o.shipping_fee,
    i.line_amount
from {{ ref('stg_orders') }} o
join {{ ref('int_order_items') }} i using (order_id)
