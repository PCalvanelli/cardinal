-- one row per order, for the weekly report
select order_id, customer_name
from {{ ref('order_summary') }}
