//! Shared by the integration tests; each test binary uses a subset.
#![allow(dead_code)]

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

/// Every property, against the decoders `$decoders` evaluates to. One
/// invocation per surface (`decompress`, `__decompress_at`).
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
                bz2ngrs::Corruption::OrigPtr,
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
                bz2ngrs::Corruption::BlockTooLong,
            );
        }

        #[hegel::test]
        fn failed_decode_leaves_no_state(tc: hegel::TestCase) {
            $crate::common::props::failure_leaves_no_state(&tc, &$decoders);
        }
    };
}
