select account_id, sum(balance) as total_balance
from {{ source('metrics', 'balances') }}
group by account_id
