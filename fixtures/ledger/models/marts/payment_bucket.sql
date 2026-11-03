-- G007: a truncated timestamp does not break ties even where the raw one might
select payment_id,
    row_number() over (partition by order_id order by date_trunc('day', paid_at), payment_id) as fixed_rn,
    row_number() over (partition by order_id order by date_trunc('day', paid_at)) as day_rn
from {{ source('ledger', 'payments') }}
