select
    currency,
    sum(revenue_to_date) as total_to_date,
    max(revenue_to_date) as latest_to_date
from {{ ref('running_revenue') }}
group by currency
