select
    o.order_id,
    p.payment_id,
    p.amount
from {{ ref('stg_orders') }} o
join {{ source('shop', 'payments') }} p on o.order_id = p.order_id
