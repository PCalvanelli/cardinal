-- Cardinal's guess: app.customers has one row per customer_id.
-- Why: customer_id is named like the table's id.
-- Why: orders_with_customer joins on it.
-- If it holds, it lets Cardinal check 2 models that depend on it.
-- This test returns every row whose customer_id is null or repeated.
-- Written by `cardinal guess`. Cardinal uses the guess only after dbt passes this test,
-- and stops guessing it if dbt fails it. Keep the file so dbt checks the guess on every run.
{{ config(tags=['cardinal'], meta={'cardinal': {'guess': {"kind":"key","relation":"source.guesses.app.customers","columns":["customer_id"]}}}) }}

select *
from (
    select
        customer_id,
        count(*) over (partition by customer_id) as cardinal_rows
    from {{ source('app', 'customers') }}
) rows_by_key
where customer_id is null or cardinal_rows > 1
