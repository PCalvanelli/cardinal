-- G014: stg_orders_raw kept the flag, and this model drops it unfiltered.
select customer_id, sum(amount) as total
from {{ ref('stg_orders_raw') }}
group by customer_id
