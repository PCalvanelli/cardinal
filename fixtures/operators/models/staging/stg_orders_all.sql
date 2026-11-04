-- G014: drops _fivetran_deleted without filtering on it.
select order_id, customer_id, amount
from {{ source('shop', 'orders') }}
