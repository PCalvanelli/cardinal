//! What Cardinal knows about each built-in aggregate, in one table. The analysis reads it
//! to decide which outputs are measures, and G001, G008 and G009 read it to decide which
//! calls they judge. A stub aggregate (from config or an adapter's pack) says only
//! whether it is idempotent; see `cardinal_sql::StubSpec`.

use crate::facts::Computed;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aggregate {
    /// Lowercase.
    pub name: &'static str,
    /// Whether repeated input rows leave the result unchanged: `Some(true)` for min and
    /// max, `Some(false)` for sum and count, None when not known (G001 then warns).
    pub idempotent: Option<bool>,
    /// The result counts or adds up rows, so dividing two of them makes a ratio. min, max
    /// and the like pick one value: `sum(a) / max(rate)` is a conversion.
    pub counting: bool,
    /// Counts distinct values: summing its results across groups can count a value once
    /// per group. `count(distinct …)` is one too; see [`is_distinct_count`].
    pub distinct_count: bool,
    /// Adds up rows, so over an ordered window frame it is a running total.
    pub totals: bool,
    /// The result is in its argument's unit: amounts in USD sum to USD.
    pub keeps_unit: bool,
    /// What the result is when it can't be added up across groups.
    pub computed: Option<Computed>,
}

const UNKNOWN: Aggregate = Aggregate {
    name: "",
    idempotent: None,
    counting: false,
    distinct_count: false,
    totals: false,
    keeps_unit: false,
    computed: None,
};

const fn adds(name: &'static str) -> Aggregate {
    Aggregate {
        name,
        idempotent: Some(false),
        ..UNKNOWN
    }
}

const fn picks(name: &'static str) -> Aggregate {
    Aggregate {
        name,
        idempotent: Some(true),
        ..UNKNOWN
    }
}

const fn average(name: &'static str) -> Aggregate {
    Aggregate {
        name,
        idempotent: Some(false),
        counting: true,
        keeps_unit: true,
        computed: Some(Computed::Average),
        ..UNKNOWN
    }
}

const fn distinct_count(name: &'static str) -> Aggregate {
    Aggregate {
        name,
        counting: true,
        distinct_count: true,
        ..UNKNOWN
    }
}

const fn percentile(name: &'static str) -> Aggregate {
    Aggregate {
        name,
        computed: Some(Computed::Percentile),
        ..UNKNOWN
    }
}

pub const AGGREGATES: &[Aggregate] = &[
    Aggregate {
        name: "sum",
        idempotent: Some(false),
        counting: true,
        totals: true,
        keeps_unit: true,
        ..UNKNOWN
    },
    Aggregate {
        name: "count",
        idempotent: Some(false),
        counting: true,
        totals: true,
        ..UNKNOWN
    },
    average("avg"),
    average("mean"),
    Aggregate {
        keeps_unit: true,
        ..picks("min")
    },
    Aggregate {
        keeps_unit: true,
        ..picks("max")
    },
    picks("any_value"),
    picks("bool_and"),
    picks("bool_or"),
    adds("var"),
    adds("var_samp"),
    adds("var_pop"),
    adds("variance"),
    adds("variance_samp"),
    adds("variance_pop"),
    adds("stddev"),
    adds("stddev_samp"),
    adds("stddev_pop"),
    adds("string_agg"),
    adds("array_agg"),
    adds("listagg"),
    distinct_count("approx_distinct"),
    distinct_count("approx_count_distinct"),
    distinct_count("hll"),
    percentile("median"),
    percentile("approx_median"),
    percentile("percentile_cont"),
    percentile("percentile_disc"),
    percentile("approx_percentile_cont"),
    percentile("approx_percentile_cont_with_weight"),
    percentile("quantile"),
    percentile("quantile_cont"),
    percentile("quantile_disc"),
];

/// The built-in aggregate named `name` (lowercase).
pub fn aggregate(name: &str) -> Option<&'static Aggregate> {
    AGGREGATES.iter().find(|a| a.name == name)
}

/// Whether the aggregate counts distinct values, `count(distinct …)` included.
pub fn is_distinct_count(name: &str, distinct: bool) -> bool {
    let n = name.to_ascii_lowercase();
    (n == "count" && distinct) || aggregate(&n).is_some_and(|a| a.distinct_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn names(f: impl Fn(&Aggregate) -> bool) -> BTreeSet<&'static str> {
        AGGREGATES.iter().filter(|a| f(a)).map(|a| a.name).collect()
    }

    fn set(v: &[&'static str]) -> BTreeSet<&'static str> {
        v.iter().copied().collect()
    }

    /// The lists each property was read from before this table, kept verbatim.
    #[test]
    fn same_as_the_lists_it_replaced() {
        let g001_non_idempotent = [
            "sum",
            "avg",
            "mean",
            "count",
            "var",
            "var_samp",
            "var_pop",
            "variance",
            "variance_samp",
            "variance_pop",
            "stddev",
            "stddev_samp",
            "stddev_pop",
            "string_agg",
            "array_agg",
            "listagg",
        ];
        let g001_idempotent = ["min", "max", "any_value", "bool_and", "bool_or"];
        let counting = [
            "sum",
            "count",
            "avg",
            "mean",
            "approx_distinct",
            "approx_count_distinct",
            "hll",
        ];
        let distinct_counts = ["approx_distinct", "approx_count_distinct", "hll"];
        let averages = ["avg", "mean"];
        let percentiles = [
            "median",
            "approx_median",
            "percentile_cont",
            "percentile_disc",
            "approx_percentile_cont",
            "approx_percentile_cont_with_weight",
            "quantile",
            "quantile_cont",
            "quantile_disc",
        ];
        // grain's running totals, and G009's and grain's amounts that keep their unit.
        let totals = ["sum", "count"];
        let keeps_unit = ["sum", "avg", "mean", "min", "max"];

        assert_eq!(
            names(|a| a.idempotent == Some(false)),
            set(&g001_non_idempotent)
        );
        assert_eq!(names(|a| a.idempotent == Some(true)), set(&g001_idempotent));
        assert_eq!(names(|a| a.counting), set(&counting));
        assert_eq!(names(|a| a.distinct_count), set(&distinct_counts));
        assert_eq!(
            names(|a| a.computed == Some(Computed::Average)),
            set(&averages)
        );
        assert_eq!(
            names(|a| a.computed == Some(Computed::Percentile)),
            set(&percentiles)
        );
        assert!(AGGREGATES.iter().all(|a| matches!(
            a.computed,
            None | Some(Computed::Average | Computed::Percentile)
        )));
        assert_eq!(names(|a| a.totals), set(&totals));
        assert_eq!(names(|a| a.keeps_unit), set(&keeps_unit));
        assert_eq!(
            names(|_| true).len(),
            AGGREGATES.len(),
            "each name appears once"
        );
    }

    #[test]
    fn distinct_counts() {
        assert!(is_distinct_count("COUNT", true));
        assert!(!is_distinct_count("count", false));
        assert!(is_distinct_count("HLL", false));
        assert!(!is_distinct_count("sum", true));
    }
}
