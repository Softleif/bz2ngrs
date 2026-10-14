# cram-extract

Standalone tool (own `[workspace]`, not part of the bz2ngrs workspace) that pulls the raw bzip2
payloads out of CRAM files: CRAM method 2 blocks (`codec=block`) and arith-coded (method 6)
blocks whose plain EXT body starts with `BZh` (`codec=arith-ext`; none found in any source so far,
nested STRIPE/PACK/RLE variants are skipped). Handles CRAM 2.x/3.0/3.1 container and block headers.
The bytes written are exactly what seqair hands to its bzip2 decoder.

```sh
cd tools/cram-extract && cargo build --release
X=target/release/cram-extract
$X extract <in.cram> <source-name> <corpus-dir> [--max-containers N] [--max-bytes N] [--dry]
$X verify <corpus-dir>      # decode every block with bzip2 0.6.1, check len <= declared; per-source stats
```

`extract` appends to `<corpus-dir>/manifest.tsv` (header: `path source compressed_len
uncompressed_len codec content_type content_id`, tab separated) and writes
`<corpus-dir>/<source>/<NNNNNN>.bz2`. `--dry` only prints the block method histogram. Delete the
corpus dir before regenerating, since extract appends and continues numbering.

## Regenerate the corpus (corpus/cram-bzip2, gitignored)

Output dir `C=corpus/cram-bzip2`. Reference for Ultima: the 1000 Genomes
`GRCh38_full_analysis_set_plus_decoy_hla.fa` (chr1..3 `M5` match the Ultima `@SQ` lines; the Ultima
`UR` points at a Broad `Homo_sapiens_assembly38.fasta` that is not available). A copy lives at
`~/.cache/rastair-happy/reference/` and `/Volumes/Softleif/bsb/na12878/`.

1. seqair's own CRAMs (only the two hts-specs files contain bzip2):

   ```sh
   D=~/Code/bsblabludwig/seqair/tests/data/hts-specs/cram/3.1/passed
   $X extract $D/level-3.cram hts-specs-level-3 $C
   $X extract $D/level-4.cram hts-specs-level-4 $C
   ```

2. Ultima ppmSeq CRAMs on /Volumes/Softleif/bsb/ultima-ppmseq use CRAM 3.0 with only raw (0),
   gzip (1) and rANS 4x8 (4) blocks, so no bzip2. Re-encode a slice of reads with bzip2:

   ```sh
   REF=~/.cache/rastair-happy/reference/GRCh38_full_analysis_set_plus_decoy_hla.fa
   U=/Volumes/Softleif/bsb/ultima-ppmseq
   # enc <input cram> <n reads> <out> <cram output options>; header is ~3400 lines
   enc() { in=$1; n=$2; out=$3; shift 3
     samtools view -h --no-PG -T $REF $U/$in | head -n $((n + 3400)) |
       samtools view --no-PG -T $REF -O "cram,$*" -o $out -; }
   enc 414004-L7384-Z0008-CACATCCTGCATGTGAT.cram 1000000 ultima-L7384-v30-bz6.cram version=3.0,use_bzip2=1,use_rans=0,use_lzma=0,level=6
   enc 414004-L7385-Z0113-CAGTTCATCTGTGAT.cram    400000 ultima-L7385-v30-bz1.cram version=3.0,use_bzip2=1,use_rans=0,use_lzma=0,level=1
   enc 414004-L7386-Z0114-CAACATACATCAGAT.cram    600000 ultima-L7386-v30-bz9.cram version=3.0,use_bzip2=1,use_rans=0,use_lzma=0,level=9
   enc 414004-L7388-Z0016-CATCCTGTGCGCATGAT.cram  500000 ultima-L7388-v31-archive.cram version=3.1,archive
   for s in ultima-L7384-v30-bz6 ultima-L7385-v30-bz1 ultima-L7386-v30-bz9 ultima-L7388-v31-archive; do
     $X extract $s.cram $s $C; done
   ```

3. seqair's `test.bam` re-encoded (reference `test.fasta.gz`):

   ```sh
   D=~/Code/bsblabludwig/seqair/tests/data
   enc() { name=$1; shift; samtools view --no-PG -T $D/test.fasta.gz -O "cram,$*" -o seqair-test-$name.cram $D/test.bam; }
   enc v30-bz1 version=3.0,use_bzip2=1,use_rans=0,use_lzma=0,level=1
   enc v30-bz9 version=3.0,use_bzip2=1,use_rans=0,use_lzma=0,level=9
   enc v31-bzonly version=3.1,use_bzip2=1,use_rans=0,use_lzma=0,use_arith=0,use_tok=0,use_fqz=0,level=6
   enc v31-archive version=3.1,archive
   enc v31-small version=3.1,small
   for s in v30-bz1 v30-bz9 v31-bzonly v31-archive v31-small; do $X extract seqair-test-$s.cram seqair-test-$s $C; done
   ```

Note: htslib with `use_bzip2=1` still chooses per block between gzip and bzip2 (smaller wins), so
the small blocks mostly stay gzip. Intermediate re-encoded CRAMs live in the session scratchpad,
not in the repo.
