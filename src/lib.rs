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
    let mut paints = std::collections::HashMap::new();
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
            .is_ok()
        {
            for color in dictionary.split("<SolidColorBrush").skip(1) {
                let key = color
                    .split("x:Key=\"")
                    .nth(1)
                    .and_then(|tail| tail.split('"').next());
                let value = color
                    .split("Color=\"")
                    .nth(1)
                    .and_then(|tail| tail.split('"').next());
                if let (Some(key), Some(value)) = (key, value) {
                    paints.insert(key.to_owned(), value.to_owned());
                }
            }
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
                            .map(|value| format!(" data-rs-cad-clip=\"{}\"", escape(&value)))
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
                            usize::from(raw_fill.as_deref().is_some_and(|paint| {
                                paint.starts_with("{StaticResource")
                                    || paint.starts_with("{DynamicResource")
                            }));
                        unresolved_resources +=
                            usize::from(raw_stroke.as_deref().is_some_and(|paint| {
                                paint.starts_with("{StaticResource")
                                    || paint.starts_with("{DynamicResource")
                            }));
                        let fill = raw_fill
                            .as_deref()
                            .and_then(|paint| {
                                resource_key(paint)
                                    .and_then(|key| paints.get(key))
                                    .cloned()
                                    .or_else(|| (!paint.starts_with('{')).then(|| paint.to_owned()))
                            })
                            .unwrap_or_else(|| "none".to_owned());
                        let stroke = raw_stroke
                            .as_deref()
                            .and_then(|paint| {
                                resource_key(paint)
                                    .and_then(|key| paints.get(key))
                                    .cloned()
                                    .or_else(|| (!paint.starts_with('{')).then(|| paint.to_owned()))
                            })
                            .unwrap_or_else(|| "none".to_owned());
                        let transform =
                            xps_transform(attr(&event, b"RenderTransform")?).unwrap_or_default();
                        if transform.is_empty() {
                            writeln!(svg, "<path d=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>", escape(&data), escape(&fill), escape(&stroke)).unwrap();
                        } else {
                            writeln!(svg, "<path d=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\" transform=\"{}\"/>", escape(&data), escape(&fill), escape(&stroke), transform).unwrap();
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
                        let fill = attr(&event, b"Fill")?.unwrap_or_else(|| "#000000".to_owned());
                        let transform =
                            xps_transform(attr(&event, b"RenderTransform")?).unwrap_or_default();
                        if transform.is_empty() {
                            writeln!(svg, "<text x=\"{}\" y=\"{}\" font-size=\"{}\" fill=\"{}\" font-family=\"sans-serif\">{}</text>", escape(&origin_x), escape(&origin_y), escape(&size), escape(&fill), escape(&value)).unwrap();
                        } else {
                            writeln!(svg, "<text x=\"{}\" y=\"{}\" font-size=\"{}\" fill=\"{}\" font-family=\"sans-serif\" transform=\"{}\">{}</text>", escape(&origin_x), escape(&origin_y), escape(&size), escape(&fill), transform, escape(&value)).unwrap();
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
        writer.write_all(br##"<FixedPage Width="100" Height="50"><Canvas RenderTransform="1,0,0,1,2,3"><Path Data="M0,0 L10,10 Z" Fill="#000000"/><Glyphs UnicodeString="QEE" OriginX="5" OriginY="10" FontRenderingEmSize="8"/></Canvas></FixedPage>"##).unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let (inspection, svg) = render_dwfx(Cursor::new(bytes)).unwrap();
        assert_eq!(inspection.paths, 1);
        assert_eq!(inspection.glyphs, 1);
        assert!(svg.contains("QEE"));
        assert!(svg.contains("M0,0 L10,10 Z"));
    }
}
