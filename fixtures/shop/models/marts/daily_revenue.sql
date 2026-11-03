select
    order_date,
    count(*) as line_count,
    sum(line_amount) + sum(shipping_fee) as revenue
from {{ ref('order_lines') }}
group by order_date
