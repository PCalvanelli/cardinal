<p align="center">
  <img src="docs/assets/cardinal.svg" width="128" height="128" alt="Cardinal logo: a red cardinal bird">
</p>

<h1 align="center">Cardinal</h1>

<p align="center"><b>Finds where your dbt models can repeat, drop or miscount rows, and proves where they can't, from the compiled SQL alone.</b></p>

<p align="center">
  <a href="https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml"><img src="https://github.com/PCalvanelli/cardinal/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <img src="https://img.shields.io/badge/status-beta-orange" alt="Status: beta">
  <img src="https://img.shields.io/badge/rust-1.94-b7410e?logo=rust" alt="Rust 1.94">
  <img src="https://img.shields.io/badge/built%20for-dbt-FF694B?logo=dbt&logoColor=white" alt="Built for dbt">
</p>

> **Beta (v0.1.0-beta.6).** Output, flags and config may change between beta releases.

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
cardinal init
```

`cardinal init` checks that the artifacts are there and current, then sets up Cardinal:

```
Setting up Cardinal in /home/me/shop
  ✓ target/manifest.json: 214 models, dbt 1.9.4
  ✓ compiled SQL for all 214 models
  ✓ target/catalog.json
  ✓ dialect: snowflake (from the manifest)
  ✓ wrote cardinal.toml
  ! wrote a stub for 2 functions Cardinal didn't know: parse_ip(), my_udf()
      check each return type in cardinal.toml
  ✓ recorded 1 finding in .cardinal/baseline.json
      in CI, cardinal check --baseline .cardinal/baseline.json fails only on new ones
  ✓ added .cardinal/cache/ to .gitignore
```

Commit cardinal.toml and the baseline. Then:

```sh
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
- **Only errors fail the run.** Warnings don't, so you can add Cardinal to an existing
  project without breaking anything.

| Exit code | Meaning |
| --- | --- |
| 0 | No errors (warnings may be shown) |
| 1 | At least one error |
| 2 | Cardinal couldn't run, for example with no manifest |

Other commands:

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

Each `cardinal check` also writes `target/cardinal_shapes.json`: every model's shape
contract (its key, columns, non-null columns, measures, and the rows of its parents it
keeps), with what each rests on. If main's CI runs `cardinal check` and keeps that file
with the other artifacts, `--state` reads main's shapes from it instead of analyzing
main again. Cardinal uses the file only if the same Cardinal version wrote it from that
`manifest.json`. Its format may change between beta releases.

Without main's artifacts, commit a baseline of today's findings instead, and CI fails
only on new ones:

```sh
cardinal check --update-baseline                   # writes .cardinal/baseline.json
cardinal check --baseline .cardinal/baseline.json  # in CI
```

Add `--format sarif --output cardinal.sarif` for GitHub code scanning, or `--format json`.

## Telling Cardinal what you know

Everything goes under `meta.cardinal` in your dbt YAML. The most useful one is a key:

```yaml
sources:
  - name: app
    tables:
      - name: orders
        meta:
          cardinal:
            grain: [order_id]     # one row per order_id; also means order_id is never null
```

| Setting | What it tells Cardinal |
| --- | --- |
| `grain` | The columns that identify one row (also read from `unique` tests and `primary_key` constraints) |
| `references` | Every value has a match in another table, so an inner join drops nothing |
| `exactly_once` | This model must keep every row of another model exactly once (P001 if unproven) |
| `measure` | A column can't be added up at all, or not across dates |
| `unit` | The column holding an amount's currency or unit |

Don't know a key? `cardinal guess` writes likely keys as dbt tests, and the ones dbt
passes start counting. See [docs/configuration.md](docs/configuration.md) for examples
of each setting.

## What it finds

Cardinal treats a dbt project as typed code: keys and grain are the types, and every
SQL operator has a predictable effect on them. It checks four layers, each depending on
the one before, like the stages of a compiler:

| Layer | Checks | Decided from | Needs |
| --- | --- | --- | --- |
| **code** | each model's file: its compiled SQL exists and parses | compiled SQL | nothing |
| **graph** | every `ref()` and every relation a declaration names resolves | manifest.json | code |
| **node** | each model's declared shape: columns, types, grain | manifest + catalog | graph |
| **operator** | each join, filter, aggregate, window and set operation, against the keys of what it reads | the plan + node declarations | node |

When a model fails one layer, Cardinal reports that and nothing from the later layers
for it: those findings would rest on what failed.

Outer joins are tracked precisely. After `customers left join orders`, `order_id` is
still unique among the rows where it isn't null (the same thing a dbt `unique` test
checks), so each order is still counted at most once, and a later join on `order_id` is
still many-to-one.

The operator layer is a catalog of what each operator can get wrong:

| Operator | What goes wrong | Code | Default |
| --- | --- | --- | --- |
| join | Fans out, and a sum or count then adds a repeated value more than once | G001 | error |
| join | Many-to-many: neither side is unique on the join columns | G003 | warning |
| filter | `NOT IN` over values that can be null keeps no rows; `WHERE` on a left join's null-filled side turns it into an inner join | G011 | warning |
| filter | A soft-delete flag (`_fivetran_deleted`, `_sdc_deleted_at`, `_ab_cdc_deleted_at`) is dropped with nothing on the way filtering on it, so deleted rows count | G014 | warning |
| aggregate | Adds up a number that can't be added: overlapping distinct counts, averages, ratios, percentiles, balances across dates | G008 | warning |
| aggregate | Adds amounts in different units, such as `amount` across currencies (needs `unit`) | G009 | error |
| aggregate | `count(*)` over a left join counts a row with no match as 1, not 0 | G013 | warning |
| window, dedup | `row_number`, `lag`, `lead` or `first_value` ordered by columns that can tie, so the row picked is random | G007 | warning |
| set operation | `UNION` matches columns by position: a later branch's columns are swapped or named differently | G010 | warning, error when swapped |
| set operation | `UNION` (not `UNION ALL`) merges equal rows, and a branch can repeat a row with an amount in it, so real rows are lost | G012 | warning |
| the model | A declared key, or an incremental model's or snapshot's `unique_key`, that the SQL doesn't guarantee | G002, G006 | warning |
| the model | An `exactly_once` contract that isn't proven, or rests on something no test checks | P001, P002 | error, warning |
| the model | A guarantee main has that the branch loses (with `--state`) | P003 | error |

And the other layers:

| Code | Layer | Default | Finds |
| --- | --- | --- | --- |
| E001–E003 | code | error | No compiled SQL, SQL that doesn't parse, or an unknown function |
| G005 | code | warning | A suppression comment that no longer matches anything |
| E004 | graph | error | The SQL reads a relation that isn't in the manifest |
| D002 | graph | warning, error for `exactly_once` | A declaration naming a relation that doesn't exist, or a contract on one the model doesn't read |
| E006 | node | error | The SQL reads a column the relation doesn't have |
| D001 | node | warning | A declaration naming a column the model doesn't have, so it's ignored |
| G004 | node | info | Models Cardinal can't check because a key is unknown, grouped by cause |
| E005 | operator | error | SQL Cardinal can't analyze yet |

All layers run in one pass, because each model's operators are checked against its
parents' shapes. To check only some layers, use `cardinal check --layer node,operator`
(or `layers = [...]` in cardinal.toml). With only `graph` and `node`, Cardinal reads no
SQL, so `cardinal check --layer graph,node` works on the output of `dbt parse`, as a
first step in CI.

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
