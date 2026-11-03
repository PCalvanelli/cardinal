-- G007: two payments on one order can share paid_at
select order_id, payment_id, amount
from {{ source('ledger', 'payments') }}
qualify row_number() over (partition by order_id order by paid_at desc) = 1
