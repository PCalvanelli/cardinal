select customer_id, name, region
from {{ source('app', 'customers') }}
