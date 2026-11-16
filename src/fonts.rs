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

pub fn strip_font_faces(css: &str) -> Result<Option<String>> {
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
pub fn remove_manifest_items(opf: &str) -> Result<Option<String>> {
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
pub fn clean_encryption(xml: &str) -> Result<Option<String>> {
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
                                points_to_font |=
                                    is_font(&uri.normalized_value(XmlVersion::Implicit1_0)?);
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
        let out = strip_font_faces(css).unwrap().unwrap();
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
        let out = remove_manifest_items(opf).unwrap().unwrap();
        assert!(!out.contains("Font.otf") && !out.contains("no-extension"));
        assert!(out.contains(r#"<item href="a.css" id="c" media-type="text/css"/>"#));
        assert!(out.contains(r#"<item href="img.png" id="i" media-type="image/png"/>"#));
        assert!(out.starts_with(r#"<?xml version="1.0"?>"#));
    }

    #[test]
    fn opf_without_fonts_is_unchanged() {
        let opf = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package version=\"3.0\">\n  <metadata><dc:title>A &amp; B</dc:title></metadata>\n  <manifest><item href=\"a.xhtml\" id=\"a\" media-type=\"application/xhtml+xml\"/></manifest>\n</package>";
        assert_eq!(remove_manifest_items(opf).unwrap().unwrap(), opf);
    }

    #[test]
    fn encryption_with_only_fonts_is_dropped() {
        let xml = r#"<encryption xmlns:enc="x"><enc:EncryptedData><enc:CipherData>
<enc:CipherReference URI="fonts/00001.ttf"/></enc:CipherData></enc:EncryptedData></encryption>"#;
        assert_eq!(clean_encryption(xml).unwrap(), None);
    }

    #[test]
    fn encryption_with_other_entries_is_kept() {
        let xml = r#"<encryption><EncryptedData><CipherReference URI="a.ttf"/></EncryptedData><EncryptedData><CipherReference URI="ch01.xhtml"/></EncryptedData></encryption>"#;
        let out = clean_encryption(xml)
            .unwrap()
            .expect("still has encrypted content");
        assert_eq!(
            out,
            r#"<encryption><EncryptedData><CipherReference URI="ch01.xhtml"/></EncryptedData></encryption>"#
        );
    }

    #[test]
    fn broken_xml_is_an_error() {
        assert!(remove_manifest_items("<package><manifest></package>").is_err());
    }
}
