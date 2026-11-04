select order_id, customer_id, status, ordered_at
from {{ source('shop', 'orders') }}
