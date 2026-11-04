select
    date_trunc('month', snapshot_date) as month,
    avg(total_balance) as average_balance,
    max(total_balance) as peak_balance
from {{ ref('daily_balances') }}
group by 1
