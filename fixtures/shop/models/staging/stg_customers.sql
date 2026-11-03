-- latest version of each customer
select
    customer_id,
    name,
    region
from {{ source('shop', 'customers') }}
qualify row_number() over (partition by customer_id order by updated_at desc) = 1
