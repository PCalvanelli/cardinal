select
    o.order_id,
    e.kind,
    e.occurred_at
from {{ ref('stg_orders') }} o
join {{ source('shop', 'events') }} e on o.order_id = e.order_id
