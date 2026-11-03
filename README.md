<p align="center">
  <img src="docs/assets/cardinal.svg" width="128" height="128" alt="Cardinal logo: a red cardinal bird">
</p>

<h1 align="center">Cardinal</h1>

<p align="center">
  <a href="https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml"><img src="https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <img src="https://img.shields.io/badge/status-beta-orange" alt="Status: beta">
  <img src="https://img.shields.io/badge/rust-1.94-b7410e?logo=rust" alt="Rust 1.94">
  <img src="https://img.shields.io/badge/built%20for-dbt-FF694B?logo=dbt&logoColor=white" alt="Built for dbt">
</p>

> **Beta (v0.1.0-beta.2).** Output, flags and config may change between beta releases.

Cardinal checks that your dbt models count every row correctly. It reads the SQL dbt
compiles and finds rows that get **repeated** (a join that fans out and inflates a sum)
or **dropped** (an inner join or filter that silently loses rows), before any data moves.
It never connects to your warehouse.

Cardinal checks structure, not meaning. It can tell you revenue counts each order once.
It can't tell you whether revenue should include refunds.

## Get started

Install once. Cardinal builds from source for now, and the first build takes a few
minutes:

```sh
git clone https://github.com/PCalvanelli/cardinal
cd cardinal && cargo install --path crates/cardinal-cli
```

Then, in your dbt project:

```sh
dbt compile && dbt docs generate   # writes target/manifest.json and target/catalog.json
cardinal check
```

Cardinal itself runs in about a second, even on a project with a thousand models. The
dbt commands take as long as they usually do on your project.

### What to expect

On a first run, expect a short report that ends like this. Your models and numbers will
differ:

```
warning[G003]: many-to-many join
 --> models/marts/customer_promos.sql:5:1
  |
5 | join {{ source('shop', 'promos') }} p on o.customer_id = p.customer_id
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ neither stg_orders (customer_id) nor shop.promos (customer_id) is unique
  |
  = help: reduce shop.promos to one row per (customer_id) before joining, or join on its full key (customer_id, promo_code)

Analyzed 214 models in 0.41s: 38 with known grain (18%).
Findings: 0 errors, 1 warning (G003 ×1).
Next: declare grain on 3 sources to cover 97 more models: app.orders, app.users, stripe.charges (see cardinal coverage).
```

Expect three things:

- **Few findings at first.** Cardinal can only reason about a model once it knows the key
  of the tables it reads. It learns keys from your `unique` tests and from the
  declarations below. If a model's grain is unknown, Cardinal reports nothing for it
  rather than guessing.
- **A `Next:` line.** It names the sources whose keys would unlock the most models. Add
  those keys first.
- **Exit code 1 only for errors.** Warnings don't fail the run, so you can run Cardinal
  on an existing project without breaking anything.

Then add the keys it asks for:

```yaml
sources:
  - name: app
    tables:
      - name: orders
        meta:
          cardinal:
            grain: [order_id]     # one row per order_id
```

Run `dbt compile && cardinal check` again, and Cardinal checks every model that reads
`app.orders`.

### Useful next commands

| Command | What it shows |
| --- | --- |
| `cardinal describe MODEL` | The model's grain, what each column belongs to, its joins, and whether it keeps each parent's rows exactly once |
| `cardinal explain G001` | What a finding means, with a broken and a fixed query |
| `cardinal coverage` | Which sources to declare keys on next |
| `cardinal assumptions` | The facts Cardinal took on trust, and whether a dbt test checks each one |

## What it finds

The main error is a sum that counts a value more than once:

```
error[G001]: aggregation fans out
 --> models/marts/daily_revenue.sql:4:24
  |
4 |     sum(line_amount) + sum(shipping_fee) as revenue
  |                        ^^^^^^^^^^^^^^^^^ shipping_fee is determined by (order_id)
  |
 ::: models/intermediate/order_lines.sql:9:1
  |
9 | join {{ ref('int_order_items') }} i using (order_id)
  | ---------------------------------------------------- one-to-many: rows repeat per row of int_order_items
  |
  = help: sum shipping_fee per (order_id) before the join, or read it from a relation at that grain
```

`order_lines` has one row per order line, but `shipping_fee` belongs to the order. Each
fee repeats on every line of its order, so summing it counts the fee once per line.

| Code | Default | Finds |
| --- | --- | --- |
| G001 | error | A sum or count that counts a value more than once because a join repeated it |
| G002 | warning | A declared key that the SQL doesn't guarantee |
| G003 | warning | A many-to-many join |
| G004 | info | Models whose grain is unknown, grouped by cause |
| G005 | warning | A suppression comment that no longer matches anything |
| G006 | warning | An incremental model whose `unique_key` isn't unique |
| G007 | warning | `row_number`, `lag`, `lead` or `first_value` ordered by columns that can tie, so the row picked is arbitrary |
| P001 | error | An `exactly_once` contract that isn't proven |
| P002 | warning | A proven contract that relies on something no test checks |
| E001–E005 | error | SQL Cardinal could not read (missing, unparseable or unsupported) |

To silence one finding, add a comment with a reason:

```sql
sum(shipping_fee) as shipping  -- cardinal: allow(G001) reason: fee is allocated per line upstream
```

## Add it to CI

Record today's findings so CI fails only on new ones:

```sh
cardinal check --update-baseline                   # writes .cardinal/baseline.json; commit it
cardinal check --baseline .cardinal/baseline.json  # in CI
```

Exit codes: 0 when there are no errors, 1 when there is at least one, 2 when Cardinal
itself couldn't run (for example, no manifest). Add `--format sarif --output
cardinal.sarif` for GitHub code scanning, or `--format json` for other tools.

## Telling Cardinal what you know

### Keys

Cardinal reads a table's key from `meta.cardinal.grain`, a `primary_key` constraint, a
`unique` test, or `dbt_utils.unique_combination_of_columns`. A `grain` is a primary key,
so its columns are also taken as non-null. A test with a `where` filter only covers some
rows, so Cardinal ignores it.

### References

An inner join drops rows that have no match. To show that every row matches, declare
that a column's values all exist in another table:

```yaml
sources:
  - name: shop
    tables:
      - name: orders
        columns:
          - name: customer_id
            tests: [not_null]
            meta:
              cardinal:
                references: shop.customers    # or shop.customers.id, or {to: shop.customers, column: id}
```

For several columns, use a list on the table:
`references: [{columns: [a, b], to: "source('shop', 'x')", to_columns: [a, b]}]`.

Cardinal doesn't treat dbt `relationships` tests as declarations. A test only shows that
a declared reference is checked when dbt runs.

### Exactly-once contracts

Declare that a model contains every row of another relation exactly once:

```yaml
models:
  - name: orders_enriched
    meta:
      cardinal:
        exactly_once: stg_orders    # or a list; write sources as source_name.table_name
```

Cardinal reports P001 unless it can prove both halves. **At least once** means no filter,
`LIMIT` or join drops a row. **At most once** means no join repeats a row. If the model
aggregates, Cardinal checks the rows that feed the aggregate. If Cardinal can't prove the
contract, P001 fails it, the way a type checker rejects code it can't verify.

### Assumptions

Every proof starts from facts Cardinal takes on trust: the keys, non-null columns and
references you declared. `cardinal assumptions` lists each one, which models rely on it,
and whether a dbt test (or an enforced constraint) checks it:

```
UNCHECKED  shop.order_items.order_id references shop.orders.order_id
           declared by meta.cardinal.references
           used by daily_revenue, order_lines
```

`cardinal assumptions --emit-tests` prints the dbt tests to add. With those tests running,
dbt checks your sources and Cardinal proves everything built from them.

## Development

```sh
cargo test --workspace                                          # unit, rule and golden tests
cargo test --release -p cardinal-grain --test soundness         # property tests against real query results
cargo test --release -p cardinal-cli --test speed -- --ignored  # speed targets
cargo xtask fixtures                                            # rebuild fixture artifacts (needs dbt-duckdb and duckdb)
```

`fixtures/` holds small dbt projects with their compiled artifacts checked in, so you can
try Cardinal without dbt: `CARDINAL_ALLOW_STALE=1 cardinal check --project-dir
fixtures/shop`. `docs/explain/` has one page per finding code.

```
crates/
  cardinal-cli/     commands, flags, the analysis engine
  cardinal-dbt/     reads manifest and catalog: models, declarations, tests, assumptions
  cardinal-sql/     SQL dialects and planning
  cardinal-grain/   the analysis: keys, inclusion facts, exactly-once verdicts
  cardinal-check/   the checks (G, P and E codes)
  cardinal-diag/    findings, rendering, baseline, suppressions
  cardinal-cache/   fact cache under .cardinal/cache
```

The toolchain is pinned to Rust 1.94 by `rust-toolchain.toml`, and DataFusion to
`=55.2.0`.
