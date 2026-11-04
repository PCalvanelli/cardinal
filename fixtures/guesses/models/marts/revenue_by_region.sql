select region, sum(amount) as revenue
from {{ ref('orders_with_customer') }}
group by region
