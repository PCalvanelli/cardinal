-- Keeps the flag for later models: no G014 here.
select *
from {{ source('shop', 'orders') }}
