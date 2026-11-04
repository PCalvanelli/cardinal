-- Cardinal's guess: app.events has one row per event_id.
-- Why: event_id is named like the table's id.
-- If it holds, it lets Cardinal check 2 models that depend on it.
-- This test returns every row whose event_id is null or repeated.
-- Written by `cardinal guess`. Cardinal uses the guess only after dbt passes this test,
-- and stops guessing it if dbt fails it. Keep the file so dbt checks the guess on every run.
{{ config(tags=['cardinal'], meta={'cardinal': {'guess': {"kind":"key","relation":"source.guesses.app.events","columns":["event_id"]}}}) }}

select *
from (
    select
        event_id,
        count(*) over (partition by event_id) as cardinal_rows
    from {{ source('app', 'events') }}
) rows_by_key
where event_id is null or cardinal_rows > 1
