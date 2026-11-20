//! What happens to each zip entry: the action [`decide`] picks for it, and
//! what a text rewrite returns.
//!
//! The passes themselves (`fonts`, `images`, `refs`) are pure transforms with
//! no zip code; `lib.rs` reads and writes the entries.

use crate::fonts::{self, Fonts};
use crate::images;
use crate::refs::{self, Renames};
use anyhow::Result;

/// What the passes know about the whole book, gathered before any entry is
/// written.
#[derive(Debug, Default)]
pub(crate) struct Book {
    /// Embedded fonts, to remove along with every reference to them.
    pub(crate) fonts: Fonts,
    /// Images converted to another format, whose references must follow.
    pub(crate) renames: Renames,
}

/// What to do with each zip entry.
pub(crate) enum Action {
    /// Already written (`mimetype`, written first).
    Skip,
    /// Dropped and counted in the report.
    RemoveFont,
    CopyRaw,
    Rewrite(Rewrite, Fallback),
    Image(images::Kind),
}

/// A text rewrite: the entry's text and zip path in, what to do with the
/// entry out.
pub(crate) type Rewrite = fn(&str, &str, &Book) -> Result<Outcome>;

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

impl Outcome {
    /// Run `next` on the text this outcome leaves (`original` if nothing
    /// changed) and combine both: the entry is replaced if either step
    /// changed it.
    fn then(self, original: &str, next: impl FnOnce(&str) -> Result<Outcome>) -> Result<Outcome> {
        Ok(match self {
            Outcome::Drop => Outcome::Drop,
            Outcome::Keep => next(original)?,
            Outcome::Replace(text) => match next(&text)? {
                Outcome::Keep => Outcome::Replace(text),
                other => other,
            },
        })
    }
}

/// What to do when an entry cannot be rewritten: it is not valid UTF-8, or
/// the rewrite fails (broken XML).
pub(crate) enum Fallback {
    /// Copy it unchanged. Fine when skipping the rewrite leaves the book
    /// consistent: a leftover `@font-face` points to a missing file, which
    /// readers already handle by falling back to their own fonts.
    Copy,
    /// Copy it unchanged and warn: references in it to converted images are
    /// left pointing at files that no longer exist.
    CopyWithWarning,
    /// Fail the book. Copying would list removed fonts in the manifest or in
    /// `encryption.xml`.
    Reject,
}

pub(crate) fn decide(name: &str, book: &Book) -> Action {
    let lower = name.to_ascii_lowercase();
    let is_document = [".xhtml", ".html", ".htm", ".svg"]
        .iter()
        .any(|ext| lower.ends_with(ext));
    if name == "mimetype" {
        Action::Skip
    } else if book.fonts.contains(name) {
        Action::RemoveFont
    } else if lower.ends_with(".css") {
        Action::Rewrite(rewrite_css, Fallback::Copy)
    } else if lower.ends_with(".opf") {
        Action::Rewrite(rewrite_opf, Fallback::Reject)
    } else if lower == "meta-inf/encryption.xml" {
        Action::Rewrite(
            |xml, _, book| fonts::clean_encryption(xml, &book.fonts),
            Fallback::Reject,
        )
    } else if let Some(kind) = images::kind_of(name) {
        Action::Image(kind)
    } else if is_document && !book.renames.is_empty() {
        Action::Rewrite(
            |doc, entry, book| refs::rewrite_document(doc, entry, &book.renames),
            Fallback::CopyWithWarning,
        )
    } else {
        Action::CopyRaw
    }
}

fn rewrite_css(css: &str, entry: &str, book: &Book) -> Result<Outcome> {
    fonts::strip_font_faces(css, &book.fonts)?
        .then(css, |css| refs::rewrite_css(css, entry, &book.renames))
}

fn rewrite_opf(opf: &str, entry: &str, book: &Book) -> Result<Outcome> {
    fonts::remove_manifest_items(opf, &book.fonts)?
        .then(opf, |opf| refs::rewrite_opf(opf, entry, &book.renames))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replace(text: &str) -> Result<Outcome> {
        Ok(Outcome::Replace(format!("{text}+")))
    }

    fn keep(_: &str) -> Result<Outcome> {
        Ok(Outcome::Keep)
    }

    #[test]
    fn then_combines_both_steps() {
        assert_eq!(Outcome::Keep.then("a", keep).unwrap(), Outcome::Keep);
        assert_eq!(
            Outcome::Keep.then("a", replace).unwrap(),
            Outcome::Replace("a+".into())
        );
        assert_eq!(
            Outcome::Replace("b".into()).then("a", keep).unwrap(),
            Outcome::Replace("b".into())
        );
        assert_eq!(
            Outcome::Replace("b".into()).then("a", replace).unwrap(),
            Outcome::Replace("b+".into())
        );
        assert_eq!(Outcome::Drop.then("a", replace).unwrap(), Outcome::Drop);
    }
}
