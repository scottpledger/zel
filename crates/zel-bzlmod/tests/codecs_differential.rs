//! Differential tests of the pure-Rust codecs (xz/LZMA2, zstd, bzip2, 7z) against the system
//! tools (`xz`, `zstd`, `bzip2`, `7zz`/`7z`/`7za`).  Tests skip (with a note on stderr) when a
//! tool is missing.  The default matrix is quick; set `ZEL_CODEC_FULL=1` to run the big matrix
//! (sizes up to 60 MB, all presets/levels).
//!
//! Extraction against *real Bazel* is covered by `tests/codec_bazel_diff.sh`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zel_bzlmod::codecs::{bunzip2, extract_7z, xz_decompress, zstd_decompress};

static COUNTER: AtomicUsize = AtomicUsize::new(0);
static CASES: AtomicUsize = AtomicUsize::new(0);

fn full() -> bool {
    std::env::var("ZEL_CODEC_FULL").map(|v| v == "1").unwrap_or(false)
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!("zel-codec-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

fn find_7z() -> Option<&'static str> {
    ["7zz", "7z", "7za"].into_iter().find(|t| {
        Command::new(t).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
    })
}

/// Runs `tool args... < input > output`; returns stdout when it exited 0.
fn run_filter(tool: &str, args: &[&str], input: &Path) -> Option<Vec<u8>> {
    let out = Command::new(tool)
        .args(args)
        .stdin(fs::File::open(input).unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if out.status.success() { Some(out.stdout) } else { None }
}

// ---------------------------------------------------------------------------
// Data generators
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for c in buf.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            c.copy_from_slice(&v[..c.len()]);
        }
    }
}

fn gen_random(n: usize, r: &mut Rng) -> Vec<u8> {
    let mut v = vec![0u8; n];
    r.fill(&mut v);
    v
}

fn gen_text(n: usize, r: &mut Rng) -> Vec<u8> {
    let vocab: Vec<Vec<u8>> = (0..3000)
        .map(|_| (0..1 + r.below(10)).map(|_| b'a' + r.below(26) as u8).collect())
        .collect();
    let mut v = Vec::with_capacity(n + 16);
    while v.len() < n {
        // zipf-ish
        let i = (r.below(3000) * r.below(3000) / 3000) as usize;
        v.extend_from_slice(&vocab[i]);
        v.push(if r.below(12) == 0 { b'\n' } else { b' ' });
    }
    v.truncate(n);
    v
}

fn gen_zeros(n: usize) -> Vec<u8> {
    vec![0u8; n]
}

fn gen_repeats(n: usize, r: &mut Rng) -> Vec<u8> {
    // a random block repeated with sparse mutations -> long distance matches
    let blk = (n / 3).clamp(1, 6 << 20);
    let base = gen_random(blk, r);
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        let start = v.len();
        v.extend_from_slice(&base[..blk.min(n - start)]);
        for _ in 0..(1 + blk / 50_000) {
            let i = start + r.below(blk as u64) as usize;
            if i < v.len() {
                v[i] ^= 1 + r.below(255) as u8;
            }
        }
    }
    v
}

fn gen_mixed(n: usize, r: &mut Rng) -> Vec<u8> {
    let mut v: Vec<u8> = Vec::with_capacity(n);
    while v.len() < n {
        let len = (1 + r.below(300_000) as usize).min(n - v.len());
        match r.below(5) {
            0 => v.extend(gen_random(len, r)),
            1 => v.extend(gen_text(len, r)),
            2 => v.extend(std::iter::repeat(0u8).take(len)),
            3 if v.len() > 10 => {
                let from = r.below(v.len() as u64) as usize;
                for k in 0..len {
                    let b = v[from + k % (v.len() - from)];
                    v.push(b);
                }
            }
            _ => v.extend(std::iter::repeat(r.below(256) as u8).take(len)),
        }
    }
    v.truncate(n);
    v
}

/// Machine-code-like data hitting every BCJ filter pattern.
fn gen_code(n: usize, r: &mut Rng) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 16);
    while v.len() < n {
        match r.below(8) {
            0 => {
                v.push(if r.below(2) == 0 { 0xE8 } else { 0xE9 });
                let rel = (r.below(1 << 20) as i32 - (1 << 19)) as u32;
                v.extend_from_slice(&rel.to_le_bytes());
            }
            1 => v.extend_from_slice(&[r.below(256) as u8, r.below(256) as u8, r.below(256) as u8, 0xEB]), // ARM BL
            2 => v.extend_from_slice(&[0x48 | r.below(4) as u8, r.below(256) as u8, r.below(256) as u8, (r.below(64) as u8) << 2 | 1]), // PPC
            3 => v.extend_from_slice(&[0x40, r.below(64) as u8, r.below(256) as u8, r.below(256) as u8]), // SPARC
            4 => v.extend_from_slice(&[r.below(256) as u8, 0xF0 | r.below(8) as u8, r.below(256) as u8, 0xF8 | r.below(8) as u8]), // thumb
            5 => {
                let mut b = [0u8; 16];
                r.fill(&mut b);
                b[0] = (b[0] & 0xE0) | [0x10, 0x11, 0x12, 0x16, 0x17, 0x18, 0x1C, 0x1D][r.below(8) as usize];
                v.extend_from_slice(&b);
            }
            _ => {
                let mut b = [0u8; 8];
                r.fill(&mut b);
                v.extend_from_slice(&b[..1 + r.below(8) as usize]);
            }
        }
    }
    v.truncate(n);
    v
}

fn gen_binary(n: usize, r: &mut Rng) -> Vec<u8> {
    // real compiled code: this very test binary, repeated/cut as needed
    let mut v = fs::read(std::env::current_exe().unwrap()).unwrap_or_default();
    if v.is_empty() {
        return gen_code(n, r);
    }
    while v.len() < n {
        let extra = gen_code(1 << 16, r);
        v.extend(extra);
    }
    v.truncate(n);
    v
}

fn kinds() -> &'static [&'static str] {
    &["random", "text", "mixed", "zeros", "repeats", "code", "binary"]
}

fn gen(kind: &str, n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(0x9E3779B97F4A7C15 ^ seed.wrapping_mul(0xA24BAED4963EE407) ^ n as u64);
    match kind {
        "random" => gen_random(n, &mut r),
        "text" => gen_text(n, &mut r),
        "mixed" => gen_mixed(n, &mut r),
        "zeros" => gen_zeros(n),
        "repeats" => gen_repeats(n, &mut r),
        "code" => gen_code(n, &mut r),
        _ => gen_binary(n, &mut r),
    }
}

fn sizes() -> Vec<usize> {
    let mut s = vec![0, 1, 2, 17, 255, 4096, 65_535, 131_073, 300_000, 1_500_000];
    if full() {
        s.extend([5_000_000, 12_000_000]);
    }
    s
}

/// Runs `f` with a timeout; reports panics and hangs as failures.
fn guarded<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(r);
        })
        .unwrap();
    match rx.recv_timeout(Duration::from_secs(if full() { 600 } else { 120 })) {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => panic!("PANIC in decoder: {what}"),
        Err(_) => panic!("HANG (timeout) in decoder: {what}"),
    }
}

fn check(name: &str, compressed: Vec<u8>, expect: &[u8], dec: fn(&[u8]) -> Result<Vec<u8>, String>, failures: &mut Vec<String>) {
    CASES.fetch_add(1, Ordering::Relaxed);
    let label = name.to_string();
    let exp_len = expect.len();
    let res = guarded(&label, move || dec(&compressed));
    match res {
        Ok(out) => {
            if out != expect {
                let first = out.iter().zip(expect).position(|(a, b)| a != b).unwrap_or(out.len().min(exp_len));
                failures.push(format!("{name}: output differs (got {} bytes, want {exp_len}, first diff at {first})", out.len()));
            }
        }
        Err(e) => failures.push(format!("{name}: decode error: {e}")),
    }
}

fn report(what: &str, failures: Vec<String>) {
    eprintln!("[{what}] cases so far: {}", CASES.load(Ordering::Relaxed));
    if !failures.is_empty() {
        for f in failures.iter().take(60) {
            eprintln!("FAIL {f}");
        }
        panic!("{what}: {} failures (of the shown above)", failures.len());
    }
}

// ---------------------------------------------------------------------------
// xz
// ---------------------------------------------------------------------------

fn xz_flag_sets(size: usize) -> Vec<Vec<&'static str>> {
    let mut v: Vec<Vec<&'static str>> = vec![
        vec![],
        vec!["-0"],
        vec!["-3"],
        vec!["-6"],
        vec!["--check=none"],
        vec!["--check=crc32"],
        vec!["--check=crc64"],
        vec!["--check=sha256"],
        vec!["--x86"],
        vec!["--arm"],
        vec!["--armthumb"],
        vec!["--powerpc"],
        vec!["--ia64"],
        vec!["--sparc"],
        vec!["--delta=dist=1", "--lzma2=preset=3"],
        vec!["--delta=dist=4", "--lzma2=preset=3"],
        vec!["--delta=dist=256", "--lzma2=preset=3"],
        vec!["--x86", "--delta=dist=2", "--lzma2=preset=4"],
        vec!["--lzma2=preset=4,lc=0,lp=4,pb=0"],
        vec!["--lzma2=preset=4,lc=4,lp=0,pb=4"],
        vec!["--lzma2=preset=1,dict=4KiB"],
        vec!["--block-size=65536"],
        vec!["--block-size=100000", "--check=sha256"],
        vec!["-T4", "--block-size=1MiB"],
    ];
    if size <= 5_000_000 {
        v.extend([vec!["-1"], vec!["-9"], vec!["-9e"], vec!["-e"], vec!["-T0", "-9"], vec!["--lzma2=preset=6,nice=273,mf=bt4,depth=1000"], vec!["--lzma2=preset=6,mode=fast"]]);
    } else if size <= 20_000_000 {
        v.extend([vec!["-9"], vec!["-1"], vec!["-T4"]]);
    }
    v
}

#[test]
fn xz_matrix() {
    if !have("xz") {
        eprintln!("SKIP xz_matrix: system xz not found");
        return;
    }
    let dir = tmp();
    let mut failures = Vec::new();
    let mut seed = 0;
    let big = full();
    for &n in &sizes() {
        for kind in kinds() {
            seed += 1;
            let data = gen(kind, n, seed);
            let inp = dir.join("in.bin");
            fs::write(&inp, &data).unwrap();
            let flagsets = xz_flag_sets(n);
            for (i, flags) in flagsets.iter().enumerate() {
                // quick mode: rotate through the flag sets so that each is exercised for several kinds/sizes
                if !big && n >= 300_000 && (i + seed as usize) % 3 != 0 {
                    continue;
                }
                if !big && n < 300_000 && (i + seed as usize) % 2 != 0 {
                    continue;
                }
                let f: Vec<&str> = flags.clone();
                let mut args = vec!["-c", "-z"];
                args.extend(f.iter());
                let Some(comp) = run_filter("xz", &args, &inp) else { continue };
                check(&format!("xz {kind} {n} {flags:?}"), comp, &data, xz_decompress, &mut failures);
            }
            // concatenated streams (+ stream padding) and a .tar.xz-like single big block
            if n <= 1_500_000 {
                let a = run_filter("xz", &["-c", "-z", "-1"], &inp).unwrap();
                let b = run_filter("xz", &["-c", "-z", "--check=none", "-6"], &inp).unwrap();
                let mut cat = a.clone();
                cat.extend_from_slice(&b);
                let mut want = data.clone();
                want.extend_from_slice(&data);
                // Bazel emulation (see codecs/xz.rs): like XZCompressorInputStream without
                // decompressConcatenated, only the FIRST stream is decoded; whatever follows its footer
                // (a second stream, stream padding) is never looked at. `want` (both) is therefore not expected.
                let _ = &want;
                check(&format!("xz concat {kind} {n}"), cat.clone(), &data, xz_decompress, &mut failures);
                cat.extend_from_slice(&[0u8; 8]);
                check(&format!("xz concat+padding {kind} {n}"), cat, &data, xz_decompress, &mut failures);
            }
        }
        report(&format!("xz size {n}"), std::mem::take(&mut failures));
    }
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// zstd
// ---------------------------------------------------------------------------

fn zstd_flag_sets(size: usize) -> Vec<Vec<&'static str>> {
    let mut v: Vec<Vec<&'static str>> = vec![
        vec!["-1"],
        vec!["-3"],
        vec!["-9"],
        vec!["--fast=5"],
        vec!["--fast=1000"],
        vec!["--no-check"],
        vec!["--long=27"],
        vec!["--long=31", "--zstd=wlog=31"],
        vec!["--zstd=wlog=10"],
        vec!["--zstd=wlog=10,strategy=1"],
        vec!["--zstd=strategy=9,wlog=14"],
        vec!["-5", "--zstd=minmatch=3"],
        vec!["--rsyncable"],
        vec!["-T4"],
        vec!["--no-content-size"],
        vec!["--zstd=ldmHashLog=20,minmatch=64", "--long"],
    ];
    if size <= 5_000_000 {
        v.extend([vec!["-15"], vec!["-19"], vec!["--ultra", "-22"], vec!["--ultra", "-20", "--zstd=wlog=24"], vec!["-12", "--zstd=targetLength=999"]]);
    } else if size <= 20_000_000 {
        v.extend([vec!["-19"], vec!["-15"]]);
    }
    v
}

fn skippable(payload: &[u8], variant: u8) -> Vec<u8> {
    let mut f = vec![0x50 + (variant & 15), 0x2A, 0x4D, 0x18];
    f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    f.extend_from_slice(payload);
    f
}

#[test]
fn zstd_matrix() {
    if !have("zstd") {
        eprintln!("SKIP zstd_matrix: system zstd not found");
        return;
    }
    let dir = tmp();
    let mut failures = Vec::new();
    let mut seed = 100;
    let big = full();
    for &n in &sizes() {
        for kind in kinds() {
            seed += 1;
            let data = gen(kind, n, seed);
            let inp = dir.join("in.bin");
            fs::write(&inp, &data).unwrap();
            for (i, flags) in zstd_flag_sets(n).iter().enumerate() {
                if !big && n >= 300_000 && (i + seed as usize) % 3 != 0 {
                    continue;
                }
                if !big && n < 300_000 && (i + seed as usize) % 2 != 0 {
                    continue;
                }
                let mut args = vec!["-c", "-q"];
                args.extend(flags.iter());
                // from stdin: no content size in the frame header
                let Some(comp) = run_filter("zstd", &args, &inp) else { continue };
                check(&format!("zstd {kind} {n} {flags:?} (stdin)"), comp, &data, zstd_decompress, &mut failures);
                // from file: frame content size present (single-segment for small inputs)
                if i % 4 == 0 {
                    let mut a2: Vec<&str> = args.clone();
                    let ip = inp.to_str().unwrap().to_string();
                    a2.push(&ip);
                    if let Ok(o) = Command::new("zstd").args(&a2).stderr(Stdio::null()).output() {
                        if o.status.success() {
                            check(&format!("zstd {kind} {n} {flags:?} (file)"), o.stdout, &data, zstd_decompress, &mut failures);
                        }
                    }
                }
            }
            if n <= 1_500_000 {
                // multi frame + skippable frames
                let a = run_filter("zstd", &["-c", "-q", "-1"], &inp).unwrap();
                let b = run_filter("zstd", &["-c", "-q", "-9", "--no-check"], &inp).unwrap();
                let mut want = data.clone();
                want.extend_from_slice(&data);
                let mut cat = a.clone();
                cat.extend_from_slice(&b);
                check(&format!("zstd 2 frames {kind} {n}"), cat, &want, zstd_decompress, &mut failures);
                let mut sk = skippable(b"hello skippable", 3);
                sk.extend_from_slice(&a);
                sk.extend(skippable(&[], 0));
                sk.extend_from_slice(&b);
                sk.extend(skippable(&vec![7u8; 100_000], 15));
                check(&format!("zstd skippable {kind} {n}"), sk, &want, zstd_decompress, &mut failures);
            }
        }
        report(&format!("zstd size {n}"), std::mem::take(&mut failures));
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zstd_dictionary_free_edge_cases() {
    // Hand-built frames: empty raw block, RLE block, checksum.
    // magic, FHD=0x20 (single segment, fcs 1 byte), FCS=0, last raw block size 0
    let f = [0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x00, 0x01, 0x00, 0x00];
    assert_eq!(zstd_decompress(&f).unwrap(), Vec::<u8>::new());
    // RLE block of 10 'a'
    let f = [0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x0A, 0x53, 0x00, 0x00, b'a'];
    assert_eq!(zstd_decompress(&f).unwrap(), vec![b'a'; 10]);
    // bad magic / truncated
    assert!(zstd_decompress(b"nope").is_err());
    assert!(zstd_decompress(&f[..8]).is_err());
}

// ---------------------------------------------------------------------------
// bzip2
// ---------------------------------------------------------------------------

#[test]
fn bzip2_matrix() {
    if !have("bzip2") {
        eprintln!("SKIP bzip2_matrix: system bzip2 not found");
        return;
    }
    let dir = tmp();
    let mut failures = Vec::new();
    let mut seed = 200;
    for &n in &sizes() {
        for kind in kinds() {
            seed += 1;
            if n > 5_000_000 && !(*kind == "mixed" || *kind == "zeros") {
                continue;
            }
            let data = gen(kind, n, seed);
            let inp = dir.join("in.bin");
            fs::write(&inp, &data).unwrap();
            for lvl in ["-1", "-5", "-9"] {
                let Some(comp) = run_filter("bzip2", &["-c", lvl], &inp) else { continue };
                check(&format!("bzip2 {kind} {n} {lvl}"), comp.clone(), &data, bunzip2, &mut failures);
                // Bazel emulation (codecs/bz2.rs): bytes after a stream that do not start another "BZh"
                // stream are an error ("Garbage after a valid BZip2 stream"), unlike the bzip2 tool.
                let mut g = comp.clone();
                g.extend_from_slice(b"garbage after stream");
                match bunzip2(&g) {
                    Err(e) if e.starts_with("Garbage after") => {}
                    other => failures.push(format!("bzip2 trailing garbage {kind} {n} {lvl}: expected Garbage error, got {:?}", other.map(|v| v.len()))),
                }
            }
            // two independent streams, and one stream cut in the middle of the data
            if n >= 2 {
                let half = n / 2;
                let p1 = dir.join("p1.bin");
                let p2 = dir.join("p2.bin");
                fs::write(&p1, &data[..half]).unwrap();
                fs::write(&p2, &data[half..]).unwrap();
                let mut cat = run_filter("bzip2", &["-c", "-9"], &p1).unwrap();
                cat.extend(run_filter("bzip2", &["-c", "-1"], &p2).unwrap());
                check(&format!("bzip2 multistream {kind} {n}"), cat, &data, bunzip2, &mut failures);
            }
        }
    }
    report("bzip2", failures);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// corruption / truncation robustness (never panic or hang; never accept wrong data)
// ---------------------------------------------------------------------------

#[test]
fn corruption_fuzz() {
    let mut r = Rng(0xC0FFEE);
    let dir = tmp();
    let data = gen("mixed", 400_000, 7);
    let inp = dir.join("in.bin");
    fs::write(&inp, &data).unwrap();
    let mut streams: Vec<(&str, Vec<u8>, fn(&[u8]) -> Result<Vec<u8>, String>)> = Vec::new();
    if have("xz") {
        streams.push(("xz-crc64", run_filter("xz", &["-c", "-3", "--check=crc64"], &inp).unwrap(), xz_decompress));
        streams.push(("xz-multi", run_filter("xz", &["-c", "-3", "--block-size=50000", "--check=sha256"], &inp).unwrap(), xz_decompress));
    }
    if have("zstd") {
        streams.push(("zstd", run_filter("zstd", &["-c", "-q", "-3"], &inp).unwrap(), zstd_decompress));
        streams.push(("zstd-19", run_filter("zstd", &["-c", "-q", "-19"], &inp).unwrap(), zstd_decompress));
    }
    if have("bzip2") {
        streams.push(("bzip2", run_filter("bzip2", &["-c", "-3"], &inp).unwrap(), bunzip2));
    }
    let iterations = if full() { 600 } else { 120 };
    let mut accepted_wrong = Vec::new();
    for (name, good, dec) in &streams {
        let dec = *dec;
        for it in 0..iterations {
            let mut bad = good.clone();
            let mode = r.below(4);
            match mode {
                0 => {
                    let i = r.below(bad.len() as u64) as usize;
                    bad[i] ^= 1 << r.below(8);
                }
                1 => {
                    let cut = r.below(bad.len() as u64) as usize;
                    bad.truncate(cut);
                }
                2 => {
                    let i = r.below(bad.len() as u64) as usize;
                    let n = (1 + r.below(16) as usize).min(bad.len() - i);
                    r.fill(&mut bad[i..i + n]);
                }
                _ => {
                    // flip bits in the first 64 bytes (headers / table descriptions)
                    let i = r.below(64.min(bad.len() as u64)) as usize;
                    bad[i] ^= 1 << r.below(8);
                }
            }
            let label = format!("{name} fuzz#{it} mode{mode}");
            let b2 = bad.clone();
            let res = guarded(&label, move || dec(&b2));
            if let Ok(out) = res {
                if out != data && mode != 1 {
                    accepted_wrong.push(format!("{label}: accepted corrupt input with wrong output"));
                }
                if out != data && mode == 1 {
                    accepted_wrong.push(format!("{label}: accepted truncated input with wrong output"));
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&dir);
    if !accepted_wrong.is_empty() {
        for a in accepted_wrong.iter().take(40) {
            eprintln!("FAIL {a}");
        }
        panic!("{} corrupt inputs were silently accepted", accepted_wrong.len());
    }
}

// ---------------------------------------------------------------------------
// 7z
// ---------------------------------------------------------------------------

fn opts() -> zel_bzlmod::archive::ExtractOptions {
    zel_bzlmod::archive::ExtractOptions {
        strip_prefix: String::new(),
        strip_components: 0,
        add_prefix: String::new(),
        rename_files: Default::default(),
        archive_type: None,
        allow_absolute: true,
    }
}

fn read_tree(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Option<Vec<u8>>)>) {
        let mut ents: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        ents.sort();
        for p in ents {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            let md = fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                out.push((rel, None));
                walk(base, &p, out);
            } else {
                out.push((rel, Some(fs::read(&p).unwrap())));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v
}

fn make_tree(root: &Path, seed: u64, big: bool) {
    fs::create_dir_all(root.join("sub/deep")).unwrap();
    fs::create_dir_all(root.join("emptydir")).unwrap();
    fs::write(root.join("empty.txt"), b"").unwrap();
    fs::write(root.join("e1"), b"x").unwrap();
    fs::write(root.join("sub/deep/f"), gen("text", 5000, seed)).unwrap();
    fs::write(root.join("mixed.bin"), gen("mixed", if big { 3_000_000 } else { 400_000 }, seed)).unwrap();
    fs::write(root.join("code.bin"), gen("code", 200_000, seed)).unwrap();
    fs::write(root.join("r1m"), gen("random", if big { 1_500_000 } else { 100_000 }, seed)).unwrap();
    fs::write(root.join("sub/zeros"), gen("zeros", 300_000, seed)).unwrap();
    for i in 0..40 {
        fs::write(root.join(format!("sub/small{i}.txt")), gen("text", 10 + i * 37, seed + i as u64)).unwrap();
    }
}

#[test]
fn sevenz_matrix() {
    let Some(sz) = find_7z() else {
        eprintln!("SKIP sevenz_matrix: no 7zz/7z/7za found");
        return;
    };
    let dir = tmp();
    let src = dir.join("src");
    make_tree(&src, 42, full());
    let expect = read_tree(&src);
    let methods: Vec<(&str, Vec<&str>, bool)> = vec![
        // (name, args, must_succeed)
        ("default", vec![], true),
        ("lzma", vec!["-m0=lzma"], true),
        ("lzma2", vec!["-m0=lzma2"], true),
        ("lzma-mx9", vec!["-m0=lzma", "-mx=9"], true),
        ("lzma2-mx1", vec!["-m0=lzma2", "-mx=1"], true),
        ("copy", vec!["-m0=copy"], true),
        ("bzip2", vec!["-m0=bzip2"], true),
        ("bzip2-mx9", vec!["-m0=bzip2", "-mx=9"], true),
        ("deflate", vec!["-m0=deflate"], true),
        ("deflate-mx9", vec!["-m0=deflate", "-mx=9"], true),
        ("nonsolid", vec!["-ms=off"], true),
        ("solid-e", vec!["-ms=e"], true),
        ("solid-1m", vec!["-ms=1m"], true),
        ("hc-off", vec!["-mhc=off"], true),
        ("he-off", vec!["-mhe=off"], true),
        ("bcj-lzma", vec!["-mf=BCJ", "-m1=lzma"], true),
        ("bcj-lzma2", vec!["-mf=BCJ", "-m1=lzma2"], true),
        ("bcj-arm", vec!["-mf=ARM"], true),
        ("bcj-armt", vec!["-mf=ARMT"], true),
        ("bcj-ppc", vec!["-mf=PPC"], true),
        ("bcj-sparc", vec!["-mf=SPARC"], true),
        ("bcj-ia64", vec!["-mf=IA64"], true),
        ("delta-4", vec!["-m0=Delta:4", "-m1=lzma2"], true),
        ("bcj-bzip2", vec!["-mf=BCJ", "-m1=bzip2"], true),
        ("mt-lzma2", vec!["-mmt=4", "-m0=lzma2", "-mx=5"], true),
        // allowed to fail (must then fail cleanly), never to produce wrong data:
        ("ppmd", vec!["-m0=ppmd"], false),
        ("bcj2", vec!["-mf=BCJ2"], false),
        ("deflate64", vec!["-m0=deflate64"], false),
        ("arm64", vec!["-mf=ARM64"], false),
    ];
    let mut failures = Vec::new();
    for (name, args, must) in methods {
        let arc = dir.join(format!("{name}.7z"));
        let mut cmd = Command::new(sz);
        cmd.current_dir(&src).arg("a").arg("-bd").arg("-y").args(&args).arg(&arc).arg(".");
        let o = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
        if !o.success() {
            eprintln!("note: 7z could not create {name} archive; skipping");
            continue;
        }
        CASES.fetch_add(1, Ordering::Relaxed);
        let raw = fs::read(&arc).unwrap();
        let out = dir.join(format!("out-{name}"));
        let (a2, o2) = (arc.clone(), out.clone());
        let res = guarded(name, move || extract_7z(&raw, &a2, &o2, &opts()));
        match res {
            Ok(()) => {
                let got = read_tree(&out);
                if got != expect {
                    failures.push(format!("7z {name}: extracted tree differs from source"));
                }
            }
            Err(e) => {
                if must {
                    failures.push(format!("7z {name}: unexpected error: {e}"));
                } else {
                    eprintln!("note: 7z {name}: clean error: {e}");
                }
            }
        }
    }
    // corruption: every bit flip must be detected (CRC32 of headers, folders and files)
    let arc = dir.join("lzma2-corrupt.7z");
    let _ = Command::new(sz).current_dir(&src).args(["a", "-bd", "-y", "-m0=bzip2", "-mx=1"]).arg(&arc).arg(".").stdout(Stdio::null()).stderr(Stdio::null()).status();
    if let Ok(good) = fs::read(&arc) {
        let mut r = Rng(99);
        let iters = if full() { 300 } else { 60 };
        for it in 0..iters {
            let mut bad = good.clone();
            let i = 8 + r.below(bad.len() as u64 - 8) as usize; // skip signature+version bytes
            bad[i] ^= 1 << r.below(8);
            let out = dir.join(format!("fuzz-out-{it}"));
            let (a2, o2) = (arc.clone(), out.clone());
            CASES.fetch_add(1, Ordering::Relaxed);
            let res = guarded("7z fuzz", move || extract_7z(&bad, &a2, &o2, &opts()));
            if res.is_ok() {
                failures.push(format!("7z bit flip at byte {i} was silently accepted"));
            }
            let _ = fs::remove_dir_all(&out);
        }
        // truncation
        for cut in [10usize, 31, 32, 100, good.len() / 2, good.len() - 1] {
            let (a2, o2) = (arc.clone(), dir.join("trunc-out"));
            let bad = good[..cut.min(good.len())].to_vec();
            CASES.fetch_add(1, Ordering::Relaxed);
            if guarded("7z trunc", move || extract_7z(&bad, &a2, &o2, &opts())).is_ok() {
                failures.push(format!("7z truncated to {cut} bytes accepted"));
            }
        }
    }
    report("7z", failures);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// performance (informational; asserts generous bounds)
// ---------------------------------------------------------------------------

#[test]
fn big_stream_performance() {
    if !full() {
        eprintln!("SKIP big_stream_performance (set ZEL_CODEC_FULL=1)");
        return;
    }
    let dir = tmp();
    let data = gen("mixed", 60_000_000, 5);
    let inp = dir.join("in.bin");
    fs::write(&inp, &data).unwrap();
    for (tool, args, dec) in [
        ("xz", vec!["-c", "-T4", "-3"], xz_decompress as fn(&[u8]) -> Result<Vec<u8>, String>),
        ("xz", vec!["-c", "-6"], xz_decompress),
        ("zstd", vec!["-c", "-q", "-3"], zstd_decompress),
        ("zstd", vec!["-c", "-q", "--long=27", "-12"], zstd_decompress),
        ("bzip2", vec!["-c", "-9"], bunzip2),
    ] {
        if !have(tool) {
            continue;
        }
        let comp = run_filter(tool, &args, &inp).unwrap();
        let t = Instant::now();
        let out = dec(&comp).unwrap();
        let el = t.elapsed();
        eprintln!("perf {tool} {args:?}: {} -> {} bytes in {:?}", comp.len(), out.len(), el);
        assert_eq!(out, data, "{tool} {args:?}");
        assert!(el < Duration::from_secs(120), "{tool} too slow: {el:?}");
    }
    let _ = fs::remove_dir_all(&dir);
}
