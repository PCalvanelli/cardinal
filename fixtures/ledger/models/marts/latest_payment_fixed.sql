-- no finding: payment_id breaks ties
select order_id, payment_id, amount
from {{ source('ledger', 'payments') }}
qualify row_number() over (partition by order_id order by paid_at desc, payment_id desc) = 1
