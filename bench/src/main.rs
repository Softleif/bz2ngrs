//! Decode benchmark: bz2ngrs vs the `bzip2` crate (libbz2-rs-sys) vs bz2-ng C.
//! See README.md for usage.
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_int, c_uint, c_void},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

// ---- bz2-ng C, via the standard libbz2 API (symbols renamed ng_*, see build.rs) ----

#[repr(C)]
struct BzStream {
    next_in: *mut c_char,
    avail_in: c_uint,
    total_in_lo32: c_uint,
    total_in_hi32: c_uint,
    next_out: *mut c_char,
    avail_out: c_uint,
    total_out_lo32: c_uint,
    total_out_hi32: c_uint,
    state: *mut c_void,
    bzalloc: *mut c_void,
    bzfree: *mut c_void,
    opaque: *mut c_void,
}

unsafe extern "C" {
    fn ng_BZ2_bzDecompressInit(s: *mut BzStream, verbosity: c_int, small: c_int) -> c_int;
    fn ng_BZ2_bzDecompress(s: *mut BzStream) -> c_int;
    fn ng_BZ2_bzDecompressEnd(s: *mut BzStream) -> c_int;
}

const BZ_STREAM_END: c_int = 4;

// ---- the three decoders: whole stream into an output of the known size ----

type Dec = fn(&[u8], &mut [u8]) -> Result<usize, String>;
const NAMES: [&str; 3] = ["ours", "bzip2", "C"];
const DECODERS: [Dec; 3] = [dec_ours, dec_bzip2, dec_c];

fn dec_ours(src: &[u8], out: &mut [u8]) -> Result<usize, String> {
    bz2ngrs::decompress(src, out).map_err(|e| e.to_string())
}

fn dec_bzip2(src: &[u8], out: &mut [u8]) -> Result<usize, String> {
    let mut d = bzip2::Decompress::new(false);
    match d.decompress(src, out) {
        Ok(bzip2::Status::StreamEnd) => Ok(usize::try_from(d.total_out()).unwrap_or(usize::MAX)),
        Ok(s) => Err(format!("status {s:?}")),
        Err(e) => Err(e.to_string()),
    }
}

fn dec_c(src: &[u8], out: &mut [u8]) -> Result<usize, String> {
    // SAFETY: BzStream matches bz_stream in bzlib.h; all-zero is the documented
    // initial state (null allocators select malloc/free). The buffers outlive
    // the calls and the avail_* counts are their lengths (all < 4 GiB here).
    unsafe {
        let mut s: BzStream = std::mem::zeroed();
        let rc = ng_BZ2_bzDecompressInit(&mut s, 0, 0);
        if rc != 0 {
            return Err(format!("DecompressInit rc {rc}"));
        }
        s.next_in = src.as_ptr() as *mut c_char;
        s.avail_in = src.len() as c_uint;
        s.next_out = out.as_mut_ptr().cast();
        s.avail_out = out.len() as c_uint;
        let rc = ng_BZ2_bzDecompress(&mut s);
        ng_BZ2_bzDecompressEnd(&mut s);
        if rc != BZ_STREAM_END {
            return Err(format!("Decompress rc {rc}"));
        }
        Ok(out.len() - s.avail_out as usize)
    }
}

// ---- data ----

struct Stream {
    comp: Vec<u8>,
    raw: Vec<u8>,
}

fn compress(raw: &[u8], level: u32) -> Vec<u8> {
    let mut c = Vec::with_capacity(raw.len() + raw.len() / 50 + 1024);
    let mut e = bzip2::Compress::new(bzip2::Compression::new(level), 30);
    let s = e
        .compress_vec(raw, &mut c, bzip2::Action::Finish)
        .expect("compress");
    assert!(
        matches!(s, bzip2::Status::StreamEnd),
        "compress did not finish"
    );
    c
}

/// Compresses every cut on all cores (order preserved).
fn compress_all(data: &[u8], cuts: &[(usize, usize)], level: u32) -> Vec<Stream> {
    let nt = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per = cuts.len().div_ceil(nt).max(1);
    std::thread::scope(|sc| {
        let hs: Vec<_> = cuts
            .chunks(per)
            .map(|cs| {
                sc.spawn(move || {
                    cs.iter()
                        .map(|&(a, b)| Stream {
                            comp: compress(&data[a..b], level),
                            raw: data[a..b].to_vec(),
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().expect("compress thread"))
            .collect()
    })
}

type Cuts = Vec<(usize, usize)>;

fn fixed_cuts(len: usize, step: usize) -> Vec<(usize, usize)> {
    (0..len)
        .step_by(step)
        .map(|a| (a, (a + step).min(len)))
        .collect()
}

/// Cuts at the cumulative end offsets of a CRAM slice sidecar.
fn slice_cuts(path: &Path, len: usize) -> Option<Vec<(usize, usize)>> {
    let txt = fs::read_to_string(path).ok()?;
    let mut prev = 0;
    let mut cuts = Vec::new();
    for l in txt.lines().filter(|l| !l.trim().is_empty()) {
        let end = usize::try_from(l.trim().parse::<u64>().ok()?)
            .ok()?
            .min(len);
        if end > prev {
            cuts.push((prev, end));
            prev = end;
        }
    }
    if prev < len {
        cuts.push((prev, len));
    }
    Some(cuts)
}

/// Real CRAM bzip2 blocks from the manifest, one data set per source directory.
/// Columns are found by header name when there is a header (path/file,
/// declared/uncomp/raw); otherwise column 0 is the path. The ground truth is the
/// bzip2 crate's output, cross-checked against the declared size if there is one.
fn load_manifest(path: &Path) -> BTreeMap<String, Vec<Stream>> {
    let mut sets: BTreeMap<String, Vec<Stream>> = BTreeMap::new();
    let Ok(txt) = fs::read_to_string(path) else {
        return sets;
    };
    let base = path.parent().unwrap_or(Path::new("."));
    let mut lines = txt
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .peekable();
    let (mut pcol, mut dcol) = (0, None);
    if let Some(first) = lines.peek() {
        let f: Vec<String> = first.split('\t').map(|s| s.trim().to_lowercase()).collect();
        if !f.iter().any(|c| c.parse::<u64>().is_ok()) {
            pcol = f
                .iter()
                .position(|c| c.contains("path") || c.contains("file"))
                .unwrap_or(0);
            dcol = f
                .iter()
                .position(|c| c.contains("uncomp") || c.contains("declared") || c.contains("raw"));
            lines.next();
        }
    }
    let (mut missing, mut bad, mut mismatch) = (0, 0, 0);
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        let Some(p) = f.get(pcol) else { continue };
        let p = base.join(p.trim());
        let Ok(comp) = fs::read(&p) else {
            missing += 1;
            continue;
        };
        let mut raw = Vec::new();
        if bzip2::read::BzDecoder::new(&comp[..])
            .read_to_end(&mut raw)
            .is_err()
        {
            bad += 1;
            continue;
        }
        let declared = dcol
            .and_then(|c| f.get(c))
            .and_then(|s| s.trim().parse::<usize>().ok());
        if declared.is_some_and(|d| d != raw.len()) {
            mismatch += 1;
        }
        let dir = p
            .parent()
            .and_then(|d| d.strip_prefix(base).ok())
            .unwrap_or(Path::new("."));
        sets.entry(format!("cram/{}", dir.display()))
            .or_default()
            .push(Stream { comp, raw });
    }
    if missing + bad + mismatch > 0 {
        eprintln!(
            "manifest: {missing} missing files, {bad} undecodable, {mismatch} declared-size mismatches (skipped/ignored)"
        );
    }
    sets
}

// ---- measuring ----

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn pass(dec: Dec, streams: &[Stream], buf: &mut [u8]) -> bool {
    streams
        .iter()
        .all(|s| dec(&s.comp, &mut buf[..s.raw.len()]).is_ok_and(|n| n == s.raw.len()))
}

/// Which decoders reproduce the original bytes on every stream.
fn verify(streams: &[Stream], buf: &mut [u8]) -> [Result<(), String>; 3] {
    let mut res = [const { Ok(()) }; 3];
    for (i, dec) in DECODERS.iter().enumerate() {
        for (k, s) in streams.iter().enumerate() {
            let out = &mut buf[..s.raw.len()];
            let r = dec(&s.comp, out).and_then(|n| {
                if n == s.raw.len() && *out == s.raw[..] {
                    Ok(())
                } else {
                    Err(format!("output differs (wrote {n} of {})", s.raw.len()))
                }
            });
            if let Err(e) = r {
                res[i] = Err(format!("stream {k}: {e}"));
                break;
            }
        }
    }
    res
}

/// Median MB/s (10^6 uncompressed bytes per second) per decoder over `rounds`
/// whole-set passes, the decoder order rotated every round. A pass repeats until
/// it has run `min_ms` so that small sets are not timer noise.
fn measure(
    streams: &[Stream],
    ok: [bool; 3],
    rounds: usize,
    min_ms: u128,
    best: bool,
) -> [Option<f64>; 3] {
    let total: usize = streams.iter().map(|s| s.raw.len()).sum();
    let mut buf = vec![0u8; streams.iter().map(|s| s.raw.len()).max().unwrap_or(0)];
    let mut samples: [Vec<f64>; 3] = Default::default();
    for r in 0..rounds {
        for k in 0..3 {
            let i = (k + r) % 3;
            if !ok[i] {
                continue;
            }
            let (t, mut reps) = (Instant::now(), 0u32);
            while reps == 0 || t.elapsed().as_millis() < min_ms {
                assert!(
                    pass(DECODERS[i], streams, &mut buf),
                    "{} failed mid-benchmark",
                    NAMES[i]
                );
                reps += 1;
            }
            samples[i].push(total as f64 * f64::from(reps) / t.elapsed().as_secs_f64() / 1e6);
        }
    }
    samples.map(|mut s| {
        (!s.is_empty()).then(|| {
            if best {
                s.iter().copied().fold(0.0, f64::max)
            } else {
                median(&mut s)
            }
        })
    })
}

struct Row {
    set: String,
    chunk: String,
    n: usize,
    bytes: usize,
    mbps: [Option<f64>; 3],
}

fn cell(v: Option<f64>, failed: &str) -> String {
    v.map_or_else(|| failed.to_string(), |v| format!("{v:.1}"))
}

fn ratio(a: Option<f64>, b: Option<f64>) -> String {
    match (a, b) {
        (Some(a), Some(b)) => format!("{:.2}x", a / b),
        _ => "-".into(),
    }
}

impl Row {
    fn md(&self) -> String {
        let [o, z, c] = self.mbps;
        format!(
            "| {} | {} | {} | {:.2} | {} | {} | {} | {} | {} |",
            self.set,
            self.chunk,
            self.n,
            self.bytes as f64 / 1e6,
            cell(o, "FAILED/stub"),
            cell(z, "FAILED"),
            cell(c, "FAILED"),
            ratio(o, c),
            ratio(o, z),
        )
    }
    fn tsv(&self) -> String {
        let f = |v: Option<f64>| v.map_or("NA".into(), |v| format!("{v:.2}"));
        let [o, z, c] = self.mbps;
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.set,
            self.chunk,
            self.n,
            self.bytes,
            f(o),
            f(z),
            f(c),
            ratio(o, c),
            ratio(o, z)
        )
    }
}

/// ns per call implied by a row's MB/s.
fn ns_per_call(r: &Row, i: usize) -> Option<f64> {
    r.mbps[i].map(|m| r.bytes as f64 / (m * 1e6) / r.n as f64 * 1e9)
}

// ---- CLI ----

struct Opts {
    sets: Vec<String>,
    chunks: Vec<String>,
    samples: Vec<String>,
    rounds: usize,
    level: u32,
    min_ms: u128,
    max_bytes: usize,
    data: PathBuf,
    manifest: PathBuf,
    tsv: Option<PathBuf>,
    decoders: Vec<String>,
    best: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: bz2ngrs-bench [--sets MC,XA,QUAL,cram] [--chunks 16k,64k,1m,whole,slices,blocks]\n\
         \x20  [--samples NA12878,NA19141] [--rounds 5] [--level 9] [--min-ms 30] [--max-bytes N (0 = all)]\n\
         \x20  [--data DIR] [--manifest FILE] [--tsv FILE] [--decoders ours,bzip2,C] [--stat median|max]"
    );
    std::process::exit(2)
}

fn parse_args() -> Opts {
    let list = |s: &str| s.split(',').map(str::to_string).collect::<Vec<_>>();
    let mut o = Opts {
        sets: list("MC,XA,QUAL,cram"),
        chunks: list("16k,64k,1m,whole,slices,blocks"),
        samples: list("NA12878"),
        rounds: 5,
        level: 9,
        min_ms: 30,
        max_bytes: 0,
        data: "/Users/pascal/code/tfenne/bz2-ng/data/corpora/1kg".into(),
        manifest: "/Users/pascal/Code/softleif/bz2ngrs/corpus/cram-bzip2/manifest.tsv".into(),
        tsv: None,
        decoders: list("ours,bzip2,C"),
        best: false,
    };
    let mut a = std::env::args().skip(1);
    while let Some(k) = a.next() {
        let Some(v) = a.next() else { usage() };
        let num = |v: &str| v.parse::<usize>().unwrap_or_else(|_| usage());
        match k.as_str() {
            "--sets" => o.sets = list(&v),
            "--chunks" => o.chunks = list(&v),
            "--samples" => o.samples = list(&v),
            "--rounds" | "-r" => o.rounds = num(&v).max(1),
            "--level" | "-l" => {
                o.level = u32::try_from(num(&v))
                    .unwrap_or_else(|_| usage())
                    .clamp(1, 9)
            }
            "--min-ms" => o.min_ms = num(&v) as u128,
            "--max-bytes" => o.max_bytes = num(&v),
            "--data" => o.data = v.into(),
            "--manifest" => o.manifest = v.into(),
            "--tsv" => o.tsv = Some(v.into()),
            "--decoders" => o.decoders = list(&v),
            "--stat" => o.best = v == "max",
            _ => usage(),
        }
    }
    o
}

fn main() {
    let o = parse_args();
    let want_set = |name: &str| o.sets.iter().any(|t| name.contains(t.as_str()));
    let want_chunk = |c: &str| o.chunks.iter().any(|t| t == c);

    let mut rows: Vec<Row> = Vec::new();
    let mut tsv = String::from(
        "set\tchunk\tstreams\tbytes\tours_MBps\tbzip2_MBps\tC_MBps\tours/C\tours/bzip2\n",
    );
    let mut first_err: [Option<String>; 3] = Default::default();
    println!(
        "level {}, {} rounds, MB/s of uncompressed data (MB = 10^6 B), {}\n",
        o.level,
        o.rounds,
        if o.best { "best" } else { "median" }
    );
    println!("| set | chunk | streams | MB | ours | bzip2 | C | ours/C | ours/bzip2 |");
    println!("|---|---|---:|---:|---:|---:|---:|---:|---:|");

    let mut run = |set: &str, chunk: &str, streams: Vec<Stream>| {
        let mut buf = vec![0u8; streams.iter().map(|s| s.raw.len()).max().unwrap_or(0)];
        let v = verify(&streams, &mut buf);
        let mut ok = NAMES.map(|n| o.decoders.iter().any(|d| d == n));
        for (i, r) in v.into_iter().enumerate() {
            if let Err(e) = r {
                ok[i] = false;
                if first_err[i].is_none() {
                    eprintln!(
                        "note: {} failed verification on {set}/{chunk}: {e}",
                        NAMES[i]
                    );
                    first_err[i] = Some(e);
                }
            }
        }
        let row = Row {
            set: set.into(),
            chunk: chunk.into(),
            n: streams.len(),
            bytes: streams.iter().map(|s| s.raw.len()).sum(),
            mbps: measure(&streams, ok, o.rounds, o.min_ms, o.best),
        };
        println!("{}", row.md());
        let _ = std::io::stdout().flush();
        tsv.push_str(&row.tsv());
        tsv.push('\n');
        rows.push(row);
    };

    for sample in &o.samples {
        for name in ["MC", "XA", "QUAL"] {
            let set = format!("{sample}/{name}");
            if !want_set(&set) {
                continue;
            }
            let dir = o.data.join(sample);
            let Ok(mut data) = fs::read(dir.join(format!("{name}.bin"))) else {
                eprintln!("skip {set}: {}/{name}.bin not found", dir.display());
                continue;
            };
            if o.max_bytes > 0 {
                data.truncate(o.max_bytes);
            }
            let kinds: [(&str, Option<Cuts>); 5] = [
                ("16k", Some(fixed_cuts(data.len(), 16 << 10))),
                ("64k", Some(fixed_cuts(data.len(), 64 << 10))),
                ("1m", Some(fixed_cuts(data.len(), 1 << 20))),
                ("whole", Some(vec![(0, data.len())])),
                (
                    "slices",
                    slice_cuts(&dir.join(format!("{name}.slices-10k.txt")), data.len()),
                ),
            ];
            for (label, cuts) in kinds {
                if !want_chunk(label) {
                    continue;
                }
                let Some(cuts) = cuts else {
                    eprintln!("skip {set}/{label}: no sidecar");
                    continue;
                };
                eprintln!("compressing {set}/{label} ({} streams)...", cuts.len());
                run(&set, label, compress_all(&data, &cuts, o.level));
            }
        }
    }
    if want_chunk("blocks")
        && o.sets
            .iter()
            .any(|t| "cram".contains(t.as_str()) || t.starts_with("cram/"))
    {
        for (name, streams) in load_manifest(&o.manifest) {
            if want_set(&name) {
                run(&name, "blocks", streams);
            }
        }
    }

    // Per-call cost: ns per call on the 16 KiB sets, and the empty stream (pure
    // init + teardown) as the fixed part.
    println!("\nPer-call cost, ns/call (ours | bzip2 | C):\n");
    println!("| stream | ours | bzip2 | C |");
    println!("|---|---:|---:|---:|");
    let ns = |v: Option<f64>, f: &str| v.map_or(f.to_string(), |v| format!("{v:.0}"));
    for r in rows.iter().filter(|r| r.chunk == "16k") {
        println!(
            "| {} 16 KiB | {} | {} | {} |",
            r.set,
            ns(ns_per_call(r, 0), "FAILED/stub"),
            ns(ns_per_call(r, 1), "FAILED"),
            ns(ns_per_call(r, 2), "FAILED")
        );
    }
    let empty = vec![Stream {
        comp: compress(&[], o.level),
        raw: Vec::new(),
    }];
    let mut buf = [0u8; 1];
    let v = verify(&empty, &mut buf);
    let mut cells = Vec::new();
    for (i, r) in v.iter().enumerate() {
        cells.push(match r {
            _ if !o.decoders.iter().any(|d| d == NAMES[i]) => "-".to_string(),
            Err(_) => (if i == 0 { "FAILED/stub" } else { "FAILED" }).to_string(),
            Ok(()) => {
                let mut best = f64::MAX;
                for _ in 0..o.rounds.max(3) {
                    let t = Instant::now();
                    for _ in 0..20_000 {
                        std::hint::black_box(
                            DECODERS[i](std::hint::black_box(&empty[0].comp), &mut buf).ok(),
                        );
                    }
                    best = best.min(t.elapsed().as_secs_f64() / 20_000.0 * 1e9);
                }
                format!("{best:.0}")
            }
        });
    }
    println!(
        "| empty stream ({} B), fixed cost | {} | {} | {} |",
        empty[0].comp.len(),
        cells[0],
        cells[1],
        cells[2]
    );

    if let Some(p) = &o.tsv {
        fs::write(p, tsv).expect("write tsv");
        eprintln!("wrote {}", p.display());
    }
}
