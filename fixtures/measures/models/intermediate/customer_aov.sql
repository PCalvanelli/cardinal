-- One row per customer and currency: an average, a count and a total per group.
select
    customer_id,
    currency,
    avg(amount) as avg_order_value,
    count(*) as orders,
    sum(amount) as revenue
from {{ source('metrics', 'orders') }}
group by customer_id, currency
