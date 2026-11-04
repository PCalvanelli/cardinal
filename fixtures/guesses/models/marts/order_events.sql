select o.order_id, o.amount, e.event_id, e.kind
from {{ source('app', 'orders') }} o
join {{ ref('stg_events') }} e on e.order_id = o.order_id
