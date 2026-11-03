-- G007 on first_amount and prev_amount; first_status is determined by order_id
select
    p.payment_id,
    first_value(o.status) over (partition by p.order_id order by p.paid_at) as first_status,
    first_value(p.amount) over (partition by p.order_id order by p.paid_at) as first_amount,
    lag(p.amount) over (partition by p.order_id order by p.paid_at) as prev_amount
from {{ source('ledger', 'payments') }} p
join {{ source('ledger', 'orders') }} o on o.order_id = p.order_id
