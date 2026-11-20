//! Pure-Rust archive decompressor codecs with zero external crates.

pub mod bz2;
pub mod lzma;
pub mod sevenz;
pub mod xz;
pub mod zstd;

pub use bz2::bunzip2;
pub use sevenz::extract_7z;
pub use xz::{xz_decompress, xz_decompress_concatenated};
pub use zstd::zstd_decompress;

use std::cell::RefCell;
thread_local! {
    /// Decoded bytes produced before the most recent decoder error (used to emulate Bazel's streaming
    /// readers, which only fail once the consumer actually reads up to the corrupt spot).
    static PARTIAL: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}
pub fn stash_partial(v: Vec<u8>) {
    PARTIAL.with(|p| *p.borrow_mut() = Some(v));
}
pub fn take_partial() -> Option<Vec<u8>> {
    PARTIAL.with(|p| p.borrow_mut().take())
}
