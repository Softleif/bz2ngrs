//! The same properties for `bz2ngrs::__decompress_at`, at `Level::fallback()`
//! and at every level the CPU supports.

#[macro_use]
mod common;

surface_properties!(common::oracle::level_decoders());
