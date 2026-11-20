//! Pure-Rust Zstandard decompressor (RFC 8878), delegating to `zel_util::zstd`.

pub use zel_util::zstd::xxh64;

pub fn zstd_decompress(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    match zel_util::zstd::decompress_into(input, &mut out) {
        Ok(()) => Ok(out),
        Err(e) => {
            super::stash_partial(out);
            Err(e)
        }
    }
}
