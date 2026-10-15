//! Shared by the fuzz targets and the seed generator: the same oracles and
//! differential rules as the hegel tests, included from `tests/common`.

#[path = "../../tests/common/oracle.rs"]
pub mod oracle;

#[path = "../../tests/common/enc_oracle.rs"]
pub mod enc_oracle;
