use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::fmt::Write;
use std::io::{Cursor, Read, Seek, SeekFrom};
use thiserror::Error;
use zip::ZipArchive;

pub mod dwg;
pub mod dxf;
pub mod format;
pub mod raster;

const MAX_ARCHIVE_ENTRIES: usize = 256;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
// ponytail: 200k covers dense title blocks and schedules; expose user-configured budgets before supporting larger sheets.
const MAX_ELEMENTS: usize = 200_000;
const MAX_SVG_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum Error {
    #[error("DWFX archive has too many entries")]
    TooManyEntries,
    #[error("DWFX archive expands beyond the configured limit")]
    TooLarge,
    #[error("DWFX archive has no FixedPage")]
    MissingFixedPage,
    #[error("DWFX XML is invalid: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("DWFX archive is invalid: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("DWFX page uses more elements than allowed")]
    TooManyElements,
    #[error("rendered SVG exceeds the configured limit")]
    SvgTooLarge,
    #[error("invalid numeric attribute: {0}")]
    Number(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    pub page_path: String,
    pub width: f64,
    pub height: f64,
    pub paths: usize,
    pub glyphs: usize,
    pub canvases: usize,
    pub text: Vec<String>,
    pub warnings: Vec<String>,
}

fn decode_entities(value: &str) -> String {
    let mut decoded = value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    for _ in 0..2 {
        let mut output = String::new();
        let mut rest = decoded.as_str();
        while let Some(start) = rest.find("&#") {
            output.push_str(&rest[..start]);
            let tail = &rest[start + 2..];
            let Some(end) = tail.find(';') else {
                output.push_str(&rest[start..]);
                break;
            };
            let token = &tail[..end];
            let value = token
                .strip_prefix('x')
                .or_else(|| token.strip_prefix('X'))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| token.parse().ok());
            if let Some(value) = value.and_then(char::from_u32) {
                output.push(value);
            } else {
                output.push_str(&rest[start..start + 2 + end + 1]);
            }
            rest = &tail[end + 1..];
        }
        output.push_str(rest);
        if output == decoded {
            break;
        }
        decoded = output;
    }
    decoded
}

fn attr(start: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>, Error> {
    for item in start.attributes().with_checks(false) {
        let item = item.map_err(|error| Error::Xml(error.into()))?;
        if item.key.as_ref() == name {
            return Ok(Some(decode_entities(&String::from_utf8_lossy(
                item.value.as_ref(),
            ))));
        }
    }
    Ok(None)
}

fn number(value: Option<String>, field: &str) -> Result<f64, Error> {
    value
        .ok_or_else(|| Error::Number(field.to_owned()))?
        .parse()
        .map_err(|_| Error::Number(field.to_owned()))
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xps_transform(value: Option<String>) -> Option<String> {
    let values: Vec<f64> = value?
        .split(',')
        .map(str::trim)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() == 6 {
        Some(format!(
            "matrix({} {} {} {} {} {})",
            values[0], values[1], values[2], values[3], values[4], values[5]
        ))
    } else {
        None
    }
}

fn sources(xml: &str, tag: &str) -> Vec<String> {
    let needle = format!("<{tag}");
    xml.split(&needle)
        .skip(1)
        .filter_map(|fragment| fragment.split("Source=\"").nth(1)?.split('"').next())
        .map(str::to_owned)
        .collect()
}

fn resource_key(value: &str) -> Option<&str> {
    value
        .strip_prefix("{StaticResource ")
        .or_else(|| value.strip_prefix("{DynamicResource "))?
        .strip_suffix('}')
}

#[derive(Clone)]
enum Paint {
    Color(String),
    ImageBrush {
        data: String,
        transform: String,
        width: String,
        height: String,
    },
}

fn deobfuscate_font(mut bytes: Vec<u8>, file: &str) -> Option<Vec<u8>> {
    let guid = file.strip_suffix(".odttf")?.replace('-', "");
    if guid.len() != 32 || bytes.len() < 32 {
        return None;
    }
    let mut key = [0_u8; 16];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&guid[index * 2..index * 2 + 2], 16).ok()?;
    }
    key.reverse();
    for (index, byte) in bytes.iter_mut().take(32).enumerate() {
        *byte ^= key[index % key.len()];
    }
    Some(bytes)
}

fn attribute(xml: &str, name: &str) -> Option<String> {
    xml.split(&format!("{name}=\""))
        .nth(1)?
        .split('"')
        .next()
        .map(str::to_owned)
}

fn unresolved_resource(
    paints: &std::collections::HashMap<String, Paint>,
    raw: Option<&str>,
) -> bool {
    raw.and_then(resource_key)
        .is_some_and(|key| !paints.contains_key(key))
}

fn paint(
    defs: &mut String,
    paints: &std::collections::HashMap<String, Paint>,
    raw: Option<String>,
    prefix: &str,
    next_id: &mut usize,
) -> String {
    let Some(raw) = raw else {
        return "none".to_owned();
    };
    let Some(key) = resource_key(&raw) else {
        return if raw.starts_with('{') {
            "none".to_owned()
        } else {
            raw
        };
    };
    match paints.get(key) {
        Some(Paint::Color(value)) => value.clone(),
        Some(Paint::ImageBrush {
            data,
            transform,
            width,
            height,
        }) => {
            *next_id += 1;
            let id = format!("rs-cad-{prefix}-{next_id}");
            writeln!(defs, "<pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" width=\"{}\" height=\"{}\" patternTransform=\"{}\"><image href=\"{}\" width=\"{}\" height=\"{}\"/></pattern>", escape(width), escape(height), escape(transform), data, escape(width), escape(height)).unwrap();
            format!("url(#{id})")
        }
        None => "none".to_owned(),
    }
}

fn normalize(base: &str, source: &str) -> Option<String> {
    let mut parts: Vec<&str> = if source.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(dir, _)| dir.split('/').collect())
            .unwrap_or_default()
    };
    for part in source.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn page_paths<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<Vec<String>, Error> {
    let sequence = (0..archive.len()).find_map(|index| {
        let entry = archive.by_index(index).ok()?;
        entry
            .name()
            .ends_with("FixedDocumentSequence.fdseq")
            .then(|| entry.name().to_owned())
    });
    let Some(sequence) = sequence else {
        let pages: Vec<String> = (0..archive.len())
            .filter_map(|index| {
                let entry = archive.by_index(index).ok()?;
                entry
                    .name()
                    .ends_with("/FixedPage.fpage")
                    .then(|| entry.name().to_owned())
            })
            .collect();
        return (pages.len() == 1)
            .then_some(pages)
            .ok_or(Error::MissingFixedPage);
    };
    let mut xml = String::new();
    archive
        .by_name(&sequence)?
        .read_to_string(&mut xml)
        .map_err(|_| Error::MissingFixedPage)?;
    let mut pages = Vec::new();
    for document in sources(&xml, "DocumentReference") {
        let document = normalize(&sequence, &document).ok_or(Error::MissingFixedPage)?;
        let mut document_xml = String::new();
        archive
            .by_name(&document)?
            .read_to_string(&mut document_xml)
            .map_err(|_| Error::MissingFixedPage)?;
        for page in sources(&document_xml, "PageContent") {
            let page = normalize(&document, &page).ok_or(Error::MissingFixedPage)?;
            if !page.ends_with(".fpage") {
                return Err(Error::MissingFixedPage);
            }
            pages.push(page);
        }
    }
    (!pages.is_empty())
        .then_some(pages)
        .ok_or(Error::MissingFixedPage)
}

/// Reads the observed XPS FixedPage subset from a bounded DWFX OPC package.
/// W2X remains optional metadata: paint primitives in FixedPage drive rendering.
pub fn render_dwfx_page<R: Read + Seek>(
    input: R,
    page_index: usize,
) -> Result<(Inspection, String), Error> {
    let mut archive = ZipArchive::new(input)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        total = total.saturating_add(entry.size());
        if entry.size() > MAX_ENTRY_BYTES
            || total > MAX_TOTAL_BYTES
            || entry.name().split('/').any(|segment| segment == "..")
        {
            return Err(Error::TooLarge);
        }
    }
    let pages = page_paths(&mut archive)?;
    let page_path = pages
        .get(page_index)
        .cloned()
        .ok_or(Error::MissingFixedPage)?;
    let mut page = String::new();
    archive
        .by_name(&page_path)?
        .read_to_string(&mut page)
        .map_err(|_| Error::MissingFixedPage)?;
    let mut paints: std::collections::HashMap<String, Paint> = std::collections::HashMap::new();
    let mut fonts: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut font_faces: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let page_dir = page_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or_default();
    let extension_parts: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            let entry = archive.by_index(index).ok()?;
            (entry.name().starts_with(page_dir)
                && entry.name().ends_with(".xml")
                && !entry.name().ends_with("FixedPage.fpage"))
            .then(|| entry.name().to_owned())
        })
        .collect();
    for part in extension_parts {
        let mut xml = String::new();
        if archive
            .by_name(&part)
            .and_then(|mut entry| {
                entry
                    .read_to_string(&mut xml)
                    .map_err(zip::result::ZipError::Io)
            })
            .is_err()
        {
            continue;
        }
        for font in xml.split("<Font ").skip(1) {
            if let (Some(name), Some(uri)) = (attribute(font, "Name"), attribute(font, "FontUri"))
                && let Some(file) = uri.rsplit('/').next()
            {
                let path = normalize(&part, file).unwrap_or_default();
                if let Ok(mut entry) = archive.by_name(&path)
                    && entry.size() <= MAX_ENTRY_BYTES
                {
                    let mut bytes = Vec::new();
                    if entry.read_to_end(&mut bytes).is_ok()
                        && let Some(bytes) = deobfuscate_font(bytes, file)
                    {
                        let data = format!(
                            "data:font/ttf;base64,{}",
                            base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                bytes
                            )
                        );
                        font_faces.insert(
                            file.to_owned(),
                            format!(
                                "@font-face{{font-family:'{}';src:url('{}')}}",
                                escape(&name),
                                data
                            ),
                        );
                    }
                }
                fonts.insert(file.to_owned(), name);
            }
        }
    }
    for source in sources(&page, "ResourceDictionary") {
        let Some(resource_path) = normalize(&page_path, &source) else {
            continue;
        };
        let mut dictionary = String::new();
        if archive
            .by_name(&resource_path)
            .and_then(|mut entry| {
                entry
                    .read_to_string(&mut dictionary)
                    .map_err(zip::result::ZipError::Io)
            })
            .is_err()
        {
            continue;
        }
        for color in dictionary.split("<SolidColorBrush").skip(1) {
            if let (Some(key), Some(value)) = (attribute(color, "x:Key"), attribute(color, "Color"))
            {
                paints.insert(key, Paint::Color(value));
            }
        }
        for brush in dictionary.split("<ImageBrush").skip(1) {
            let Some(key) = attribute(brush, "x:Key") else {
                continue;
            };
            let Some(image_source) =
                attribute(brush, "ImageSource").and_then(|path| normalize(&resource_path, &path))
            else {
                continue;
            };
            let Ok(mut image) = archive.by_name(&image_source) else {
                continue;
            };
            if image.size() > MAX_ENTRY_BYTES {
                continue;
            }
            let mut bytes = Vec::new();
            if image.read_to_end(&mut bytes).is_err() {
                continue;
            }
            let data = format!(
                "data:image/jpeg;base64,{}",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
            );
            let transform = attribute(brush, "Transform")
                .and_then(|value| xps_transform(Some(value)))
                .unwrap_or_default();
            let viewport = attribute(brush, "Viewport").unwrap_or_else(|| "0,0,140,125".to_owned());
            let dimensions: Vec<&str> = viewport.split(',').map(str::trim).collect();
            let width = dimensions.get(2).unwrap_or(&"140").to_string();
            let height = dimensions.get(3).unwrap_or(&"125").to_string();
            paints.insert(
                key,
                Paint::ImageBrush {
                    data,
                    transform,
                    width,
                    height,
                },
            );
        }
    }

    let mut reader = Reader::from_str(&page);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut width = 0.0;
    let mut height = 0.0;
    let mut paths = 0;
    let mut glyphs = 0;
    let mut canvases = 0;
    let mut elements = 0;
    let mut depth = 0_usize;
    let mut text = Vec::new();
    let mut unresolved_resources = 0_usize;
    let mut unresolved_clips = 0_usize;
    let mut clip_id = 0_usize;
    let mut defs = String::new();
    let mut svg = String::new();
    let mut initialized = false;

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(event) | Event::Empty(event) => {
                let tag = event.local_name();
                match tag.as_ref() {
                    b"FixedPage" => {
                        width = number(attr(&event, b"Width")?, "FixedPage.Width")?;
                        height = number(attr(&event, b"Height")?, "FixedPage.Height")?;
                        writeln!(svg, "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" width=\"{width}\" height=\"{height}\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/>").unwrap();
                        initialized = true;
                    }
                    b"Canvas" if initialized => {
                        elements += 1;
                        canvases += 1;
                        let transform =
                            xps_transform(attr(&event, b"RenderTransform")?).unwrap_or_default();
                        let clip = attr(&event, b"Clip")?
                            .and_then(|value| {
                                if resource_key(&value).is_some() {
                                    unresolved_clips += 1;
                                    return None;
                                }
                                clip_id += 1;
                                let id = format!("rs-cad-clip-{clip_id}");
                                writeln!(
                                    defs,
                                    "<clipPath id=\"{id}\"><path d=\"{}\"/></clipPath>",
                                    escape(&value)
                                )
                                .unwrap();
                                Some(format!(" clip-path=\"url(#{id})\""))
                            })
                            .unwrap_or_default();
                        if transform.is_empty() {
                            writeln!(svg, "<g{clip}>").unwrap();
                        } else {
                            writeln!(svg, "<g transform=\"{transform}\"{clip}>").unwrap();
                        }
                        if event.is_empty() {
                            svg.push_str("</g>\n");
                        } else {
                            depth += 1;
                        }
                    }
                    b"Path" if initialized => {
                        elements += 1;
                        paths += 1;
                        let data = attr(&event, b"Data")?.unwrap_or_default();
                        let raw_fill = attr(&event, b"Fill")?;
                        let raw_stroke = attr(&event, b"Stroke")?;
                        unresolved_resources +=
                            usize::from(unresolved_resource(&paints, raw_fill.as_deref()));
                        unresolved_resources +=
                            usize::from(unresolved_resource(&paints, raw_stroke.as_deref()));
                        let fill = paint(&mut defs, &paints, raw_fill, "fill", &mut clip_id);
                        let stroke = paint(&mut defs, &paints, raw_stroke, "stroke", &mut clip_id);
                        let transform =
                            xps_transform(attr(&event, b"RenderTransform")?).unwrap_or_default();
                        let stroke_width =
                            attr(&event, b"StrokeThickness")?.unwrap_or_else(|| "1".to_owned());
                        let opacity = attr(&event, b"Opacity")?.unwrap_or_else(|| "1".to_owned());
                        if transform.is_empty() {
                            writeln!(svg, "<path d=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{}\" opacity=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>", escape(&data), escape(&fill), escape(&stroke), escape(&stroke_width), escape(&opacity)).unwrap();
                        } else {
                            writeln!(svg, "<path d=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{}\" opacity=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\" transform=\"{}\"/>", escape(&data), escape(&fill), escape(&stroke), escape(&stroke_width), escape(&opacity), transform).unwrap();
                        }
                    }
                    b"Glyphs" if initialized => {
                        elements += 1;
                        glyphs += 1;
                        let value = attr(&event, b"UnicodeString")?.unwrap_or_default();
                        let origin_x = attr(&event, b"OriginX")?.unwrap_or_else(|| "0".to_owned());
                        let origin_y = attr(&event, b"OriginY")?.unwrap_or_else(|| "0".to_owned());
                        let size = attr(&event, b"FontRenderingEmSize")?
                            .unwrap_or_else(|| "12".to_owned());
                        let raw_fill = attr(&event, b"Fill")?;
                        unresolved_resources +=
                            usize::from(unresolved_resource(&paints, raw_fill.as_deref()));
                        let fill = paint(&mut defs, &paints, raw_fill, "glyph-fill", &mut clip_id);
                        let transform =
                            xps_transform(attr(&event, b"RenderTransform")?).unwrap_or_default();
                        let family = attr(&event, b"FontUri")?
                            .and_then(|uri| {
                                uri.rsplit('/')
                                    .next()
                                    .and_then(|file| fonts.get(file))
                                    .cloned()
                            })
                            .unwrap_or_else(|| "sans-serif".to_owned());
                        let opacity = attr(&event, b"Opacity")?.unwrap_or_else(|| "1".to_owned());
                        if transform.is_empty() {
                            writeln!(svg, "<text x=\"{}\" y=\"{}\" font-size=\"{}\" fill=\"{}\" opacity=\"{}\" font-family=\"{}\">{}</text>", escape(&origin_x), escape(&origin_y), escape(&size), escape(&fill), escape(&opacity), escape(&family), escape(&value)).unwrap();
                        } else {
                            writeln!(svg, "<text x=\"{}\" y=\"{}\" font-size=\"{}\" fill=\"{}\" opacity=\"{}\" font-family=\"{}\" transform=\"{}\">{}</text>", escape(&origin_x), escape(&origin_y), escape(&size), escape(&fill), escape(&opacity), escape(&family), transform, escape(&value)).unwrap();
                        }
                        if !value.is_empty() {
                            text.push(value);
                        }
                    }
                    _ => {}
                }
                if elements > MAX_ELEMENTS {
                    return Err(Error::TooManyElements);
                }
            }
            Event::End(event) if event.local_name().as_ref() == b"Canvas" && depth > 0 => {
                svg.push_str("</g>\n");
                depth -= 1;
            }
            Event::Eof => break,
            _ => {}
        }
        if svg.len() > MAX_SVG_BYTES {
            return Err(Error::SvgTooLarge);
        }
        buffer.clear();
    }
    if !font_faces.is_empty() {
        writeln!(
            defs,
            "<style>{}</style>",
            font_faces.values().cloned().collect::<String>()
        )
        .unwrap();
    }
    if !defs.is_empty() {
        svg = svg.replacen("><rect", &format!("><defs>{defs}</defs><rect"), 1);
    }
    svg.push_str("</svg>\n");
    Ok((
        Inspection {
            page_path,
            width,
            height,
            paths,
            glyphs,
            canvases,
            text,
            warnings: {
                let mut warnings =
                    vec!["W2X layer metadata is not rendered in this release".to_owned()];
                if unresolved_resources > 0 {
                    warnings.push(format!(
                        "{unresolved_resources} XPS resource paint references are not rendered"
                    ));
                }
                if unresolved_clips > 0 {
                    warnings.push(format!(
                        "{unresolved_clips} XPS resource clip references are not rendered"
                    ));
                }
                warnings
            },
        },
        svg,
    ))
}

pub fn render_dwfx<R: Read + Seek>(input: R) -> Result<(Inspection, String), Error> {
    render_dwfx_page(input, 0)
}

pub fn render_dwfx_all<R: Read + Seek>(mut input: R) -> Result<Vec<(Inspection, String)>, Error> {
    let mut archive = ZipArchive::new(&mut input)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    let pages = page_paths(&mut archive)?;
    drop(archive);
    input
        .seek(SeekFrom::Start(0))
        .map_err(|_| Error::MissingFixedPage)?;
    let mut source = Vec::new();
    input
        .read_to_end(&mut source)
        .map_err(|_| Error::MissingFixedPage)?;
    pages
        .iter()
        .enumerate()
        .map(|(index, _)| render_dwfx_page(Cursor::new(&source), index))
        .collect()
}

pub fn inspect_bytes(bytes: &[u8]) -> Result<Inspection, Error> {
    render_dwfx(Cursor::new(bytes)).map(|(inspection, _)| inspection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn decodes_numeric_character_references() {
        assert_eq!(
            decode_entities("Execu&#x00C7;&#x00C3;O &amp; N&#x00BA;"),
            "ExecuÇÃO & Nº"
        );
    }

    #[test]
    fn renders_a_minimal_dwfx_page() {
        let source = Cursor::new(Vec::<u8>::new());
        let mut writer = zip::ZipWriter::new(source);
        writer
            .start_file(
                "doc/FixedPage.fpage",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(br##"<FixedPage Width="100" Height="50"><Canvas RenderTransform="2,0,0,2,2,3"><Path Data="M0,0 L10,10 Z" Fill="#000000" Stroke="#000000" StrokeThickness="2"/><Glyphs UnicodeString="QEE" OriginX="5" OriginY="10" FontRenderingEmSize="8"/></Canvas></FixedPage>"##).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let (inspection, svg) = render_dwfx(Cursor::new(bytes)).unwrap();
        assert_eq!(inspection.paths, 1);
        assert_eq!(inspection.glyphs, 1);
        assert!(svg.contains("QEE"));
        assert!(svg.contains("M0,0 L10,10 Z"));
        assert!(svg.contains("stroke-width=\"2\""));
        assert!(!svg.contains("vector-effect"));
    }

    #[test]
    fn rejects_unresolved_resource_paint() {
        let mut defs = String::new();
        let paints = std::collections::HashMap::new();
        assert_eq!(
            paint(
                &mut defs,
                &paints,
                Some("{StaticResource Missing}".to_owned()),
                "fill",
                &mut 0,
            ),
            "none"
        );
    }
}
