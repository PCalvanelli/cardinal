-- cardinal: allow(P003) reason: the dashboard shows only the latest orders
select order_id, ordered_at
from {{ ref('stg_orders') }}
order by ordered_at desc
limit 2
