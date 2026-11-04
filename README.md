<p align="center">
  <img src="docs/assets/cardinal.svg" width="128" height="128" alt="Cardinal logo: a red cardinal bird">
</p>

<h1 align="center">Cardinal</h1>

<p align="center"><b>Proves your dbt models count every row exactly once, before anything runs.</b></p>

<p align="center">
  <a href="https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml"><img src="https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <img src="https://img.shields.io/badge/status-beta-orange" alt="Status: beta">
  <img src="https://img.shields.io/badge/rust-1.94-b7410e?logo=rust" alt="Rust 1.94">
  <img src="https://img.shields.io/badge/built%20for-dbt-FF694B?logo=dbt&logoColor=white" alt="Built for dbt">
</p>

> **Beta (v0.1.0-beta.3).** Output, flags and config may change between beta releases.

Cardinal reads the SQL that dbt compiles and tells you, before anything runs, where a
model can count a row twice, lose a row, or add up numbers that don't add up. It never
connects to your warehouse.

```
error[G001]: a join repeats values that are then added up
 --> models/marts/daily_revenue.sql:4:24
  |
4 |     sum(line_amount) + sum(shipping_fee) as revenue
  |                        ^^^^^^^^^^^^^^^^^ shipping_fee has one value per (order_id)
  |
 ::: models/intermediate/order_lines.sql:9:1
  |
9 | join {{ ref('int_order_items') }} i using (order_id)
  | ---------------------------------------------------- one-to-many: rows repeat per row of int_order_items
  |
  = help: sum shipping_fee per (order_id) before the join, or read it from a table with one row per (order_id)
```

`order_lines` has one row per order line, but each order has one shipping fee. The join
copies the fee onto every line, so the sum counts it once per line.

Cardinal checks structure, not meaning: it can tell you revenue counts each order once,
not whether revenue should include refunds.

## Get started

Install (builds from source; the first build takes a few minutes):

```sh
git clone https://github.com/PCalvanelli/cardinal
cd cardinal && cargo install --path crates/cardinal-cli
```

In your dbt project:

```sh
dbt compile && dbt docs generate   # writes target/manifest.json and target/catalog.json
cardinal check
```

Cardinal takes about a second, even on a thousand models. The first run usually ends
like this:

```
Analyzed 214 models in 0.41s: 38 with a known key (18%).
Findings: 0 errors, 1 warning (G003 ×1).
Next: 3 sources without a known key keep 97 models from being checked: app.orders, app.users, stripe.charges. Run cardinal guess so dbt can test likely keys, or declare them (see cardinal coverage).
```

- **Few findings at first.** Cardinal needs to know each table's **key**: the columns
  that identify one row, such as one row per `order_id`. It learns keys from your
  `unique` tests and the declarations below. Where it doesn't know a key, it stays quiet
  rather than guess.
- **The `Next:` line** names the keys that would let Cardinal check the most models.
- **Only errors fail the run** (exit code 1). Warnings don't, so you can add Cardinal to
  an existing project without breaking anything. Exit code 2 means Cardinal itself
  couldn't run, for example with no manifest.

| Command | What it shows |
| --- | --- |
| `cardinal describe MODEL` | A model's key, what each column belongs to, its joins, and whether it keeps each parent's rows exactly once |
| `cardinal explain G001` | What a finding means, with a broken and a fixed query |
| `cardinal coverage` | Which keys to declare next |
| `cardinal guess` | Likely keys, written as dbt tests for dbt to check |
| `cardinal assumptions` | The facts Cardinal takes on trust, and whether a dbt test checks each one |

## Add it to CI

Compare each pull request with main. If you use dbt's Slim CI, you already download
main's artifacts for `state:modified`; point Cardinal at the same directory:

```sh
dbt compile && dbt docs generate
cardinal check --state prod-artifacts/   # or set DBT_STATE
```

Whatever Cardinal proves on main becomes a check, with nothing to declare. If a model
kept each order exactly once on main, or had one row per `order_id`, and the branch
breaks that, the run fails with P003. Findings main already has don't fail the run:

```
Compared with main (state: prod-artifacts/): 1 guarantee lost, 2 gained, 41 kept; 3 findings already on main, not shown.
```

The directory needs main's `manifest.json`; `catalog.json` is optional.

Without main's artifacts, commit a baseline of today's findings instead, and CI fails
only on new ones:

```sh
cardinal check --update-baseline                   # writes .cardinal/baseline.json
cardinal check --baseline .cardinal/baseline.json  # in CI
```

Add `--format sarif --output cardinal.sarif` for GitHub code scanning, or `--format json`.

## Telling Cardinal what you know

Everything goes under `meta.cardinal` in your dbt YAML.

### Keys

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

### Guessed keys

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

### References

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

### Exactly-once contracts

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

### Measures and units

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

### Checking what Cardinal trusts

Every proof starts from the keys, non-null columns and references above.
`cardinal assumptions` lists each one and whether a dbt test checks it;
`cardinal assumptions --emit-tests` prints the missing tests as YAML.

## What it finds

| Code | Default | Finds |
| --- | --- | --- |
| G001 | error | A join repeats a value, and a sum or count then adds it more than once |
| G002 | warning | A declared key that the SQL doesn't guarantee |
| G003 | warning | A many-to-many join |
| G004 | info | Models Cardinal can't check because a key is unknown, grouped by cause |
| G005 | warning | A suppression comment that no longer matches anything |
| G006 | warning | An incremental model whose `unique_key` isn't unique |
| G007 | warning | `row_number`, `lag`, `lead` or `first_value` ordered by columns that can tie, so the row picked is random |
| G008 | warning | Adds up a number that can't be added: overlapping distinct counts (daily active users summed into monthly), averages, ratios, percentiles, or balances and running totals across dates |
| G009 | error | Adds amounts in different units, such as `amount` across currencies (needs `unit`) |
| P001 | error | An `exactly_once` contract that isn't proven |
| P002 | warning | A proven contract that relies on something no test checks |
| P003 | error | A guarantee main has that the branch loses (with `--state`) |
| E001–E005 | error | SQL Cardinal couldn't read (missing, unparseable or unsupported) |

To silence one finding, add a comment with a reason:

```sql
sum(shipping_fee) as shipping  -- cardinal: allow(G001) reason: fee is allocated per line upstream
```

## How it works

Cardinal doesn't run your SQL against data. It runs it against facts about the data:
which columns identify a row, which columns belong to which entity, which are never null,
which rows are sure to find a match, and which numbers can't be added. The facts start
from your dbt tests and `meta.cardinal` declarations. Every join, filter, aggregate and
window updates them, and they flow from each model to the models that read it.

A finding means the facts can't rule a miscount out. A proof means it can't happen on
any data that passes the tests it relies on.

## Development

```sh
cargo test --workspace                                          # unit, rule and golden tests
cargo test --release -p cardinal-grain --test soundness         # property tests against real query results
cargo test --release -p cardinal-cli --test speed -- --ignored  # speed targets
cargo xtask fixtures                                            # rebuild fixture artifacts (needs dbt-duckdb and duckdb)
```

`fixtures/` holds small dbt projects with compiled artifacts checked in, so you can try
Cardinal without dbt: `CARDINAL_ALLOW_STALE=1 cardinal check --project-dir fixtures/shop`.
`docs/explain/` has one page per finding code. The toolchain is pinned to Rust 1.94, and
DataFusion to `=55.2.0`.
