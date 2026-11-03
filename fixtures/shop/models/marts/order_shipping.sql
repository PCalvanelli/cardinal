select
    order_id,
    sum(shipping_fee) as shipping  -- derive: allow(G001) reason: fee is allocated per line upstream
from {{ ref('order_lines') }}
group by order_id
