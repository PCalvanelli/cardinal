select account_id, balance
from {{ source('metrics', 'balances') }}
