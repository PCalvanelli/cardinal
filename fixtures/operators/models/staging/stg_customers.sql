select customer_id, name
from {{ source('shop', 'customers') }}
