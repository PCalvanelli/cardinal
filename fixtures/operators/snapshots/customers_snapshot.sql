{% snapshot customers_snapshot %}
{{ config(target_schema='snapshots', unique_key='customer_id', strategy='timestamp', updated_at='updated_at') }}
-- G006: several addresses per customer, so customer_id repeats.
select c.customer_id, c.name, a.city, c.updated_at
from {{ source('shop', 'customers') }} c
join {{ source('shop', 'addresses') }} a on c.customer_id = a.customer_id
{% endsnapshot %}
