select
    i.order_id,
    i.line_id,
    i.quantity,
    p.name as product_name
from {{ source('shop', 'order_items') }} i
join {{ source('shop', 'products') }} p on i.product_id = p.product_id
