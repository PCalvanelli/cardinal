select order_id, status, customer_name
from {{ ref('orders_enriched') }}
