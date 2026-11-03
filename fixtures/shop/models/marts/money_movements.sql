select order_id as id, 'order' as kind, shipping_fee as amount
from {{ ref('stg_orders') }}
union all
select refund_id as id, 'refund' as kind, -amount as amount
from {{ source('shop', 'refunds') }}
