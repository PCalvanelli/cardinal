select event_id, order_id, kind, occurred_at
from {{ source('app', 'events') }}
