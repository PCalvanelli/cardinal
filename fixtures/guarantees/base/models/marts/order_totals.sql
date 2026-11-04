select
    o.order_id,
    sum(p.amount) as paid
from {{ ref('stg_orders') }} o
join {{ source('shop', 'payments') }} p on o.order_id = p.order_id
group by o.order_id
