//! What happens to each zip entry: the action [`decide`] picks for it, and
//! what a text rewrite returns.
//!
//! The passes themselves (`fonts`, `images`) are pure transforms with no zip
//! code; `lib.rs` reads and writes the entries.

use crate::fonts::{self, Fonts};
use crate::images;

/// What to do with each zip entry.
pub(crate) enum Action {
    /// Already written (`mimetype`, written first).
    Skip,
    /// Dropped and counted in the report.
    RemoveFont,
    CopyRaw,
    Rewrite(Rewrite, NotUtf8),
    Image(images::Kind),
}

/// A text rewrite: the entry's text in, what to do with the entry out.
pub(crate) type Rewrite = fn(&str, &Fonts) -> anyhow::Result<Outcome>;

/// Result of a [`Rewrite`].
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Nothing to change: the entry is copied as stored, byte for byte.
    Keep,
    /// Write this text instead.
    Replace(String),
    /// Leave the entry out of the output.
    Drop,
}

/// What to do with a text entry that is not valid UTF-8.
pub(crate) enum NotUtf8 {
    /// Copy it unchanged. Fine when skipping the rewrite leaves the book
    /// consistent: a leftover `@font-face` points to a missing file, which
    /// readers already handle by falling back to their own fonts.
    CopyRaw,
    /// Fail the book. Copying would list removed fonts in the manifest or in
    /// `encryption.xml`.
    Reject,
}

pub(crate) fn decide(name: &str, fonts: &Fonts) -> Action {
    let lower = name.to_ascii_lowercase();
    if name == "mimetype" {
        Action::Skip
    } else if fonts.contains(name) {
        Action::RemoveFont
    } else if lower.ends_with(".css") {
        Action::Rewrite(fonts::strip_font_faces, NotUtf8::CopyRaw)
    } else if lower.ends_with(".opf") {
        Action::Rewrite(fonts::remove_manifest_items, NotUtf8::Reject)
    } else if lower == "meta-inf/encryption.xml" {
        Action::Rewrite(fonts::clean_encryption, NotUtf8::Reject)
    } else if let Some(kind) = images::kind_of(name) {
        Action::Image(kind)
    } else {
        Action::CopyRaw
    }
}
