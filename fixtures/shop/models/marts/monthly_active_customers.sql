select
    date_trunc('month', order_date) as order_month,
    sum(active_customers) as active_customers,
    sum(orders) as orders
from {{ ref('daily_active_customers') }}
group by 1
