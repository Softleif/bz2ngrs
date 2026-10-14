//! Shared by the fuzz targets and the seed generator: the same oracle and
//! differential rule as the hegel tests, included from `tests/common`.

#[path = "../../tests/common/oracle.rs"]
pub mod oracle;
