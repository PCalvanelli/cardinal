select
    order_date,
    sum(active_customers) over (order by order_date) as customers_to_date
from {{ ref('daily_active_customers') }}
