select o.order_id, p.promo_code
from {{ ref('stg_orders') }} o
join {{ source('shop', 'promos') }} p on o.customer_id = p.customer_id
