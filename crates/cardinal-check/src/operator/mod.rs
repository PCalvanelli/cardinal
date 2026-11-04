//! The operator layer: each join, filter, aggregate, window and set operation inside a
//! model, checked against the keys and functional dependencies (the types) of what it
//! reads. Depends on the node layer: declared grain and keys are what it checks against.
//!
//! By operator:
//! - join: fans out and inflates measures (G001), many-to-many (G003)
//! - filter: drops rows with nulls (G011: NOT IN over nulls, WHERE turning an outer
//!   join inner); a soft-delete flag dropped without one (G014)
//! - aggregate, the only operator allowed to change grain: repeated values (G001),
//!   non-additive numbers (G008), mixed units (G009), count(*) over an outer join
//!   (G013)
//! - window and dedup: order that can tie (G007)
//! - set operation: columns matched by position, not name (G010); UNION merging rows
//!   that can repeat (G012)
//! - the model as a whole: declared key not guaranteed (G002, G006), exactly-once
//!   contracts (P001, P002), guarantees lost since main (P003)

pub(crate) mod g001;
pub(crate) mod g002;
pub(crate) mod g003;
pub(crate) mod g006;
pub(crate) mod g007;
pub(crate) mod g008;
pub(crate) mod g009;
pub(crate) mod g010;
pub(crate) mod g011;
pub(crate) mod g012;
pub(crate) mod g013;
pub(crate) mod g014;
pub(crate) mod p001;
pub(crate) mod p003;
