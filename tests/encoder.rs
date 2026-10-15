//! The properties of `bz2ngrs::Encoder`, for `Encoder::new()` and for an
//! encoder at `Level::fallback()` and at every level the CPU supports: all of
//! them write libbz2 1.0.8's stream, byte for byte, up to the origPtr of
//! exactly periodic blocks.

#[macro_use]
mod common;

use common::enc_oracle::all_encoders;
use common::{enc_props, inputs};

#[hegel::test]
fn matches_libbz2_arbitrary_bytes(tc: hegel::TestCase) {
    enc_props::matches_libbz2(&tc, &all_encoders(), inputs::any_bytes());
}

#[hegel::test]
fn matches_libbz2_runs(tc: hegel::TestCase) {
    enc_props::matches_libbz2(&tc, &all_encoders(), inputs::runs());
}

#[hegel::test]
fn matches_libbz2_periodic(tc: hegel::TestCase) {
    enc_props::matches_libbz2(&tc, &all_encoders(), inputs::periodic());
}

#[hegel::test]
fn matches_libbz2_tiny_alphabet(tc: hegel::TestCase) {
    enc_props::matches_libbz2(&tc, &all_encoders(), inputs::tiny_alphabet());
}

#[hegel::test(
    test_cases = common::pinned::cases(300),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_expanding(tc: hegel::TestCase) {
    enc_props::matches_libbz2(&tc, &all_encoders(), inputs::expanding(21));
}

#[hegel::test(
    test_cases = common::pinned::cases(60),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_block_boundary(tc: hegel::TestCase) {
    enc_props::matches_libbz2_boundary(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(40),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_full_block_at_end(tc: hegel::TestCase) {
    enc_props::matches_libbz2_full_at_end(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(20),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_many_blocks(tc: hegel::TestCase) {
    enc_props::matches_libbz2_many_blocks(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(100),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_long_periodic(tc: hegel::TestCase) {
    enc_props::matches_libbz2_long_periodic(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(6),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_code_length_limit(tc: hegel::TestCase) {
    enc_props::matches_libbz2_code_length_limit(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(150),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_exact_periodic(tc: hegel::TestCase) {
    enc_props::matches_libbz2_exact_periodic(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(300),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_table_count_thresholds(tc: hegel::TestCase) {
    enc_props::matches_libbz2_table_count_threshold(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(300),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn matches_libbz2_single_distinct_byte(tc: hegel::TestCase) {
    enc_props::matches_libbz2_single_byte(&tc, &all_encoders());
}

#[hegel::test(
    test_cases = common::pinned::cases(60),
    suppress_health_check = [hegel::HealthCheck::TooSlow]
)]
fn reuse_independent_of_earlier_calls(tc: hegel::TestCase) {
    enc_props::reuse_independent(&tc, &all_encoders());
}

#[hegel::test]
fn compress_appends_to_out(tc: hegel::TestCase) {
    enc_props::compress_appends(&tc, &all_encoders());
}
