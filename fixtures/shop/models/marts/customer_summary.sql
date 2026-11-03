select
    c.customer_id,
    c.region,
    count(o.order_id) as orders,
    sum(o.shipping_fee) as shipping  -- derive: allow(G001) reason: stale, nothing fans out here
from {{ ref('stg_customers') }} c
left join {{ ref('stg_orders') }} o on c.customer_id = o.customer_id
group by c.customer_id, c.region
