select order_id, status, ordered_at
from {{ ref('stg_orders') }}
where status <> 'cancelled'
