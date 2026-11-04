//! The graph layer: the edges between models. Every ref() and every relation a
//! declaration names must resolve, decidable from the manifest. Depends on the code
//! layer: the model's SQL must parse.

pub(crate) mod d002;
