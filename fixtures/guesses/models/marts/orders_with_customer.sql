select o.order_id, o.amount, c.name as customer_name, c.region
from {{ source('app', 'orders') }} o
join {{ ref('stg_customers') }} c on o.customer_id = c.customer_id
