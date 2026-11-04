-- Cardinal's guess: every app.orders.customer_id is in app.customers.customer_id.
-- Why: orders_with_customer joins app.orders to app.customers on it, and drops the rows that have no match.
-- If it holds, it lets Cardinal prove orders_with_customer keeps every row of app.orders.
-- This test returns every row whose customer_id is not null and has no match in app.customers.
-- Written by `cardinal guess`. Cardinal uses the guess only after dbt passes this test,
-- and stops guessing it if dbt fails it. Keep the file so dbt checks the guess on every run.
{{ config(tags=['cardinal'], meta={'cardinal': {'guess': {"kind":"references","relation":"source.guesses.app.orders","columns":["customer_id"],"to":"source.guesses.app.customers","to_columns":["customer_id"]}}}) }}

select child.customer_id
from {{ source('app', 'orders') }} child
left join {{ source('app', 'customers') }} parent
    on child.customer_id = parent.customer_id
where child.customer_id is not null
    and parent.customer_id is null
