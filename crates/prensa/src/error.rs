//! The library's error type.

use std::error::Error as StdError;
use std::io;

/// Boxed cause of an [`Error`]. Boxing keeps the crates prensa uses internally
/// (`zip`, `quick-xml`) out of the public API, so upgrading them is not a
/// breaking change.
pub type Source = Box<dyn StdError + Send + Sync + 'static>;

/// Why [`optimize`](crate::optimize) failed.
///
/// Problems that do not stop the book (an image that fails to decode) are not
/// errors; they end up in [`Report::warnings`](crate::Report::warnings).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The input is not a zip archive, or its zip structure is damaged.
    #[error("input is not a valid EPUB (zip archive)")]
    InvalidZip(#[source] Source),

    /// An entry the optimizer must rewrite could not be parsed, e.g. a broken
    /// OPF. The book is rejected rather than written half-consistent (a font
    /// removed but still listed in the manifest).
    #[error("could not process {entry}")]
    Malformed {
        /// Path of the entry inside the EPUB.
        entry: String,
        #[source]
        source: Source,
    },

    /// An [`Options`](crate::Options) value is out of range, e.g. a maximum
    /// image width of `0`. Checked before anything is read or written.
    #[error("invalid options: {0}")]
    InvalidOptions(String),

    /// Reading the input or writing the output failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Convert a `zip` error without exposing `zip` types in the public API.
///
/// This is a function, not `impl From<ZipError> for Error`: trait impls are
/// public, so a `From` impl would make `zip` part of prensa's API.
pub(crate) fn from_zip(e: zip::result::ZipError) -> Error {
    match e {
        zip::result::ZipError::Io(e) => Error::Io(e),
        e => Error::InvalidZip(Box::new(e)),
    }
}
