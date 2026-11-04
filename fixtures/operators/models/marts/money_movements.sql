-- G012: two payments of 10.00 by customer 1 become one row.
select customer_id, amount from {{ source('shop', 'payments') }}
union
select customer_id, -amount from {{ source('shop', 'refunds') }}
