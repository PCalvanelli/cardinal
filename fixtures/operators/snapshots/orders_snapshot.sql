{% snapshot orders_snapshot %}
{{ config(target_schema='snapshots', unique_key='order_id', strategy='check', check_cols=['amount']) }}
select order_id, customer_id, amount
from {{ ref('stg_orders') }}
{% endsnapshot %}
