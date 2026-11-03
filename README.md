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

> **Beta (v0.1.0-beta.2).** Cardinal is pre-release software. Findings, flags, the
> cardinal.toml format and the JSON output schema may change between beta releases, and
> the checks have not yet been validated against a corpus of production dbt projects.

A static grain checker for dbt projects. `cardinal check` reads dbt's compiled artifacts,
infers the keys and column determinants of every model, and reports grain errors (fan-out
aggregations, many-to-many joins, unprovable declared keys) before any data moves. It
never connects to a warehouse.

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
  = note: order_lines has grain (order_id, line_id), proven: one-to-many join on order_id; key of shop.order_items from meta.cardinal.grain
  = help: sum shipping_fee per (order_id) before the join, or read it from a relation at that grain
  = see: cardinal explain G001
```

## Getting started

### 1. Install

Cardinal is built from source for now. `rustup` installs the pinned toolchain (Rust 1.94)
automatically on the first build.

```sh
git clone https://github.com/PCalvanelli/cardinal
cd cardinal
cargo install --path crates/cardinal-cli
cardinal --version   # cardinal 0.1.0-beta.2
```

### 2. Try it on the bundled example

`fixtures/shop` is a small dbt project whose compiled artifacts are checked in, so you can
run Cardinal without installing dbt or connecting to a warehouse:

```sh
cardinal check --project-dir fixtures/shop
```

(If it says the manifest is stale, git gave the checked-in files newer timestamps than
the manifest. Run it with `CARDINAL_ALLOW_STALE=1` to skip that check.)

It reports four findings, each planted on purpose, and exits with 1 because one of them
is an error:

```
error[G001]: aggregation fans out
 --> models/marts/daily_revenue.sql:4:24
...
warning[G002]: declared key (customer_id) is not proven by the SQL
 --> models/marts/latest_order.sql
...
Analyzed 12 models in 0.02s: 11 with known grain (92%).
Findings: 1 error, 3 warnings (G001 ×1, G002 ×1, G003 ×1, G005 ×1); 1 below --min-severity, 1 suppressed.
Next: declare grain on 1 source to cover 1 more model: shop.events (see cardinal coverage).
```

To see why the error happens, look at the grain of the model that `daily_revenue` reads:

```sh
cardinal describe order_lines --project-dir fixtures/shop
```

```
grain   (order_id, line_id)
        proven: one-to-many join on order_id; key of shop.order_items from meta.cardinal.grain

column         native grain          nulls
order_id       (order_id)            never
line_id        (order_id, line_id)   possible
shipping_fee   (order_id)            possible
line_amount    (order_id, line_id)   possible
...
```

`order_lines` has one row per order line, but `shipping_fee` belongs to the order, so it
repeats on every line of that order. Summing it per day counts each order's fee once per
line. `cardinal explain G001` prints the full explanation, with a broken and a fixed query.

### 3. Run it on your own project

Cardinal reads `target/manifest.json` (models and compiled SQL) and `target/catalog.json`
(column types), so generate both first:

```sh
cd your-dbt-project
dbt compile && dbt docs generate
cardinal init       # optional: writes cardinal.toml with the detected dialect
cardinal check
cardinal coverage   # which sources to declare keys on to cover the most models
```

Re-run `dbt compile` after you edit models. If the manifest is older than your SQL, Cardinal
reports it instead of checking stale code.

Most models in a new project show unknown grain at first because Cardinal has nothing to
start from. Declare keys on your sources (see [Declaring keys](#declaring-keys)) in the
order `cardinal coverage` lists them, and each one lets Cardinal prove grain for every model
downstream.

### 4. Add it to CI

On an existing project, record today's findings in a baseline so CI fails only on new
ones:

```sh
cardinal check --update-baseline                    # writes .cardinal/baseline.json; commit it
cardinal check --baseline .cardinal/baseline.json     # in CI: exits 1 only on new errors
```

Add `--format sarif --output cardinal.sarif` to upload findings to GitHub code scanning, or
`--format json` for other tools.

### Commands

| Command | What it does |
| --- | --- |
| `cardinal check [SELECT...]` | Analyze the project (or selected models) and print findings |
| `cardinal describe MODEL` | A model's grain, each column's native grain, join cardinalities, evidence |
| `cardinal explain CODE` | Long explanation of a finding, with a broken and a fixed example |
| `cardinal coverage` | Share of models with known grain, and the root causes for the rest |
| `cardinal init` | Write cardinal.toml with the detected dialect and an empty baseline |
| `cardinal watch` | Re-run check whenever target/manifest.json changes |

Exit codes: 0 with no error-severity findings, 1 with at least one, 2 when the tool
itself failed (missing artifacts, unreadable manifest).

## Declaring keys

Cardinal reads keys from, in priority order: `meta.cardinal.grain`, `primary_key` contract
constraints, `unique` tests, and `dbt_utils.unique_combination_of_columns`. `not_null`
tests give nullability facts.

```yaml
sources:
  - name: shop
    tables:
      - name: order_items
        meta:
          cardinal:
            grain: [order_id, line_id]
```

## Checks

| Code | Default | Fires when |
| --- | --- | --- |
| G001 | error | An aggregate counts a value more than once because rows repeat it |
| G002 | warning | A declared key is not proven by the SQL |
| G003 | warning | A join is many-to-many |
| G004 | info | A model's grain is unknown (reported once per root cause) |
| G005 | warning | A suppression comment no longer matches any finding |
| G006 | warning | An incremental model's `unique_key` is not a key of its rows (error if it names a column the model does not output) |
| G007 | warning | `row_number`, `lag`, `lead` or `first_value` is ordered by columns that can tie, so the row it picks is arbitrary |
| E001–E005 | error | Missing compiled SQL, parse error, unresolved function, unresolved relation or column, unsupported SQL |

Suppress a finding with a SQL comment and a reason:

```sql
sum(shipping_fee) as shipping  -- cardinal: allow(G001) reason: fee is allocated per line upstream
```

Adopt on an existing project with a baseline, so CI fails only on new findings:

```sh
cardinal check --update-baseline
cardinal check --baseline .cardinal/baseline.json --format sarif --output cardinal.sarif
```

## Workspace

```
crates/
  cardinal-cli/     binary: commands, flags, exit codes, engine, synthetic projects
  cardinal-dbt/     manifest and catalog loading, DAG, declared keys, selection
  cardinal-sql/     dialects, SqlToRel context provider, function stubs, spans
  cardinal-grain/   RelationFacts, closure, one rule per plan node, summaries
  cardinal-check/   Check trait, G001 to G004, E-codes
  cardinal-diag/    Diagnostic, source mapping, renderers, baseline, suppressions
  cardinal-cache/   fact cache under .cardinal/cache
fixtures/         small dbt projects with compiled artifacts checked in
docs/explain/     one page per finding code
xtask/            fixture regeneration, new-check scaffolding, corpus runs, synth projects
```

## Development

```sh
cargo test --workspace                                        # unit, rule and golden tests
cargo test --release -p cardinal-grain --test soundness         # soundness property tests
cargo test --release -p cardinal-cli --test speed -- --ignored  # speed targets
cargo bench -p cardinal-cli                                     # criterion benchmarks
cargo xtask fixtures                                          # regenerate fixture artifacts (needs dbt-duckdb, duckdb)
cargo xtask new-check G006                                    # scaffold a check
cargo xtask corpus ../some-dbt-project ...                    # corpus statistics
```

The toolchain is pinned to Rust 1.94 by `rust-toolchain.toml`, the minimum DataFusion
55 supports. DataFusion is pinned to `=55.2.0` and upgraded deliberately.
