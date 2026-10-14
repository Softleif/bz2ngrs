//! Compiles bz2-ng's C library the way c/CMakeLists.txt does (same source
//! list, same per-file ISA flags, -O2/-O3) and links it statically.
//!
//! Symbol clash: libbz2-rs-sys (behind the `bzip2` crate) can export the same
//! `BZ2_*` names, so every public `BZ2_*` symbol of bz2-ng is renamed
//! `ng_BZ2_*` with a force-included header, like test/reference_rename.h does
//! for the stock libbz2. All other bz2-ng globals are `bzng_*`.
use std::{env, fs, path::PathBuf};

const SOURCES: &[&str] = &[
    "lib/bzlib.c",
    "lib/compress.c",
    "lib/enc_block.c",
    "lib/enc_huff.c",
    "lib/enc_sort.c",
    "lib/sort_libsais.c",
    "lib/decompress.c",
    "lib/dec_block.c",
    "lib/randtable.c",
    "lib/crc32.c",
    "lib/crc32_table.c",
    "lib/utils.c",
    "lib/kern_scalar.c",
    "lib/arm/cpu_features.c",
    "lib/arm/crc32_armv8.c",
    "lib/arm/crc32_pmull.c",
    "lib/arm/kern_neon.c",
    "lib/x86/cpu_features.c",
    "lib/x86/crc32_pclmul.c",
    "lib/x86/crc32_vpclmul.c",
    "lib/x86/kern_sse4.c",
    "lib/x86/kern_avx2.c",
];
// CMake builds the encoder side at -O3 and the rest at -O2.
const O3: &[&str] = &[
    "lib/compress.c",
    "lib/enc_block.c",
    "lib/enc_huff.c",
    "lib/enc_sort.c",
    "lib/sort_libsais.c",
];
const PUBLIC: &[&str] = &[
    "BZ2_bzCompressInit",
    "BZ2_bzCompress",
    "BZ2_bzCompressEnd",
    "BZ2_bzDecompressInit",
    "BZ2_bzDecompress",
    "BZ2_bzDecompressEnd",
    "BZ2_bzBuffToBuffCompress",
    "BZ2_bzBuffToBuffDecompress",
    "BZ2_bzlibVersion",
    "BZ2_bzReadOpen",
    "BZ2_bzReadClose",
    "BZ2_bzReadGetUnused",
    "BZ2_bzRead",
    "BZ2_bzWriteOpen",
    "BZ2_bzWrite",
    "BZ2_bzWriteClose",
    "BZ2_bzWriteClose64",
    "BZ2_bzopen",
    "BZ2_bzdopen",
    "BZ2_bzread",
    "BZ2_bzwrite",
    "BZ2_bzflush",
    "BZ2_bzclose",
    "BZ2_bzerror",
];

/// Per-file flags of bzng_isa_flags() in CMakeLists.txt. The x86 ones are
/// untested here (this machine is aarch64).
fn isa_flags(src: &str, arch: &str) -> &'static [&'static str] {
    match (arch, src) {
        ("x86_64", "lib/x86/kern_sse4.c") => &["-march=x86-64-v2"],
        ("x86_64", "lib/x86/kern_avx2.c") => &["-march=x86-64-v3"],
        ("x86_64", "lib/x86/crc32_pclmul.c") => &["-msse4.1", "-mpclmul"],
        ("x86_64", "lib/x86/crc32_vpclmul.c") => &["-mavx2", "-mpclmul", "-mvpclmulqdq"],
        ("aarch64", "lib/arm/crc32_armv8.c") => &["-march=armv8-a+crc"],
        ("aarch64", "lib/arm/crc32_pmull.c") => &["-march=armv8-a+crc+crypto"],
        _ => &[],
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=BZ2NG_C_DIR");
    println!("cargo:rerun-if-env-changed=BZ2NG_C_DEBUG");
    println!("cargo:rerun-if-changed=build.rs");
    let dir = PathBuf::from(
        env::var("BZ2NG_C_DIR").unwrap_or_else(|_| "/Users/pascal/code/tfenne/bz2-ng/c".into()),
    );
    assert!(
        dir.join("bzlib.h").exists(),
        "no bzlib.h in {dir:?}; set BZ2NG_C_DIR to bz2-ng's c/ directory"
    );
    println!("cargo:rerun-if-changed={}", dir.join("lib").display());
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let rename: String = PUBLIC
        .iter()
        .map(|s| format!("#define {s} ng_{s}\n"))
        .collect();
    let hdr = out.join("ng_rename.h");
    fs::write(&hdr, rename).unwrap();

    let mut objects = Vec::new();
    for src in SOURCES {
        let mut b = cc::Build::new();
        b.file(dir.join(src))
            .include(&dir)
            .include(dir.join("lib"))
            .std("c11")
            .opt_level(if O3.contains(src) { 3 } else { 2 })
            // -g moves a few instructions, so only on request (for a profile).
            .debug(env::var_os("BZ2NG_C_DEBUG").is_some())
            .warnings(false)
            .pic(true)
            .flag("-fvisibility=hidden")
            .flag("-include")
            .flag(hdr.to_str().unwrap())
            .define("BZNG_VERSION", "\"0.1.0\"")
            .define("_FILE_OFFSET_BITS", "64")
            .define("_DEFAULT_SOURCE", None);
        if *src == "lib/decompress.c" {
            // Decoder memory layout CMake defaults to (BZNG_DECODER_IN_FORMULA=OFF).
            b.define("BZNG_DEFAULT_LAYOUT", "BZNG_LAYOUT_WIDE");
        }
        for f in isa_flags(src, &arch) {
            b.flag(f);
        }
        objects.extend(b.compile_intermediates());
    }
    cc::Build::new().objects(objects).compile("bz2ng");
}
