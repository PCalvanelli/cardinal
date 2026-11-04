-- A ratio per day: completed orders over all orders.
select
    order_date,
    sum(case when status = 'complete' then 1 else 0 end) / count(*) as completion_rate
from {{ source('metrics', 'orders') }}
group by order_date
