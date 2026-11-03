select kind, count(*) as n
from {{ ref('order_events') }}
group by kind
