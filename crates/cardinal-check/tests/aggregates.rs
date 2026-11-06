//! G001, G008 and G009 judge the same aggregate calls. A join that repeats the values is
//! the bigger problem: G001 reports it, and G008 and G009 leave that call alone.

use cardinal_check::{Check, CheckContext, FanOutAggregation, Locator, MixedUnits, NonAdditiveSum};
use cardinal_dbt::RelationId;
use cardinal_diag::{Code, Label};
use cardinal_grain::testing::{GrainCase, functions, grain_case, key, test_relation};
use cardinal_sql::SqlSpan;

struct Plain;

impl Locator for Plain {
    fn label(&self, relation: &RelationId, span: Option<SqlSpan>, message: String) -> Label {
        Label {
            file: relation.0.clone(),
            span,
            message,
        }
    }
    fn display(&self, relation: &RelationId) -> String {
        relation.0.clone()
    }
}

fn codes(case: &GrainCase) -> Vec<Code> {
    let model = case.run();
    let rel = test_relation(&model.relation, "m");
    let cx = CheckContext {
        relation: &rel,
        locator: &Plain,
        functions: functions(),
        declared: &[],
    };
    let checks: [&dyn Check; 3] = [&FanOutAggregation, &NonAdditiveSum, &MixedUnits];
    let mut out = Vec::new();
    for c in checks {
        c.run(&cx, &model, &mut out);
    }
    out.iter().map(|d| d.code).collect()
}

#[test]
fn mixed_units_after_a_repeating_join_are_g001_only() {
    let case = |sql: &str| {
        grain_case()
            .table(
                "orders",
                &["order_id", "customer_id", "amount", "currency"],
                key(&["order_id"]),
            )
            .table(
                "lines",
                &["order_id", "line_no"],
                key(&["order_id", "line_no"]),
            )
            .unit("orders", "amount", "currency")
            .sql(sql)
    };
    let alone = case("select customer_id, sum(amount) as total from orders group by customer_id");
    assert_eq!(codes(&alone), [Code::G009]);
    let joined = case(
        "select o.customer_id, sum(o.amount) as total
         from orders o join lines l on l.order_id = o.order_id
         group by o.customer_id",
    );
    assert_eq!(codes(&joined), [Code::G001]);
}

#[test]
fn a_summed_stock_after_a_repeating_join_is_g001_only() {
    let case = |sql: &str| {
        grain_case()
            .table(
                "balances",
                &["account_id", "day", "balance"],
                key(&["account_id", "day"]),
            )
            .table("txns", &["txn_id", "account_id"], key(&["txn_id"]))
            .stock("balances", "balance", &["day"])
            .sql(sql)
    };
    let alone = case("select account_id, sum(balance) as b from balances group by account_id");
    assert_eq!(codes(&alone), [Code::G008]);
    let joined = case(
        "select b.account_id, sum(b.balance) as b
         from balances b join txns t on t.account_id = b.account_id
         group by b.account_id",
    );
    assert_eq!(codes(&joined), [Code::G001]);
}
