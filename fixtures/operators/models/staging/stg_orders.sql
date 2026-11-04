-- Deleted orders filtered out: no G014.
select order_id, customer_id, amount
from {{ source('shop', 'orders') }}
where not coalesce(_fivetran_deleted, false)
