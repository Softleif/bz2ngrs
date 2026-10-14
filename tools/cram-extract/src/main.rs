//! Extract the raw bzip2 payloads of CRAM blocks (method 2, and arith-coded
//! blocks with an EXT/bzip2 body) into a corpus directory, and verify them.
//!
//!   cram-extract extract <in.cram> <source> <corpus-dir> [--max-containers N] [--max-bytes N] [--dry]
//!   cram-extract verify <corpus-dir>
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufReader, Read, Write},
    path::Path,
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn u8_(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

/// ITF8: leading 1 bits of the first byte = number of following bytes; 5th byte adds 4 bits.
fn itf8(r: &mut impl Read) -> io::Result<u32> {
    let first = u8_(r)?;
    let extra = first.leading_ones().min(4);
    let mask = if extra == 4 {
        0x0F
    } else {
        0xFF >> (extra + 1)
    };
    let mut v = u32::from(first) & mask;
    for i in 0..extra {
        let b = u32::from(u8_(r)?);
        v = if i == 3 {
            (v << 4) | (b & 0x0F)
        } else {
            (v << 8) | b
        };
    }
    Ok(v)
}

/// LTF8, value discarded (we never need it).
fn skip_ltf8(r: &mut impl Read) -> io::Result<()> {
    let first = u8_(r)?;
    let mut sink = [0u8; 8];
    r.read_exact(&mut sink[..first.leading_ones().min(8) as usize])
}

fn uint7(s: &mut &[u8]) -> Option<usize> {
    let mut v = 0usize;
    loop {
        let (&b, rest) = s.split_first()?;
        *s = rest;
        v = v.checked_mul(128)? | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
}

struct Entry {
    codec: &'static str,
    content_type: u8,
    content_id: u32,
    uncompressed_len: usize,
    data: Vec<u8>,
}

/// bzip2 body of an arith block, if it is the plain EXT case (no STRIPE/PACK/RLE/CAT).
fn arith_ext(data: &[u8]) -> Option<(usize, &[u8])> {
    let (&flags, mut rest) = data.split_first()?;
    // shortcut: nested EXT (STRIPE) and EXT+PACK/RLE blocks are skipped, they are rare.
    if flags & 0x04 == 0 || flags & (0x08 | 0x20 | 0x40 | 0x80) != 0 {
        return None;
    }
    let len = if flags & 0x10 == 0 {
        uint7(&mut rest)?
    } else {
        return None;
    };
    rest.starts_with(b"BZh").then_some((len, rest))
}

fn extract(args: &[String]) -> Res<()> {
    let [cram, source, out, opts @ ..] = args else {
        return Err("usage: extract <in.cram> <source> <corpus-dir> [--max-containers N] [--max-bytes N] [--dry]".into());
    };
    let (mut max_containers, mut max_bytes, mut dry) = (u64::MAX, u64::MAX, false);
    let mut it = opts.iter();
    while let Some(o) = it.next() {
        match o.as_str() {
            "--max-containers" => max_containers = it.next().ok_or("value")?.parse()?,
            "--max-bytes" => max_bytes = it.next().ok_or("value")?.parse()?,
            "--dry" => dry = true,
            _ => return Err(format!("unknown option {o}").into()),
        }
    }
    let mut f = BufReader::with_capacity(1 << 20, fs::File::open(cram)?);
    let mut def = [0u8; 26];
    f.read_exact(&mut def)?;
    if &def[..4] != b"CRAM" {
        return Err("not a CRAM file".into());
    }
    let major = def[4];
    let crc = major >= 3;
    eprintln!("CRAM {}.{}", major, def[5]);

    let dir = Path::new(out).join(source);
    let mut n = if dry {
        0
    } else {
        fs::read_dir(&dir).map_or(0, Iterator::count)
    };
    let mut manifest = None;
    if !dry {
        let mp = Path::new(out).join("manifest.tsv");
        let fresh = !mp.exists();
        let mut m = fs::OpenOptions::new().create(true).append(true).open(mp)?;
        if fresh {
            writeln!(
                m,
                "path\tsource\tcompressed_len\tuncompressed_len\tcodec\tcontent_type\tcontent_id"
            )?;
        }
        manifest = Some(m);
    }

    // method -> (blocks, compressed bytes)
    let mut methods: BTreeMap<u8, (u64, u64)> = BTreeMap::new();
    let (mut containers, mut bytes_read, mut written) = (0u64, 26u64, 0u64);
    while containers < max_containers && bytes_read < max_bytes {
        let mut l = [0u8; 4];
        match f.read_exact(&mut l) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let length = u32::from_le_bytes(l) as usize;
        for _ in 0..4 {
            itf8(&mut f)?; // ref id, start, span, n records
        }
        skip_ltf8(&mut f)?; // record counter
        skip_ltf8(&mut f)?; // bases
        let n_blocks = itf8(&mut f)?;
        for _ in 0..itf8(&mut f)? {
            itf8(&mut f)?; // landmarks
        }
        if crc {
            io::copy(&mut (&mut f).take(4), &mut io::sink())?;
        }
        let mut body = vec![0u8; length];
        f.read_exact(&mut body)?;
        bytes_read += length as u64 + 32; // approximate: header size ignored
        containers += 1;

        let mut b = &body[..];
        for _ in 0..n_blocks {
            let method = u8_(&mut b)?;
            let content_type = u8_(&mut b)?;
            let content_id = itf8(&mut b)?;
            let clen = itf8(&mut b)? as usize;
            let ulen = itf8(&mut b)? as usize;
            let (data, rest) = b.split_at_checked(clen).ok_or("truncated block")?;
            b = rest;
            if crc {
                b = b.get(4..).ok_or("truncated crc")?;
            }
            let e = methods.entry(method).or_default();
            e.0 += 1;
            e.1 += clen as u64;
            let entry = match method {
                2 => Some(Entry {
                    codec: "block",
                    content_type,
                    content_id,
                    uncompressed_len: ulen,
                    data: data.to_vec(),
                }),
                6 => arith_ext(data).map(|(len, body)| Entry {
                    codec: "arith-ext",
                    content_type,
                    content_id,
                    uncompressed_len: len,
                    data: body.to_vec(),
                }),
                _ => None,
            };
            let (Some(e), Some(m)) = (entry, manifest.as_mut()) else {
                if entry_counts_only(method, data) && dry {
                    written += 1;
                }
                continue;
            };
            let rel = format!("{source}/{n:06}.bz2");
            fs::create_dir_all(&dir)?;
            fs::write(Path::new(out).join(&rel), &e.data)?;
            writeln!(
                m,
                "{rel}\t{source}\t{}\t{}\t{}\t{}\t{}",
                e.data.len(),
                e.uncompressed_len,
                e.codec,
                e.content_type,
                e.content_id
            )?;
            n += 1;
            written += e.data.len() as u64;
        }
    }
    eprintln!("containers: {containers}; bytes read ~{bytes_read}");
    for (m, (c, bytes)) in &methods {
        eprintln!("  method {m}: {c} blocks, {bytes} bytes");
    }
    eprintln!(
        "bzip2 payload {} {written}",
        if dry { "blocks (dry)" } else { "bytes written" }
    );
    Ok(())
}

/// Dry-run helper: does this block hold bzip2?
fn entry_counts_only(method: u8, data: &[u8]) -> bool {
    method == 2 || (method == 6 && arith_ext(data).is_some())
}

fn verify(args: &[String]) -> Res<()> {
    let dir = Path::new(args.first().ok_or("usage: verify <corpus-dir>")?);
    let manifest = fs::read_to_string(dir.join("manifest.tsv"))?;
    // source -> (uncompressed lens, content types, total compressed)
    let mut per: BTreeMap<String, (Vec<usize>, BTreeMap<String, u64>, u64)> = BTreeMap::new();
    let mut bad = 0;
    for line in manifest.lines().skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        let [path, source, clen, ulen, _codec, ctype, _cid] = c[..] else {
            return Err(format!("bad row {line}").into());
        };
        let data = fs::read(dir.join(path))?;
        let (clen, ulen): (usize, usize) = (clen.parse()?, ulen.parse()?);
        let mut out = Vec::new();
        let r = bzip2::read::BzDecoder::new(&data[..]).read_to_end(&mut out);
        if data.len() != clen || r.is_err() || out.len() > ulen {
            bad += 1;
            eprintln!(
                "BAD {path}: file {} manifest {clen} decoded {} declared {ulen} {:?}",
                data.len(),
                out.len(),
                r.err()
            );
        }
        let e = per.entry(source.to_string()).or_default();
        e.0.push(ulen);
        *e.1.entry(ctype.to_string()).or_default() += 1;
        e.2 += clen as u64;
    }
    let (mut total_n, mut total_b) = (0, 0);
    for (s, (mut u, ct, bytes)) in per {
        u.sort_unstable();
        let q = |p: usize| u[(u.len() - 1) * p / 100];
        println!("{s}: {} blocks, {} MB compressed, uncompressed min/median/p90/max = {}/{}/{}/{}, content types {ct:?}",
            u.len(), bytes / 1_000_000, u[0], q(50), q(90), u[u.len() - 1]);
        total_n += u.len();
        total_b += bytes;
    }
    println!(
        "total: {total_n} blocks, {} MB compressed, {bad} bad",
        total_b / 1_000_000
    );
    if bad > 0 {
        return Err("verification failures".into());
    }
    Ok(())
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("extract") => extract(&args[1..]),
        Some("verify") => verify(&args[1..]),
        _ => Err("usage: cram-extract extract|verify ...".into()),
    }
}
