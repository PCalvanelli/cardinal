select order_date, currency, sum(amount) as revenue
from {{ source('metrics', 'orders') }}
group by order_date, currency
