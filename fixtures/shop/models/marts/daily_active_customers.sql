select
    order_date,
    count(distinct customer_id) as active_customers,
    count(distinct order_id) as orders
from {{ ref('stg_orders') }}
group by order_date
