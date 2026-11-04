-- G013: customers with no orders count 1.
select c.customer_id, count(*) as order_count
from {{ ref('stg_customers') }} c
left join {{ ref('stg_orders') }} o on c.customer_id = o.customer_id
group by c.customer_id
