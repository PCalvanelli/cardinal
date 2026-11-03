select
    cast(ordered_at as date) as order_date,
    sum(line_amount) as revenue
from {{ ref('order_lines') }}
group by 1
