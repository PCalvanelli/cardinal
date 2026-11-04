# CARDINAL Structural Verification Program

**Program Reference Manual**
Release 0.1 Modification Level Beta-4 &nbsp;·&nbsp; Form No. CRD-0001-4

> **Programming Notice.** This is a beta-level program. Output formats, operands and
> control statements are subject to change without notice between beta releases.

---

## Preface

This publication describes the CARDINAL program (hereafter, "the program"). The
program examines the SQL compiled by dbt and determines, prior to any execution,
whether a model can count a row twice, lose a row, or add values that may not be
added. The program does not connect to the data warehouse.

This manual is intended for data engineers responsible for the correctness of dbt
projects. The reader should be familiar with SQL and dbt.

## 1.0 Installation

The program is distributed in source form. Installation requires the Rust 1.94
toolchain. Initial compilation requires several minutes.

```sh
git clone https://github.com/PCalvanelli/cardinal
cd cardinal && cargo install --path crates/cardinal-cli
```

## 2.0 Operation

From the dbt project directory, issue:

```sh
dbt compile && dbt docs generate
cardinal check
```

Typical execution time is under one second for 1,000 models.

**Return codes:**

| Code | Meaning |
| --- | --- |
| 0 | Completed. Warnings may have been issued. |
| 1 | Completed. One or more errors were detected. |
| 2 | Program could not run (for example, `manifest.json` not found). |

**Note:** The program must know the **key** of each table (the columns that identify
one row). Keys are obtained from `unique` tests and from `meta.cardinal` declarations.
Where no key is known, the program issues no finding. The `Next:` line of the report
identifies the keys that would enable the most additional checking.

## 3.0 Commands

| Command | Function |
| --- | --- |
| `cardinal check` | Verify the project and report findings |
| `cardinal describe MODEL` | Display a model's key, column ownership and joins |
| `cardinal explain CODE` | Display the meaning of a finding code |
| `cardinal coverage` | List keys recommended for declaration |
| `cardinal guess` | Generate dbt tests for probable keys |
| `cardinal assumptions` | List facts taken on trust, and whether dbt tests them |

## 4.0 Use in Continuous Integration

To compare a branch with main, supply main's dbt artifacts:

```sh
cardinal check --state prod-artifacts/    # or set DBT_STATE
```

Any guarantee proven on main and lost on the branch is reported as P003. Findings
already present on main do not cause failure. Where main's artifacts are unavailable,
a baseline may be used:

```sh
cardinal check --update-baseline
cardinal check --baseline .cardinal/baseline.json
```

SARIF output is available with `--format sarif --output cardinal.sarif`.

## 5.0 Control Statements

All declarations are coded under `meta.cardinal` in dbt YAML.

| Operand | Purpose |
| --- | --- |
| `grain: [cols]` | Declares the table key. Implies the columns are not null. |
| `references: src.table` | Declares that each value has a match in another table |
| `exactly_once: model` | Requires proof that every parent row is kept exactly once |
| `measure: {kind: ...}` | Declares a column non-additive or a stock over dates |
| `unit: column` | Names the column holding the unit of an amount |

## 6.0 Diagnostic Messages

| Code | Severity | Condition |
| --- | --- | --- |
| G001 | E | Join repeats a value which is then summed or counted |
| G002 | W | Declared key not guaranteed by the SQL |
| G003 | W | Many-to-many join |
| G004 | I | Models not checkable due to unknown key |
| G005 | W | Suppression comment matches nothing |
| G006 | W | Incremental `unique_key` not unique |
| G007 | W | Window function ordered by columns that can tie |
| G008 | W | Addition of a non-additive value |
| G009 | E | Addition of amounts in differing units |
| P001 | E | `exactly_once` contract not proven |
| P002 | W | Proven contract relies on an untested assumption |
| P003 | E | Guarantee present on main lost on branch |
| E001–E005 | E | SQL could not be read |

To suppress a single message, code a comment with a reason:

```sql
sum(shipping_fee) as shipping  -- cardinal: allow(G001) reason: allocated upstream
```

For full description of each message, issue `cardinal explain CODE` or refer to
`docs/explain/`.

## 7.0 Restrictions

The program verifies structure, not meaning. It can establish that revenue counts each
order once; it cannot establish whether revenue should include refunds. A proof holds
for any data satisfying the dbt tests on which it relies.

## Appendix A. Maintenance Procedures

```sh
cargo test --workspace
cargo test --release -p cardinal-grain --test soundness
cargo xtask fixtures    # requires dbt-duckdb and duckdb
```

Sample projects are supplied in `fixtures/`. The toolchain is fixed at Rust 1.94 and
DataFusion at `=55.2.0`.
