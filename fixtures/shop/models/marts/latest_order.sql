select customer_id, order_id, order_date
from {{ ref('stg_orders') }}
where status = 'complete'
