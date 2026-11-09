<p align="center"><img src="docs/assets/cardinal.svg" width="128" alt="Cardinal logo"></p>

# Cardinal

[![CI](https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml)

Finds where your dbt models can repeat, drop or miscount rows, and proves where they
can't, from the compiled SQL alone. It never connects to your warehouse.

> **Beta (v0.1.0-beta.6).** Output, flags and config may change.

```
error[G001]: a join repeats values that are then added up
 --> models/marts/daily_revenue.sql:4:24
  |
4 |     sum(line_amount) + sum(shipping_fee) as revenue
  |                        ^^^^^^^^^^^^^^^^^ shipping_fee has one value per (order_id)
```

It checks structure, not meaning: whether revenue counts each order once, not whether
revenue should include refunds.

## Get started

```sh
cargo install --path crates/cardinal-cli   # from a clone of this repo
dbt compile && dbt docs generate
cardinal init                              # writes cardinal.toml and a baseline
cardinal check
```

Cardinal learns each table's key, the columns that identify one row, from `unique`
tests and `meta.cardinal` declarations. Where it doesn't know a key, it stays quiet.
Only errors fail the run (exit 1); exit 2 means Cardinal couldn't run.

## In CI

```sh
cardinal check --state prod-artifacts/             # fail on guarantees main loses
cardinal check --baseline .cardinal/baseline.json  # or fail only on new findings
```

Add `--format sarif` for GitHub code scanning.

## More

- Commands: `describe`, `explain`, `coverage`, `guess`, `assumptions`, `watch`.
- Declarations: `grain`, `references`, `exactly_once`, `measure` and `unit`; see
  [docs/configuration.md](docs/configuration.md).
- Every finding code: [docs/explain/](docs/explain/).
- Silence one finding: `-- cardinal: allow(G001) reason: ...`

## Development

```sh
cargo test --workspace --exclude cardinal-grain && cargo test -p cardinal-grain --test rules --lib
```

Try it without dbt: `CARDINAL_ALLOW_STALE=1 cardinal check --project-dir fixtures/shop`.
