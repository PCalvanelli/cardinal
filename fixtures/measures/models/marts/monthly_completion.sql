select
    date_trunc('month', order_date) as month,
    sum(completion_rate) as completion_rate
from {{ ref('daily_completion') }}
group by 1
