//! `fonts` pass: remove embedded fonts.
//!
//! CrossPoint ignores `@font-face` and always renders with its built-in fonts,
//! so the EPUB's fonts are dead weight. Removing them touches 4 places:
//! - the font files;
//! - the OPF manifest items (XML, via `quick-xml`);
//! - references in `META-INF/encryption.xml` (font obfuscation, XML);
//! - `@font-face` blocks in CSS (regex: `@font-face` never has nested braces).

use anyhow::{Result, bail};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

const FONT_EXTS: [&str; 4] = [".ttf", ".otf", ".woff", ".woff2"];
const FONT_MEDIA_TYPES: [&str; 4] = [
    "font/",
    "application/font-",
    "application/x-font-",
    "application/vnd.ms-opentype",
];

/// Compiled once, on first use.
static FONT_FACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)@font-face\s*\{[^}]*\}").unwrap());

pub fn is_font(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    FONT_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// The fonts of a book: files with a font extension, plus files the OPF
/// manifest declares as fonts by `media-type` alone (`href="f0001"
/// media-type="font/ttf"`). Every place that removes fonts asks this, so the
/// files, the manifest and `encryption.xml` stay consistent.
#[derive(Debug, Default)]
pub struct Fonts {
    /// Zip paths of manifest font items.
    declared: HashSet<String>,
}

impl Fonts {
    /// Add the font items of the OPF at `opf_path` (a zip path).
    pub fn add_manifest(&mut self, opf: &str, opf_path: &str) -> Result<()> {
        let base = opf_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let mut reader = Reader::from_str(opf);
        loop {
            match reader.read_event()? {
                Event::Eof => return Ok(()),
                Event::Start(e) | Event::Empty(e) if is_font_item(&e)? => {
                    if let Some(href) = e.try_get_attribute("href")? {
                        let href = href.normalized_value(XmlVersion::Implicit1_0)?;
                        if let Some(path) = resolve(base, &href) {
                            self.declared.insert(path);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Is the entry at this zip path a font?
    pub fn contains(&self, path: &str) -> bool {
        is_font(path) || self.declared.contains(path)
    }
}

/// Zip path of a manifest `href`, relative to the OPF directory `base`.
/// `None` for remote resources.
fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or_default();
    if href.contains("://") {
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

/// Decode `%XX` escapes. Invalid escapes or non-UTF-8 results are kept as is.
fn percent_decode(s: &str) -> String {
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

pub fn strip_font_faces(css: &str, _fonts: &Fonts) -> Result<Option<String>> {
    Ok(Some(FONT_FACE.replace_all(css, "").into_owned()))
}

/// A manifest `<item>` is a font if its `href` has a font extension
/// or its `media-type` is a font type.
fn is_font_item(e: &BytesStart) -> Result<bool> {
    if e.local_name().as_ref() != "item" {
        return Ok(false);
    }
    if let Some(href) = e.try_get_attribute("href")?
        && is_font(&href.normalized_value(XmlVersion::Implicit1_0)?)
    {
        return Ok(true);
    }
    if let Some(media) = e.try_get_attribute("media-type")? {
        let media = media
            .normalized_value(XmlVersion::Implicit1_0)?
            .to_ascii_lowercase();
        return Ok(FONT_MEDIA_TYPES.iter().any(|t| media.starts_with(t)));
    }
    Ok(false)
}

/// Remove font items from the manifest. Everything else in the OPF passes through untouched.
pub fn remove_manifest_items(opf: &str, _fonts: &Fonts) -> Result<Option<String>> {
    let mut reader = Reader::from_str(opf);
    let mut writer = Writer::new(Vec::new());
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            // `<item .../>`: just skip it.
            Event::Empty(e) if is_font_item(&e)? => {}
            // `<item ...></item>`: skip up to the closing tag.
            Event::Start(e) if is_font_item(&e)? => {
                reader.read_to_end(e.name())?;
            }
            event => writer.write_event(event)?,
        }
    }
    Ok(Some(String::from_utf8(writer.into_inner())?))
}

/// Remove `<EncryptedData>` blocks that point to fonts.
/// Returns `None` when no block is left, meaning the file can be dropped.
pub fn clean_encryption(xml: &str, fonts: &Fonts) -> Result<Option<String>> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Vec::new());
    let mut kept = 0;
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(start) if start.local_name().as_ref() == "EncryptedData" => {
                // Buffer the whole block before deciding whether it stays.
                let mut block = vec![Event::Start(start)];
                let mut points_to_font = false;
                loop {
                    let event = reader.read_event()?;
                    match &event {
                        Event::Start(e) | Event::Empty(e)
                            if e.local_name().as_ref() == "CipherReference" =>
                        {
                            if let Some(uri) = e.try_get_attribute("URI")? {
                                // URIs here are relative to the container root,
                                // i.e. zip paths.
                                let uri = uri.normalized_value(XmlVersion::Implicit1_0)?;
                                points_to_font |=
                                    resolve("", &uri).is_some_and(|path| fonts.contains(&path));
                            }
                        }
                        Event::Eof => bail!("encryption.xml: unclosed <EncryptedData>"),
                        _ => {}
                    }
                    let is_end = matches!(&event, Event::End(e) if e.local_name().as_ref() == "EncryptedData");
                    block.push(event);
                    if is_end {
                        break;
                    }
                }
                if !points_to_font {
                    kept += 1;
                    for event in block {
                        writer.write_event(event)?;
                    }
                }
            }
            event => writer.write_event(event)?,
        }
    }
    if kept == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8(writer.into_inner())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_fonts_case_insensitively() {
        assert!(is_font("fonts/00001.TTF"));
        assert!(is_font("OEBPS/UbuntuMono-Bold.otf"));
        assert!(is_font("a.woff2"));
        assert!(!is_font("styles/fonts.css"));
    }

    #[test]
    fn strips_font_face_and_keeps_the_rest() {
        let css = "body { margin: 0 }\n@font-face {\n  font-family: \"Charis\";\n  src: url(x.ttf);\n}\np { color: red }";
        let out = strip_font_faces(css, &Fonts::default()).unwrap().unwrap();
        assert_eq!(out, "body { margin: 0 }\n\np { color: red }");
    }

    #[test]
    fn removes_only_font_items_from_manifest() {
        let opf = r#"<?xml version="1.0"?><package><manifest>
<item href="a.css" id="c" media-type="text/css"/>
<item href="Font.otf" id="f" media-type="application/vnd.ms-opentype"/>
<item href="no-extension" id="g" media-type="font/ttf"></item>
<item href="img.png" id="i" media-type="image/png"/>
</manifest></package>"#;
        let out = remove_manifest_items(opf, &Fonts::default())
            .unwrap()
            .unwrap();
        assert!(!out.contains("Font.otf") && !out.contains("no-extension"));
        assert!(out.contains(r#"<item href="a.css" id="c" media-type="text/css"/>"#));
        assert!(out.contains(r#"<item href="img.png" id="i" media-type="image/png"/>"#));
        assert!(out.starts_with(r#"<?xml version="1.0"?>"#));
    }

    #[test]
    fn opf_without_fonts_is_unchanged() {
        let opf = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package version=\"3.0\">\n  <metadata><dc:title>A &amp; B</dc:title></metadata>\n  <manifest><item href=\"a.xhtml\" id=\"a\" media-type=\"application/xhtml+xml\"/></manifest>\n</package>";
        assert_eq!(
            remove_manifest_items(opf, &Fonts::default())
                .unwrap()
                .unwrap(),
            opf
        );
    }

    #[test]
    fn encryption_with_only_fonts_is_dropped() {
        let xml = r#"<encryption xmlns:enc="x"><enc:EncryptedData><enc:CipherData>
<enc:CipherReference URI="fonts/00001.ttf"/></enc:CipherData></enc:EncryptedData></encryption>"#;
        assert_eq!(clean_encryption(xml, &Fonts::default()).unwrap(), None);
    }

    #[test]
    fn encryption_with_other_entries_is_kept() {
        let xml = r#"<encryption><EncryptedData><CipherReference URI="a.ttf"/></EncryptedData><EncryptedData><CipherReference URI="ch01.xhtml"/></EncryptedData></encryption>"#;
        let out = clean_encryption(xml, &Fonts::default())
            .unwrap()
            .expect("still has encrypted content");
        assert_eq!(
            out,
            r#"<encryption><EncryptedData><CipherReference URI="ch01.xhtml"/></EncryptedData></encryption>"#
        );
    }

    #[test]
    fn broken_xml_is_an_error() {
        assert!(remove_manifest_items("<package><manifest></package>", &Fonts::default()).is_err());
    }

    #[test]
    fn manifest_declares_fonts_without_extension() {
        let opf = r#"<package><manifest>
<item href="fonts/f0001" id="f" media-type="font/ttf"/>
<item href="../Misc/My%20Font.otf" id="g" media-type="application/vnd.ms-opentype"></item>
<item href="ch01.xhtml" id="c" media-type="application/xhtml+xml"/>
</manifest></package>"#;
        let mut fonts = Fonts::default();
        fonts.add_manifest(opf, "OEBPS/content.opf").unwrap();
        assert!(fonts.contains("OEBPS/fonts/f0001"));
        assert!(fonts.contains("Misc/My Font.otf"));
        assert!(!fonts.contains("OEBPS/ch01.xhtml"));
        assert!(
            !fonts.contains("fonts/f0001"),
            "resolved against the OPF directory"
        );
    }

    #[test]
    fn encryption_entry_for_declared_font_is_dropped() {
        let mut fonts = Fonts::default();
        fonts
            .add_manifest(
                r#"<package><manifest><item href="f0001" media-type="font/otf"/></manifest></package>"#,
                "OEBPS/content.opf",
            )
            .unwrap();
        let xml = r#"<encryption><EncryptedData><CipherReference URI="OEBPS/f0001"/></EncryptedData></encryption>"#;
        assert_eq!(clean_encryption(xml, &fonts).unwrap(), None);
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
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
