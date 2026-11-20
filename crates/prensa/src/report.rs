//! What [`optimize`](crate::optimize) tells the caller: progress along the
//! way, then a report of what changed and what went wrong.

use std::fmt;

/// What [`optimize`](crate::optimize) did to the book.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Report {
    /// Embedded font files removed.
    pub fonts_removed: usize,
    /// Size of the removed fonts, in bytes (compressed, as stored in the EPUB).
    pub font_bytes: u64,
    /// Images rewritten.
    pub images_changed: usize,
    /// Images converted to another format, because the reader cannot decode
    /// theirs (WebP, BMP) or because it is smaller (GIF to PNG). Counted in
    /// `images_changed` too. The file is renamed (`fig.webp` becomes
    /// `fig.jpg`) and every reference to it updated.
    pub images_converted: usize,
    /// Images turned 90° so that a wide image fills the screen's height.
    /// Counted in `images_changed` too.
    pub images_rotated: usize,
    /// Images moved out of chapters, where they were embedded as `data:`
    /// URIs that CrossPoint does not show, into entries of their own. Each is
    /// also counted in `images_changed` or `images_kept`.
    pub images_extracted: usize,
    /// Images kept as they were because rewriting gained nothing.
    pub images_kept: usize,
    /// Images copied unchanged because they could not be rewritten (failed to
    /// decode, or too large to read). Each one also has a warning.
    pub images_failed: usize,
    /// Size of all images before, in bytes (compressed, as stored in the EPUB).
    pub image_bytes_before: u64,
    /// Size of all images after, in bytes (as stored in the EPUB).
    pub image_bytes_after: u64,
    /// Problems that did not stop the book, e.g. an image that failed to
    /// decode and was copied unchanged. One per problem.
    pub warnings: Vec<Warning>,
}

/// A problem that did not stop the book. `Display` gives `entry: message`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Warning {
    /// Path of the entry inside the EPUB.
    pub entry: String,
    /// What went wrong and what prensa did about it, e.g. "failed to decode;
    /// copied unchanged".
    pub message: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.entry, self.message)
    }
}

/// Where [`optimize_with_progress`](crate::optimize_with_progress) is in the book.
///
/// Reported before each entry, so `done` goes from `0` to `total - 1`; there is
/// no call with `done == total`. The book is finished when
/// [`optimize_with_progress`](crate::optimize_with_progress) returns.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Progress<'a> {
    /// Entries already processed.
    pub done: usize,
    /// Entries in the book (files and directories).
    pub total: usize,
    /// Path of the entry about to be processed.
    pub entry: &'a str,
}
