//! The streaming surface of the decoder: `decompress_prefix`,
//! `decompress_all`, `decompress_all_to_vec`, `DecompressReader` and
//! `From<Error> for io::Error`, at `Decoder::new()` and every SIMD level.

mod common;

use common::oracle::all_decoders;
use common::pinned::cases;
use common::stream_props as p;

macro_rules! prop {
    ($name:ident, $f:path) => {
        #[hegel::test]
        fn $name(tc: hegel::TestCase) {
            $f(&tc, &all_decoders());
        }
    };
    ($name:ident, $n:expr, $f:path) => {
        #[hegel::test(
                                    test_cases = cases($n),
                                    suppress_health_check = [hegel::HealthCheck::TooSlow]
                                )]
        fn $name(tc: hegel::TestCase) {
            $f(&tc, &all_decoders());
        }
    };
}

prop!(
    prefix_of_valid_stream_with_trailing_bytes,
    p::prefix_with_trailing
);
prop!(
    prefix_agrees_with_oracle_and_decompress,
    400,
    p::prefix_agrees
);
prop!(all_agrees_with_multi_oracle, 600, p::all_agrees_into_vec);
prop!(
    all_to_vec_agrees_with_multi_oracle,
    600,
    p::all_to_vec_agrees
);
prop!(
    all_limit_below_length_is_output_too_small,
    p::all_limit_too_small
);
prop!(all_limit_at_least_length_appends, p::all_limit_enough);
prop!(
    all_reuse_independent_of_failures,
    150,
    p::all_reuse_independent
);
prop!(error_converts_to_io_error, 400, p::into_io_error);
prop!(
    reader_valid_concatenations_equal_oracle,
    300,
    p::reader_valid
);
prop!(
    reader_agrees_with_decompress_all,
    300,
    p::reader_agrees_with_all
);
prop!(
    reader_truncated_yields_whole_blocks_then_eof,
    300,
    p::reader_truncated
);
prop!(
    reader_block_crc_yields_earlier_blocks,
    300,
    p::reader_block_crc
);
prop!(reader_reset_equals_fresh, 100, p::reader_reset);
prop!(reader_block_beyond_read_ahead, 6, p::reader_large_block);
