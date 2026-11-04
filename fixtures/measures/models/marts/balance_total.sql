select sum(balance) as total_balance
from {{ ref('account_balances') }}
