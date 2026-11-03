select
    o.order_id,
    o.status,
    c.name as customer_name
from {{ ref('stg_orders') }} o
join {{ ref('stg_customers') }} c on o.customer_id = c.customer_id
