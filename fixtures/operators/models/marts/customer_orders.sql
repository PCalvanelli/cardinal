-- Every order exactly once, through a left join from customers: each order has one
-- customer (declared reference), and the order key holds among matched rows.
select c.customer_id, c.name, o.order_id, o.amount
from {{ ref('stg_customers') }} c
left join {{ ref('stg_orders') }} o on c.customer_id = o.customer_id
