-- A UNION of identifiers is a deliberate de-duplication: no G012.
select customer_id from {{ source('shop', 'payments') }}
union
select customer_id from {{ source('shop', 'refunds') }}
