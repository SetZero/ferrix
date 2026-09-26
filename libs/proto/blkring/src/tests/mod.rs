//! Tests for the block ring, with both sides stepping over one shared buffer.
//!
//! The buffer counts every read a side makes of a field it owns — its own
//! indices and want-bell flag, the header fields it wrote or read once, the
//! array it produces — and every test that builds a pair asserts the count is
//! zero, so the rule that a side never reads back its own fields is checked by
//! every exchange here rather than by one test about it.

mod control;
mod driver;
mod geometry;
mod handshake;
mod identity;
mod kernel;
mod layout;
mod lifecycle;
mod model;
mod support;
