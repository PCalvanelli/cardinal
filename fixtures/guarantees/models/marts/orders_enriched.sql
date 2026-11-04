select
    o.order_id,
    o.status,
    c.name as customer_name,
    p.amount
from {{ ref('stg_orders') }} o
left join {{ ref('stg_customers') }} c on o.customer_id = c.customer_id
left join {{ source('shop', 'payments') }} p on o.order_id = p.order_id
