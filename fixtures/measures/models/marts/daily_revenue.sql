select order_date, sum(amount) as revenue
from {{ source('metrics', 'orders') }}
group by order_date
