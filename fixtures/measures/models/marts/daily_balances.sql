select snapshot_date, sum(balance) as total_balance
from {{ source('metrics', 'balances') }}
group by snapshot_date
