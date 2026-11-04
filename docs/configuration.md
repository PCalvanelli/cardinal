# Telling Cardinal what you know

Everything goes under `meta.cardinal` in your dbt YAML.

## Keys

Cardinal reads a key from a `unique` test, `dbt_utils.unique_combination_of_columns`, a
`primary_key` constraint, or `grain`:

```yaml
sources:
  - name: app
    tables:
      - name: orders
        meta:
          cardinal:
            grain: [order_id]     # one row per order_id; also means order_id is never null
```

A test with a `where` filter covers only some rows, so it doesn't count.

## Guessed keys

Don't know a table's key? Let Cardinal guess and dbt check:

```sh
cardinal guess                    # writes a dbt test per guess to tests/cardinal/
dbt test --select tag:cardinal    # dbt checks each guess against your data
cardinal check                    # guesses that passed now count, labeled as guesses
```

Cardinal guesses from column names (`id`, `customer_id`) and from the columns other
models join on. A guess dbt fails is never used or guessed again, and `cardinal
coverage` shows what dbt found. Keep the tests so dbt keeps checking the guesses, and
commit `.cardinal/guesses.json`, which records the results for CI.

## References

An inner join drops rows that have no match. To show that every row has one, declare
where a column's values come from:

```yaml
columns:
  - name: customer_id
    tests: [not_null]
    meta:
      cardinal:
        references: shop.customers    # or shop.customers.id
```

dbt `relationships` tests don't declare a reference; they only check one you declared.

## Exactly-once contracts

Declare that a model holds every row of another one exactly once:

```yaml
models:
  - name: orders_enriched
    meta:
      cardinal:
        exactly_once: stg_orders    # sources as source_name.table_name
```

Cardinal fails the contract (P001) unless it can prove that no filter or join drops a
row, and no join repeats one.

## Measures and units

Cardinal already knows that averages, ratios, percentiles, distinct counts and running
totals can't simply be added up (G008). Tell it about columns that already hold such
numbers, and about amounts in different currencies or units (G009):

```yaml
columns:
  - name: balance
    meta:
      cardinal:
        measure: {kind: stock, over: [snapshot_date]}   # don't add across dates
  - name: conversion_rate
    meta:
      cardinal:
        measure: {kind: non_additive}                   # don't add up at all
  - name: amount
    meta:
      cardinal:
        unit: currency                                  # the column holding amount's unit
```

## Checking what Cardinal trusts

Every proof starts from the keys, non-null columns and references above.
`cardinal assumptions` lists each one and whether a dbt test checks it;
`cardinal assumptions --emit-tests` prints the missing tests as YAML.
