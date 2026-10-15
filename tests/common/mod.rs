//! Shared by the integration tests; each test binary uses a subset.
#![allow(dead_code)]

pub mod enc_oracle;
pub mod enc_props;
pub mod forge;
pub mod inputs;
pub mod oracle;
pub mod props;

/// Case counts for the slow properties that pin their own `test_cases`. A
/// count compiled into `#[hegel::test]` wins over `HEGEL_TEST_CASES` and the
/// profile, so these read `BZ2NGRS_PINNED_CASES` instead.
pub mod pinned {
    /// `n`, or the value of `BZ2NGRS_PINNED_CASES` when it is set.
    pub fn cases(n: u64) -> u64 {
        match std::env::var("BZ2NGRS_PINNED_CASES") {
            Ok(v) if !v.is_empty() => v
                .parse()
                .expect("BZ2NGRS_PINNED_CASES must be a positive integer"),
            _ => n,
        }
    }
}

/// One property per header rule: a forged block that breaks just that rule is
/// `Corrupt` of that kind.
#[allow(unused_macros)]
macro_rules! header_defect_properties {
    ($decoders:expr; $($name:ident: $rule:ident,)*) => {
        $(
            #[hegel::test(
                test_cases = $crate::common::pinned::cases(1000),
                suppress_health_check = [hegel::HealthCheck::TooSlow]
            )]
            fn $name(tc: hegel::TestCase) {
                $crate::common::props::forged_is_corrupt(
                    &tc,
                    &$decoders,
                    $crate::common::forge::defective($crate::common::forge::Rule::$rule),
                );
            }
        )*
    };
}

/// Every property, against the decoders `$decoders` evaluates to. One
/// invocation per surface (`Decoder::new()` and every level).
#[allow(unused_macros)]
macro_rules! surface_properties {
    ($decoders:expr) => {
        #[hegel::test]
        fn roundtrip_arbitrary_bytes(tc: hegel::TestCase) {
            $crate::common::props::roundtrip(&tc, &$decoders, $crate::common::inputs::any_bytes());
        }

        #[hegel::test]
        fn roundtrip_runs(tc: hegel::TestCase) {
            $crate::common::props::roundtrip(&tc, &$decoders, $crate::common::inputs::runs());
        }

        #[hegel::test]
        fn roundtrip_periodic(tc: hegel::TestCase) {
            $crate::common::props::roundtrip(&tc, &$decoders, $crate::common::inputs::periodic());
        }

        #[hegel::test]
        fn roundtrip_tiny_alphabet(tc: hegel::TestCase) {
            $crate::common::props::roundtrip(
                &tc,
                &$decoders,
                $crate::common::inputs::tiny_alphabet(),
            );
        }

        #[hegel::test]
        fn roundtrip_many_blocks(tc: hegel::TestCase) {
            $crate::common::props::roundtrip_pieces(&tc, &$decoders);
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(60),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn roundtrip_block_boundary(tc: hegel::TestCase) {
            $crate::common::props::roundtrip_boundary(&tc, &$decoders);
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(300),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn roundtrip_expanding(tc: hegel::TestCase) {
            $crate::common::props::roundtrip(
                &tc,
                &$decoders,
                $crate::common::inputs::expanding(22),
            );
        }

        #[hegel::test]
        fn larger_out_gives_same_prefix(tc: hegel::TestCase) {
            $crate::common::props::larger_out(&tc, &$decoders);
        }

        #[hegel::test]
        fn short_out_is_output_too_small_with_capacity(tc: hegel::TestCase) {
            $crate::common::props::short_out(&tc, &$decoders);
        }

        #[hegel::test]
        fn trailing_bytes_are_ignored(tc: hegel::TestCase) {
            $crate::common::props::trailing_bytes(&tc, &$decoders);
        }

        #[hegel::test]
        fn mutated_stream_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::mutated_agrees(&tc, &$decoders);
        }

        #[hegel::test]
        fn truncated_stream_is_unexpected_eof_in_its_block(tc: hegel::TestCase) {
            $crate::common::props::truncated(&tc, &$decoders);
        }

        #[hegel::test]
        fn corrupt_block_crc_is_block_crc_with_values(tc: hegel::TestCase) {
            $crate::common::props::corrupt_block_crc(&tc, &$decoders);
        }

        #[hegel::test]
        fn corrupt_stream_crc_is_stream_crc_with_values(tc: hegel::TestCase) {
            $crate::common::props::corrupt_stream_crc(&tc, &$decoders);
        }

        #[hegel::test]
        fn wrong_header_is_not_bzip2(tc: hegel::TestCase) {
            $crate::common::props::not_bzip2(&tc, &$decoders);
        }

        #[hegel::test]
        fn hostile_arbitrary_bytes_agree_with_reference(tc: hegel::TestCase) {
            $crate::common::props::hostile(
                &tc,
                &$decoders,
                $crate::common::inputs::hostile_bytes(),
            );
        }

        #[hegel::test]
        fn hostile_bzh_header_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::hostile(&tc, &$decoders, $crate::common::inputs::hostile_bzh());
        }

        #[hegel::test]
        fn hostile_block_header_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::hostile(
                &tc,
                &$decoders,
                $crate::common::inputs::hostile_block(),
            );
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(2000),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn forged_block_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::forged_agrees(
                &tc,
                &$decoders,
                $crate::common::forge::one_block($crate::common::forge::OrigClass::Any),
            );
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(600),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn forged_multi_block_stream_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::forged_agrees(
                &tc,
                &$decoders,
                $crate::common::forge::several_blocks(),
            );
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(1500),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn forged_origptr_out_of_range_is_corrupt_orig_ptr(tc: hegel::TestCase) {
            $crate::common::props::forged_is_corrupt(
                &tc,
                &$decoders,
                $crate::common::forge::bad_orig_ptr_last(),
            );
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(400),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn forged_oversize_block_is_corrupt_block_too_long(tc: hegel::TestCase) {
            $crate::common::props::forged_is_corrupt(
                &tc,
                &$decoders,
                $crate::common::forge::oversize_block(),
            );
        }

        #[hegel::test(
            test_cases = $crate::common::pinned::cases(1500),
            suppress_health_check = [hegel::HealthCheck::TooSlow]
        )]
        fn forged_run_length_boundary_is_run_too_long_from_22_symbols(tc: hegel::TestCase) {
            $crate::common::props::forged_is_corrupt(
                &tc,
                &$decoders,
                $crate::common::forge::run_boundary(),
            );
        }

        #[hegel::test(
            test_cases = $crate::common::pinned::cases(30),
            suppress_health_check = [hegel::HealthCheck::TooSlow]
        )]
        fn forged_largest_block_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::forged_agrees(
                &tc,
                &$decoders,
                $crate::common::forge::largest_block(),
            );
        }

        #[hegel::test(
            test_cases = $crate::common::pinned::cases(1500),
            suppress_health_check = [hegel::HealthCheck::TooSlow]
        )]
        fn forged_excess_selectors_are_ignored(tc: hegel::TestCase) {
            $crate::common::props::forged_excess_selectors(&tc, &$decoders);
        }

        #[hegel::test(
            test_cases = $crate::common::pinned::cases(2000),
            suppress_health_check = [hegel::HealthCheck::TooSlow]
        )]
        fn forged_defect_cut_off_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::forged_defect_cut_agrees(&tc, &$decoders);
        }

        header_defect_properties! {
            $decoders;
            forged_bad_block_magic_is_corrupt_block_magic: BlockMagic,
            forged_empty_symbol_map_is_corrupt_no_symbols: NoSymbols,
            forged_table_count_out_of_range_is_corrupt_group_count: GroupCount,
            forged_zero_selectors_is_corrupt_selector_count: SelectorCount,
            forged_selector_beyond_tables_is_corrupt_selector: Selector,
            forged_code_length_out_of_range_is_corrupt_code_length: CodeLength,
            forged_unassigned_code_is_corrupt_huffman_code: HuffmanCode,
            forged_missing_end_of_block_is_corrupt_no_end_of_block: NoEndOfBlock,
        }

        #[hegel::test]
        fn failed_decode_leaves_no_state(tc: hegel::TestCase) {
            $crate::common::props::failure_leaves_no_state(&tc, &$decoders);
        }

        #[hegel::test(
                                            test_cases = $crate::common::pinned::cases(100),
                                            suppress_health_check = [hegel::HealthCheck::TooSlow]
                                        )]
        fn reuse_independent_of_earlier_calls(tc: hegel::TestCase) {
            $crate::common::props::reuse_independent(&tc, &$decoders);
        }

        #[hegel::test]
        fn to_vec_equals_oracle_with_actual_length(tc: hegel::TestCase) {
            $crate::common::props::to_vec_equals_oracle(&tc, &$decoders);
        }

        #[hegel::test]
        fn to_vec_below_actual_length_is_output_too_small(tc: hegel::TestCase) {
            $crate::common::props::to_vec_too_small(&tc, &$decoders);
        }

        #[hegel::test]
        fn to_vec_mutated_agrees_with_reference(tc: hegel::TestCase) {
            $crate::common::props::to_vec_mutated_agrees(&tc, &$decoders);
        }
    };
}
