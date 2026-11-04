-- Row-level division is a conversion, not a ratio: these sums are fine.
select
    order_date,
    currency,
    sum(amount / 100) as amount_hundreds,
    count(*) as orders
from {{ source('metrics', 'orders') }}
group by order_date, currency
