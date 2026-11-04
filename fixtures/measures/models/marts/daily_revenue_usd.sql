select order_date, sum(amount) as revenue_usd
from {{ source('metrics', 'orders') }}
where currency = 'USD'
group by order_date
