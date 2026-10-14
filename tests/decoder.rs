//! The properties of `bz2ngrs::Decoder`, for `Decoder::new()` and for a
//! decoder at `Level::fallback()` and at every level the CPU supports.

#[macro_use]
mod common;

surface_properties!(common::oracle::all_decoders());
