//! Images embedded in chapters as `data:` URIs.
//!
//! `<img src="data:image/png;base64,...">` puts the whole image inside the
//! chapter. CrossPoint only loads images from files, so these never show, and
//! the images pass, which works on zip entries, never sees them. Each one is
//! moved to a file next to the first chapter that has it (`ch1.xhtml` ->
//! `ch1-image-1.png`), listed in the manifest, and every attribute that
//! embedded it points to that file instead.

use crate::images::{self, Kind};
use crate::refs::{dir_of, relative_href};
use crate::{Options, Warning};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// `</manifest>`, with any namespace prefix in group 1.
static MANIFEST_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</([\w.-]+:)?manifest\s*>").unwrap());

/// An image moved out of a chapter into its own entry.
#[derive(Debug)]
pub(crate) struct Extracted {
    /// Zip path of the new entry.
    pub(crate) path: String,
    pub(crate) media_type: &'static str,
    pub(crate) bytes: Vec<u8>,
    /// Size of the image as it was inside the chapter (decoded).
    pub(crate) original_len: usize,
    /// The images pass rewrote it (rather than keeping the decoded bytes).
    pub(crate) changed: bool,
    pub(crate) rotated: bool,
}

/// Every image extracted from the book's chapters.
#[derive(Debug, Default)]
pub(crate) struct Inline {
    /// Extracted file by `data:` URI: the same image embedded in several
    /// places becomes one file.
    by_uri: HashMap<String, usize>,
    pub(crate) files: Vec<Extracted>,
}

impl Inline {
    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The file a `data:` URI was moved to.
    pub(crate) fn get(&self, uri: &str) -> Option<&Extracted> {
        Some(&self.files[*self.by_uri.get(uri)?])
    }

    /// Extract the images in the `data:` URIs `uris` of `chapter`, running
    /// the images pass on each. Names avoid every entry in `taken`. An image
    /// that cannot be read stays where it is, with a warning.
    pub(crate) fn extract(
        &mut self,
        chapter: &str,
        uris: impl IntoIterator<Item = String>,
        opts: &Options,
        taken: &mut HashSet<String>,
        warnings: &mut Vec<Warning>,
    ) {
        let stem = chapter
            .rsplit('/')
            .next()
            .unwrap_or(chapter)
            .rsplit_once('.')
            .map_or(chapter, |(stem, _)| stem);
        let dir = dir_of(chapter);
        let mut n = 0;
        for uri in uris {
            if self.get(&uri).is_some() {
                continue;
            }
            let Some((kind, data)) = decode_data_uri(&uri) else {
                continue;
            };
            let (bytes, format_ext, media_type, changed, rotated) =
                match images::optimize(kind, &data, opts) {
                    Ok(Some(image)) => (
                        image.bytes,
                        image.format.extension(),
                        image.format.media_type(),
                        true,
                        image.rotated,
                    ),
                    Ok(None) => {
                        let (ext, media_type) = kind.extension_and_media_type();
                        (data.clone(), ext, media_type, false, false)
                    }
                    Err(e) => {
                        warnings.push(Warning {
                            entry: chapter.to_owned(),
                            message: format!(
                                "embedded image: {}; left in the chapter",
                                e.to_string().trim_end()
                            ),
                        });
                        continue;
                    }
                };
            let path = loop {
                n += 1;
                let name = format!("{stem}-image-{n}.{format_ext}");
                let path = if dir.is_empty() {
                    name
                } else {
                    format!("{dir}/{name}")
                };
                if taken.insert(path.to_lowercase()) {
                    break path;
                }
            };
            self.by_uri.insert(uri, self.files.len());
            self.files.push(Extracted {
                path,
                media_type,
                original_len: data.len(),
                bytes,
                changed,
                rotated,
            });
        }
    }
}

/// The image in a `data:image/...;base64,...` URI. `None` for anything else:
/// another media type (SVG stays inline), no base64, or bad base64.
pub(crate) fn decode_data_uri(uri: &str) -> Option<(Kind, Vec<u8>)> {
    let rest = uri.strip_prefix("data:")?;
    let (header, payload) = rest.split_once(',')?;
    let mut parts = header.split(';');
    let kind = match parts.next()?.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Kind::Jpeg,
        "image/png" => Kind::Png,
        "image/gif" => Kind::Gif,
        "image/webp" => Kind::Webp,
        "image/bmp" => Kind::Bmp,
        _ => return None,
    };
    if !parts.any(|p| p.trim().eq_ignore_ascii_case("base64")) {
        return None;
    }
    let payload: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD.decode(payload).ok()?;
    Some((kind, bytes))
}

/// Add a manifest item for every extracted image to the OPF at `entry`,
/// before its `</manifest>`. `None` if there is nothing to add.
pub(crate) fn manifest_items(opf: &str, entry: &str, inline: &Inline) -> Option<String> {
    if inline.is_empty() {
        return None;
    }
    let end = MANIFEST_END.captures(opf)?;
    let whole = end.get(0)?;
    let prefix = end.get(1).map_or("", |p| p.as_str());
    let mut items = String::new();
    for file in &inline.files {
        let base_id = format!(
            "prensa-{}",
            file.path.rsplit('/').next().unwrap_or(&file.path)
        )
        .replace(
            |c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "-",
        );
        let mut id = base_id.clone();
        let mut n = 1;
        while opf.contains(&format!("id=\"{id}\"")) || opf.contains(&format!("id='{id}'")) {
            n += 1;
            id = format!("{base_id}-{n}");
        }
        let href = relative_href(dir_of(entry), &file.path);
        items.push_str(&format!(
            "<{prefix}item id=\"{id}\" href=\"{}\" media-type=\"{}\"/>",
            quick_xml::escape::escape(href.as_str()),
            file.media_type
        ));
    }
    let mut out = String::with_capacity(opf.len() + items.len());
    out.push_str(&opf[..whole.start()]);
    out.push_str(&items);
    out.push_str(&opf[whole.start()..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn png_uri(w: u32, h: u32) -> String {
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(image::GrayImage::new(w, h))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        format!("data:image/png;base64,{}", STANDARD.encode(png))
    }

    #[test]
    fn decodes_base64_images_only() {
        let (kind, bytes) = decode_data_uri("data:image/png;base64,iVBO\n Rw0K").unwrap();
        assert_eq!(kind, Kind::Png);
        assert_eq!(bytes, b"\x89PNG\r\n");
        assert_eq!(
            decode_data_uri("data:IMAGE/JPEG;charset=x;BASE64,AAAA")
                .unwrap()
                .0,
            Kind::Jpeg
        );
        assert!(decode_data_uri("data:image/svg+xml;base64,AAAA").is_none());
        assert!(decode_data_uri("data:image/png,raw").is_none());
        assert!(decode_data_uri("data:image/png;base64,***").is_none());
        assert!(decode_data_uri("images/a.png").is_none());
    }

    #[test]
    fn extracts_next_to_the_chapter_once_per_uri() {
        let mut inline = Inline::default();
        let mut taken: HashSet<String> = ["oebps/text/ch1-image-1.png".to_owned()].into();
        let mut warnings = Vec::new();
        let uri = png_uri(4, 4);
        inline.extract(
            "OEBPS/text/ch1.xhtml",
            [
                uri.clone(),
                uri.clone(),
                "data:image/png;base64,AAAA".to_owned(),
            ],
            &Options::default(),
            &mut taken,
            &mut warnings,
        );
        // The same URI twice: one file. The name taken: the next one.
        assert_eq!(inline.files.len(), 1);
        assert_eq!(inline.files[0].path, "OEBPS/text/ch1-image-2.png");
        assert_eq!(inline.get(&uri).unwrap().path, "OEBPS/text/ch1-image-2.png");
        // "AAAA" decodes but is not a PNG.
        let [warning] = warnings.as_slice() else {
            panic!("expected one warning, got {warnings:?}");
        };
        assert!(
            warning.message.ends_with("left in the chapter"),
            "{warning}"
        );
    }

    #[test]
    fn adds_manifest_items_relative_to_the_opf() {
        let mut inline = Inline::default();
        inline.extract(
            "OEBPS/Text Files/ch1.xhtml",
            [png_uri(2, 2)],
            &Options::default(),
            &mut HashSet::new(),
            &mut Vec::new(),
        );
        let opf = r#"<opf:package><opf:manifest><opf:item id="prensa-ch1-image-1-png" href="x"/></opf:manifest></opf:package>"#;
        let out = manifest_items(opf, "OEBPS/content.opf", &inline).unwrap();
        assert_eq!(
            out,
            r#"<opf:package><opf:manifest><opf:item id="prensa-ch1-image-1-png" href="x"/><opf:item id="prensa-ch1-image-1-png-2" href="Text%20Files/ch1-image-1.png" media-type="image/png"/></opf:manifest></opf:package>"#
        );
        assert!(manifest_items(opf, "OEBPS/content.opf", &Inline::default()).is_none());
    }
}
