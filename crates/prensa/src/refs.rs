//! References between entries: resolving an `href` to a zip path, and
//! rewriting references to images that were renamed.
//!
//! CrossPoint picks an image decoder from the file extension, so an image
//! converted to another format must be renamed (`fig.webp` -> `fig.jpg`), and
//! every reference to it updated: `src`/`href` attributes in XHTML and SVG,
//! manifest items in the OPF, `url(...)` in CSS and `style` attributes.
//!
//! References to an image that was turned 90° lose their `width`/`height`
//! attributes and inline CSS sizes, which were set for the original
//! orientation.
//!
//! Edits replace or remove attributes (and `url(...)` arguments) in place;
//! every other byte of the document is kept, so the element tree and the text
//! never change.

use crate::passes::Outcome;
use anyhow::{Result, bail};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// `url(...)` in CSS, with the argument in group 1 (double quotes), 2 (single
/// quotes) or 3 (unquoted).
static CSS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^)'"\s]*))\s*\)"#).unwrap()
});

/// Zip path of an `href`, relative to the directory `base`. `None` for remote
/// resources. Fragments and queries are dropped and `%XX` escapes decoded.
pub(crate) fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or_default();
    if href.contains("://") || href.starts_with("data:") {
        return None;
    }
    let href = percent_decode(href);
    let (start, href) = match href.strip_prefix('/') {
        Some(rest) => ("", rest),
        None => (base, href.as_str()),
    };
    let mut parts: Vec<&str> = start.split('/').filter(|p| !p.is_empty()).collect();
    for part in href.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Directory of a zip path: `OEBPS/text/ch1.xhtml` -> `OEBPS/text`.
pub(crate) fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Decode `%XX` escapes. Invalid escapes or non-UTF-8 results are kept as is.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// Images renamed because their format changed, by old zip path.
#[derive(Debug, Default)]
pub(crate) struct Renames {
    by_old_path: HashMap<String, Renamed>,
}

#[derive(Debug)]
struct Renamed {
    /// New zip path, in the same directory.
    path: String,
    /// What replaces the old extension in the file name, e.g. `.jpg` or
    /// `-1.png`: the stem before it never changes.
    tail: String,
    media_type: &'static str,
}

impl Renames {
    pub(crate) fn is_empty(&self) -> bool {
        self.by_old_path.is_empty()
    }

    /// Rename the image at `old` to the extension `ext` (e.g. `jpg`), unless
    /// it already has it. Picks a name no entry in `taken` has, comparing
    /// without case (memory cards are case-insensitive), and adds it to
    /// `taken`. Returns the new path.
    pub(crate) fn add(
        &mut self,
        old: &str,
        ext: &str,
        media_type: &'static str,
        taken: &mut HashSet<String>,
    ) -> String {
        let (dir_and_stem, old_ext) = old.rsplit_once('.').unwrap_or((old, ""));
        if old_ext.eq_ignore_ascii_case(ext) {
            return old.to_owned();
        }
        let mut tail = format!(".{ext}");
        let mut n = 1;
        while taken.contains(&format!("{dir_and_stem}{tail}").to_lowercase()) {
            tail = format!("-{n}.{ext}");
            n += 1;
        }
        let path = format!("{dir_and_stem}{tail}");
        taken.insert(path.to_lowercase());
        self.by_old_path.insert(
            old.to_owned(),
            Renamed {
                path: path.clone(),
                tail,
                media_type,
            },
        );
        path
    }

    /// New zip path of the image at `old`, if it was renamed.
    pub(crate) fn new_path(&self, old: &str) -> Option<&str> {
        self.by_old_path.get(old).map(|r| r.path.as_str())
    }

    /// The new `href` for a reference from a document in `base` (a directory),
    /// or `None` if it does not point to a renamed image.
    ///
    /// Only the extension at the end of the path changes: directories,
    /// escapes, the fragment and the query are kept as written.
    fn new_href(&self, base: &str, href: &str) -> Option<String> {
        let renamed = self.by_old_path.get(&resolve(base, href)?)?;
        let path_end = href.find(['#', '?']).unwrap_or(href.len());
        let (path, rest) = href.split_at(path_end);
        // The extension is plain ASCII in every real book; if it is escaped
        // (`%2Ewebp`), leave the reference alone rather than guess.
        let dot = path.rfind('.').filter(|&i| !path[i..].contains('/'))?;
        Some(format!("{}{}{rest}", &path[..dot], renamed.tail))
    }

    /// New media type for a renamed manifest item.
    fn media_type(&self, base: &str, href: &str) -> Option<&'static str> {
        Some(self.by_old_path.get(&resolve(base, href)?)?.media_type)
    }

    /// Rewrite `url(...)` references in CSS from a document in `base`.
    /// `None` if nothing pointed to a renamed image.
    fn rewrite_css_urls(&self, base: &str, css: &str) -> Option<String> {
        let mut changed = false;
        let out = CSS_URL.replace_all(css, |caps: &Captures| {
            let arg = caps.get(1).or(caps.get(2)).or(caps.get(3));
            match arg.and_then(|arg| self.new_href(base, arg.as_str()).map(|new| (arg, new))) {
                Some((arg, new)) => {
                    changed = true;
                    let whole = caps.get(0).unwrap();
                    let (before, after) = (
                        &whole.as_str()[..arg.start() - whole.start()],
                        &whole.as_str()[arg.end() - whole.start()..],
                    );
                    format!("{before}{new}{after}")
                }
                None => caps[0].to_owned(),
            }
        });
        changed.then(|| out.into_owned())
    }
}

/// Rewrite references to renamed images in a CSS file at `entry`.
pub(crate) fn rewrite_css(css: &str, entry: &str, renames: &Renames) -> Result<Outcome> {
    Ok(match renames.rewrite_css_urls(dir_of(entry), css) {
        Some(css) => Outcome::Replace(css),
        None => Outcome::Keep,
    })
}

/// Rewrite references to changed images in an XHTML or SVG document at
/// `entry`: `src`, `href` and `xlink:href` attributes, and `url(...)` in
/// `style` attributes, follow renamed images; an element showing a turned
/// image (zip paths in `rotated`, before renaming) loses its sizes.
pub(crate) fn rewrite_document(
    xhtml: &str,
    entry: &str,
    renames: &Renames,
    rotated: &HashSet<String>,
) -> Result<Outcome> {
    let base = dir_of(entry);
    edit_attributes(xhtml, |_, attrs| {
        let shows_rotated = attrs.iter().any(|(name, value)| {
            matches!(name.as_str(), "src" | "href")
                && resolve(base, value).is_some_and(|path| rotated.contains(&path))
        });
        let mut edits = Vec::new();
        for (i, (name, value)) in attrs.iter().enumerate() {
            let edit = match name.as_str() {
                "src" | "href" => renames.new_href(base, value).map(Edit::Set),
                "width" | "height" if shows_rotated => Some(Edit::Remove),
                "style" => {
                    let css = renames.rewrite_css_urls(base, value);
                    let current = css.as_deref().unwrap_or(value);
                    match (shows_rotated.then(|| without_sizes(current)).flatten(), css) {
                        (Some(stripped), _) if stripped.trim().is_empty() => Some(Edit::Remove),
                        (Some(stripped), _) => Some(Edit::Set(stripped)),
                        (None, css) => css.map(Edit::Set),
                    }
                }
                _ => None,
            };
            edits.extend(edit.map(|edit| (i, edit)));
        }
        edits
    })
}

/// `style` attribute text without its `width` and `height` declarations, or
/// `None` if it has none. `max-width` and the like stay.
fn without_sizes(style: &str) -> Option<String> {
    let is_size = |decl: &&str| {
        let property = decl.split(':').next().unwrap_or_default().trim();
        property.eq_ignore_ascii_case("width") || property.eq_ignore_ascii_case("height")
    };
    let decls: Vec<&str> = style.split(';').collect();
    if !decls.iter().any(|d| d.contains(':') && is_size(d)) {
        return None;
    }
    let kept: Vec<&str> = decls
        .into_iter()
        .filter(|d| !(d.contains(':') && is_size(d)))
        .collect();
    Some(kept.join(";"))
}

/// Point manifest items of renamed images in the OPF at `entry` to the new
/// files, with their new media type.
pub(crate) fn rewrite_opf(opf: &str, entry: &str, renames: &Renames) -> Result<Outcome> {
    let base = dir_of(entry);
    edit_attributes(opf, |element, attrs| {
        if element != "item" {
            return Vec::new();
        }
        let Some(href) = attrs.iter().find(|(n, _)| n == "href").map(|(_, v)| v) else {
            return Vec::new();
        };
        let (Some(new_href), Some(media_type)) =
            (renames.new_href(base, href), renames.media_type(base, href))
        else {
            return Vec::new();
        };
        attrs
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _))| match name.as_str() {
                "href" => Some((i, Edit::Set(new_href.clone()))),
                "media-type" => Some((i, Edit::Set(media_type.to_owned()))),
                _ => None,
            })
            .collect()
    })
}

/// What to do with one attribute.
#[derive(Debug, PartialEq)]
enum Edit {
    /// Replace its value with this unescaped text.
    Set(String),
    /// Remove the attribute, with the whitespace before it.
    Remove,
}

/// Edit attributes in an XML document, keeping every other byte.
///
/// `edit` is called for each start or empty tag with the element's local name
/// and its attributes as (local name, unescaped value), and returns edits by
/// attribute index. [`Outcome::Keep`] if nothing changed.
///
/// End tags are not matched against start tags: only attributes are edited,
/// so HTML-style chapters (`<br>` never closed) are handled too.
fn edit_attributes(
    xml: &str,
    mut edit: impl FnMut(&str, &[(String, String)]) -> Vec<(usize, Edit)>,
) -> Result<Outcome> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    // Byte ranges to replace, in document order.
    let mut splices: Vec<(usize, usize, String)> = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let (raws, attrs) = attributes(&e)?;
                for (i, edit) in edit(e.local_name().as_ref(), &attrs) {
                    let raw = &raws[i];
                    let (start, end) = span_in(xml, raw.value)?;
                    splices.push(match edit {
                        Edit::Set(new) => (start, end, quick_xml::escape::escape(new).into_owned()),
                        Edit::Remove => {
                            let (key_start, _) = span_in(xml, raw.key)?;
                            // Back over the whitespace before the name; on to
                            // the closing quote after the value.
                            let from = xml[..key_start].trim_end().len();
                            let to = end + xml[end..].chars().next().map_or(0, char::len_utf8);
                            (from, to, String::new())
                        }
                    });
                }
            }
            _ => {}
        }
    }
    if splices.is_empty() {
        return Ok(Outcome::Keep);
    }
    let mut out = String::with_capacity(xml.len());
    let mut copied = 0;
    for (start, end, value) in splices {
        out.push_str(&xml[copied..start]);
        out.push_str(&value);
        copied = end;
    }
    out.push_str(&xml[copied..]);
    Ok(Outcome::Replace(out))
}

/// An attribute as written in the document: its name and raw (escaped) value.
struct RawAttr<'t> {
    key: &'t str,
    value: &'t str,
}

/// The attributes of a tag: each one as written in the document, and
/// (local name, unescaped value) pairs, index for index.
///
/// An attribute whose value cannot be unescaped (an entity such as `&nbsp;`
/// with no DTD to define it) is left out: it is never edited, and the rest of
/// the document still is.
fn attributes<'t>(tag: &'t BytesStart) -> Result<(Vec<RawAttr<'t>>, Pairs)> {
    let mut raws = Vec::new();
    let mut attrs = Vec::new();
    for attr in tag.attributes() {
        let attr = attr?;
        let Ok(value) = attr.normalized_value(XmlVersion::Implicit1_0) else {
            continue;
        };
        let value = value.into_owned();
        let name = attr.key.local_name().as_ref().to_owned();
        // A reader over a `&str` borrows every raw name and value from it.
        let Cow::Borrowed(raw) = attr.value else {
            bail!("attribute value is not borrowed from the document");
        };
        raws.push(RawAttr {
            key: attr.key.0,
            value: raw,
        });
        attrs.push((name, value));
    }
    Ok((raws, attrs))
}

/// (local name, unescaped value) of each attribute of a tag.
type Pairs = Vec<(String, String)>;

/// Byte range of `part`, a slice borrowed from `whole`.
fn span_in(whole: &str, part: &str) -> Result<(usize, usize)> {
    let start = (part.as_ptr() as usize).wrapping_sub(whole.as_ptr() as usize);
    match whole.get(start..start.saturating_add(part.len())) {
        Some(slice) if std::ptr::eq(slice, part) => Ok((start, start + part.len())),
        _ => bail!("attribute value is not part of the document"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renames(old: &[&str], ext: &str) -> Renames {
        let mut renames = Renames::default();
        let mut taken: HashSet<String> = old.iter().map(|p| p.to_lowercase()).collect();
        for path in old {
            renames.add(path, ext, "image/jpeg", &mut taken);
        }
        renames
    }

    fn replaced(outcome: Outcome) -> String {
        match outcome {
            Outcome::Replace(text) => text,
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[test]
    fn resolves_hrefs() {
        assert_eq!(resolve("OEBPS", "a.ttf").as_deref(), Some("OEBPS/a.ttf"));
        assert_eq!(
            resolve("OEBPS/text", "../fonts/a.ttf").as_deref(),
            Some("OEBPS/fonts/a.ttf")
        );
        assert_eq!(resolve("", "./a%20b.ttf#x").as_deref(), Some("a b.ttf"));
        assert_eq!(resolve("OEBPS", "/root.ttf").as_deref(), Some("root.ttf"));
        assert_eq!(resolve("OEBPS", "https://example.com/a.ttf"), None);
        assert_eq!(resolve("OEBPS", "data:image/png;base64,AAAA"), None);
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn new_names_avoid_existing_entries() {
        let mut renames = Renames::default();
        let mut taken: HashSet<String> = ["a/fig.webp", "a/fig.bmp", "a/FIG.JPG", "a/x.jpg"]
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        assert_eq!(
            renames.add("a/fig.webp", "jpg", "image/jpeg", &mut taken),
            "a/fig-1.jpg"
        );
        assert_eq!(
            renames.add("a/fig.bmp", "jpg", "image/jpeg", &mut taken),
            "a/fig-2.jpg"
        );
        // Already the right extension: not a rename.
        assert_eq!(
            renames.add("a/x.jpg", "jpg", "image/jpeg", &mut taken),
            "a/x.jpg"
        );
        assert!(renames.new_href("a", "x.jpg").is_none());
    }

    #[test]
    fn new_href_keeps_directories_escapes_and_fragment() {
        let r = renames(&["OEBPS/images/my fig.webp"], "jpg");
        assert_eq!(
            r.new_href("OEBPS/text", "../images/my%20fig.webp#part")
                .as_deref(),
            Some("../images/my%20fig.jpg#part")
        );
        assert_eq!(r.new_href("OEBPS/text", "../images/other.webp"), None);
        assert_eq!(r.new_href("OEBPS/text", "my%20fig.webp"), None);
    }

    #[test]
    fn rewrites_only_the_attribute_values() {
        let r = renames(&["OEBPS/img/a.webp", "OEBPS/img/b.bmp"], "jpg");
        let xhtml = "<?xml version=\"1.0\"?>\n<!-- a.webp -->\n<html><body>\n  <p>See <img  src='../img/a.webp'\n alt=\"a.webp &amp; more\"/> and a.webp.</p>\n  <svg:image xlink:href=\"../img/b.bmp\"/><a href=\"ch2.xhtml\">next</a>\n  <div style=\"background: url(../img/a.webp)\"></div>\n</body></html>";
        let out =
            replaced(rewrite_document(xhtml, "OEBPS/text/ch1.xhtml", &r, &HashSet::new()).unwrap());
        assert_eq!(
            out,
            "<?xml version=\"1.0\"?>\n<!-- a.webp -->\n<html><body>\n  <p>See <img  src='../img/a.jpg'\n alt=\"a.webp &amp; more\"/> and a.webp.</p>\n  <svg:image xlink:href=\"../img/b.jpg\"/><a href=\"ch2.xhtml\">next</a>\n  <div style=\"background: url(../img/a.jpg)\"></div>\n</body></html>"
        );
    }

    #[test]
    fn escaped_values_are_unescaped_then_escaped_again() {
        let r = renames(&["R&D.webp"], "jpg");
        let out = replaced(
            rewrite_document(
                r#"<img src="R&amp;D.webp"/>"#,
                "x.xhtml",
                &r,
                &HashSet::new(),
            )
            .unwrap(),
        );
        assert_eq!(out, r#"<img src="R&amp;D.jpg"/>"#);
    }

    #[test]
    fn document_without_renamed_images_is_kept() {
        let r = renames(&["OEBPS/a.webp"], "jpg");
        let xhtml = r#"<html><body><img src="b.png"/></body></html>"#;
        assert_eq!(
            rewrite_document(xhtml, "OEBPS/ch.xhtml", &r, &HashSet::new()).unwrap(),
            Outcome::Keep
        );
    }

    #[test]
    fn broken_document_is_an_error() {
        let r = renames(&["a.webp"], "jpg");
        assert!(
            rewrite_document("<p><img src=\"a.webp\"</p>", "x.xhtml", &r, &HashSet::new()).is_err()
        );
    }

    #[test]
    fn html_style_void_tags_are_fine() {
        let r = renames(&["a.webp"], "jpg");
        let html = "<p>One<br>two <img src=\"a.webp\"></p>";
        let out = replaced(rewrite_document(html, "x.html", &r, &HashSet::new()).unwrap());
        assert_eq!(out, "<p>One<br>two <img src=\"a.jpg\"></p>");
    }

    #[test]
    fn unknown_entity_skips_only_that_attribute() {
        let r = renames(&["a.webp"], "jpg");
        let xhtml = r#"<img alt="a&nbsp;b" src="a.webp"/>"#;
        let out = replaced(rewrite_document(xhtml, "x.xhtml", &r, &HashSet::new()).unwrap());
        assert_eq!(out, r#"<img alt="a&nbsp;b" src="a.jpg"/>"#);
    }

    fn rotated(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn turned_image_loses_its_sizes() {
        let xhtml = "<p><img class=\"f\" width=\"2391\" src=\"../img/wide.png\"\n   height='1000' style=\"width: 90%; max-width:100%;height:20em\" alt=\"\"/><img src=\"../img/tall.png\" width=\"300\"/></p>";
        let out = replaced(
            rewrite_document(
                xhtml,
                "OEBPS/text/ch.xhtml",
                &Renames::default(),
                &rotated(&["OEBPS/img/wide.png"]),
            )
            .unwrap(),
        );
        assert_eq!(
            out,
            "<p><img class=\"f\" src=\"../img/wide.png\" style=\" max-width:100%\" alt=\"\"/><img src=\"../img/tall.png\" width=\"300\"/></p>"
        );
    }

    #[test]
    fn style_with_only_sizes_is_removed() {
        let out = replaced(
            rewrite_document(
                r#"<img src="w.webp" style="width:600px;height:300px"/>"#,
                "x.xhtml",
                &renames(&["w.webp"], "jpg"),
                &rotated(&["w.webp"]),
            )
            .unwrap(),
        );
        assert_eq!(out, r#"<img src="w.jpg"/>"#);
    }

    #[test]
    fn sizes_are_kept_when_nothing_turned() {
        let xhtml = r#"<img src="a.png" width="900" style="width:50%"/>"#;
        assert_eq!(
            rewrite_document(xhtml, "x.xhtml", &Renames::default(), &rotated(&["b.png"])).unwrap(),
            Outcome::Keep
        );
    }

    #[test]
    fn rewrites_manifest_href_and_media_type() {
        let r = renames(&["OEBPS/images/a.webp"], "jpg");
        let opf = r#"<package><manifest>
<item id="a" href="images/a.webp" media-type="image/webp" properties="cover-image"/>
<item id="b" href="images/b.png" media-type="image/png"/>
</manifest></package>"#;
        let out = replaced(rewrite_opf(opf, "OEBPS/content.opf", &r).unwrap());
        assert_eq!(
            out,
            r#"<package><manifest>
<item id="a" href="images/a.jpg" media-type="image/jpeg" properties="cover-image"/>
<item id="b" href="images/b.png" media-type="image/png"/>
</manifest></package>"#
        );
    }

    #[test]
    fn rewrites_css_urls_in_every_quoting() {
        let r = renames(&["OEBPS/img/a.webp"], "jpg");
        let css = "a { background: url(\"../img/a.webp\") } b { background: url( '../img/a.webp' ) } c { background: url(../img/a.webp) } d { background: url(../img/z.webp) }";
        let out = replaced(rewrite_css(css, "OEBPS/css/s.css", &r).unwrap());
        assert_eq!(
            out,
            "a { background: url(\"../img/a.jpg\") } b { background: url( '../img/a.jpg' ) } c { background: url(../img/a.jpg) } d { background: url(../img/z.webp) }"
        );
        assert_eq!(rewrite_css("p {}", "s.css", &r).unwrap(), Outcome::Keep);
    }
}
