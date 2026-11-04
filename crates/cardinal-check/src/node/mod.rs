//! The node layer: each model's declared shape (columns, types, grain), decidable from
//! the manifest and catalog. Depends on the graph layer: declarations must name
//! relations that resolve.

pub(crate) mod d001;
pub(crate) mod g004;
