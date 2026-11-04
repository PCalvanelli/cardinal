-- Revenue to date per currency: a running total over order_date.
with daily as (
    select order_date, currency, sum(amount) as revenue
    from {{ source('metrics', 'orders') }}
    group by order_date, currency
)
select
    order_date,
    currency,
    revenue,
    sum(revenue) over (partition by currency order by order_date) as revenue_to_date
from daily
