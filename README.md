# Derive

> **Beta (v0.1.0-beta.1).** Derive is pre-release software. Findings, flags, the
> derive.toml format and the JSON output schema may change between beta releases, and
> the checks have not yet been validated against a corpus of production dbt projects.

A static grain checker for dbt projects. `derive check` reads dbt's compiled artifacts,
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
  = note: order_lines has grain (order_id, line_id), proven: one-to-many join on order_id; key of shop.order_items from meta.derive.grain
  = help: sum shipping_fee per (order_id) before the join, or read it from a relation at that grain
  = see: derive explain G001
```

## Quick start

```sh
cargo install --path crates/derive-cli
cd your-dbt-project
dbt compile && dbt docs generate
derive check
```

| Command | What it does |
| --- | --- |
| `derive check [SELECT...]` | Analyze the project (or selected models) and print findings |
| `derive describe MODEL` | A model's grain, each column's native grain, join cardinalities, evidence |
| `derive explain CODE` | Long explanation of a finding, with a broken and a fixed example |
| `derive coverage` | Share of models with known grain, and the root causes for the rest |
| `derive init` | Write derive.toml with the detected dialect and an empty baseline |
| `derive watch` | Re-run check whenever target/manifest.json changes |

Exit codes: 0 with no error-severity findings, 1 with at least one, 2 when the tool
itself failed (missing artifacts, unreadable manifest).

## Declaring keys

Derive reads keys from, in priority order: `meta.derive.grain`, `primary_key` contract
constraints, `unique` tests, and `dbt_utils.unique_combination_of_columns`. `not_null`
tests give nullability facts.

```yaml
sources:
  - name: shop
    tables:
      - name: order_items
        meta:
          derive:
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
| E001–E005 | error | Missing compiled SQL, parse error, unresolved function, unresolved relation or column, unsupported SQL |

Suppress a finding with a SQL comment and a reason:

```sql
sum(shipping_fee) as shipping  -- derive: allow(G001) reason: fee is allocated per line upstream
```

Adopt on an existing project with a baseline, so CI fails only on new findings:

```sh
derive check --update-baseline
derive check --baseline .derive/baseline.json --format sarif --output derive.sarif
```

## Workspace

```
crates/
  derive-cli/     binary: commands, flags, exit codes, engine, synthetic projects
  derive-dbt/     manifest and catalog loading, DAG, declared keys, selection
  derive-sql/     dialects, SqlToRel context provider, function stubs, spans
  derive-grain/   RelationFacts, closure, one rule per plan node, summaries
  derive-check/   Check trait, G001 to G004, E-codes
  derive-diag/    Diagnostic, source mapping, renderers, baseline, suppressions
  derive-cache/   fact cache under .derive/cache
fixtures/         small dbt projects with compiled artifacts checked in
docs/explain/     one page per finding code
xtask/            fixture regeneration, new-check scaffolding, corpus runs, synth projects
```

## Development

```sh
cargo test --workspace                                        # unit, rule and golden tests
cargo test --release -p derive-grain --test soundness         # soundness property tests
cargo test --release -p derive-cli --test speed -- --ignored  # speed targets
cargo bench -p derive-cli                                     # criterion benchmarks
cargo xtask fixtures                                          # regenerate fixture artifacts (needs dbt-duckdb, duckdb)
cargo xtask new-check G006                                    # scaffold a check
cargo xtask corpus ../some-dbt-project ...                    # corpus statistics
```

The toolchain is pinned to Rust 1.94 by `rust-toolchain.toml`, the minimum DataFusion
55 supports. DataFusion is pinned to `=55.2.0` and upgraded deliberately.
