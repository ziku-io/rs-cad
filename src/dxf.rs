use std::fmt::Write;
use thiserror::Error;

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAIRS: usize = 200_000;
const MAX_ENTITIES: usize = 50_000;
const MAX_SVG_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entity {
    Line { start: Point, end: Point },
    Circle { center: Point, radius: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub entities: Vec<Entity>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    pub lines: usize,
    pub circles: usize,
    pub bounds: Option<Bounds>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("DXF input exceeds the configured limit")]
    InputTooLarge,
    #[error("DXF has an odd number of group-code/value lines")]
    OddPair,
    #[error("invalid DXF group code: {0}")]
    GroupCode(String),
    #[error("invalid DXF number: {0}")]
    Number(String),
    #[error("DXF has no ENTITIES section")]
    MissingEntities,
    #[error("DXF entity is missing required coordinate {0}")]
    MissingCoordinate(&'static str),
    #[error("DXF has too many entities")]
    TooManyEntities,
    #[error("DXF SVG exceeds the configured limit")]
    SvgTooLarge,
}

fn finite(value: &str) -> Result<f64, Error> {
    let number: f64 = value.parse().map_err(|_| Error::Number(value.to_owned()))?;
    if number.is_finite() {
        Ok(number)
    } else {
        Err(Error::Number(value.to_owned()))
    }
}

fn bounds_for(entity: &Entity) -> Bounds {
    match entity {
        Entity::Line { start, end } => Bounds {
            min_x: start.x.min(end.x),
            min_y: start.y.min(end.y),
            max_x: start.x.max(end.x),
            max_y: start.y.max(end.y),
        },
        Entity::Circle { center, radius } => Bounds {
            min_x: center.x - radius,
            min_y: center.y - radius,
            max_x: center.x + radius,
            max_y: center.y + radius,
        },
    }
}

fn merge(left: Option<Bounds>, right: Bounds) -> Bounds {
    match left {
        Some(left) => Bounds {
            min_x: left.min_x.min(right.min_x),
            min_y: left.min_y.min(right.min_y),
            max_x: left.max_x.max(right.max_x),
            max_y: left.max_y.max(right.max_y),
        },
        None => right,
    }
}

fn finish(kind: &str, values: &[(i32, String)], document: &mut Document) -> Result<(), Error> {
    if kind != "LINE" && kind != "CIRCLE" {
        document
            .warnings
            .push(format!("unsupported DXF entity: {kind}"));
        return Ok(());
    }
    let find = |code| {
        values
            .iter()
            .find(|(candidate, _)| *candidate == code)
            .map(|(_, value)| value.as_str())
    };
    let center = Point {
        x: finite(find(10).ok_or(Error::MissingCoordinate("10"))?)?,
        y: finite(find(20).ok_or(Error::MissingCoordinate("20"))?)?,
    };
    let entity = if kind == "LINE" {
        Entity::Line {
            start: center,
            end: Point {
                x: finite(find(11).ok_or(Error::MissingCoordinate("11"))?)?,
                y: finite(find(21).ok_or(Error::MissingCoordinate("21"))?)?,
            },
        }
    } else {
        let radius = finite(find(40).ok_or(Error::MissingCoordinate("40"))?)?;
        if radius <= 0.0 {
            return Err(Error::Number("circle radius must be positive".to_owned()));
        }
        Entity::Circle { center, radius }
    };
    document.entities.push(entity);
    if document.entities.len() > MAX_ENTITIES {
        return Err(Error::TooManyEntities);
    }
    Ok(())
}

pub fn parse(bytes: &[u8]) -> Result<Document, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::InputTooLarge);
    }
    let source = std::str::from_utf8(bytes)
        .map_err(|_| Error::Number("DXF must be UTF-8 ASCII text in this release".to_owned()))?;
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    if !lines.len().is_multiple_of(2) {
        return Err(Error::OddPair);
    }
    if lines.len() / 2 > MAX_PAIRS {
        return Err(Error::InputTooLarge);
    }
    let pairs: Vec<(i32, &str)> = lines
        .chunks_exact(2)
        .map(|pair| {
            Ok((
                pair[0]
                    .parse()
                    .map_err(|_| Error::GroupCode(pair[0].to_owned()))?,
                pair[1],
            ))
        })
        .collect::<Result<_, Error>>()?;
    let mut in_entities = false;
    let mut found_entities = false;
    let mut current: Option<(String, Vec<(i32, String)>)> = None;
    let mut document = Document {
        entities: Vec::new(),
        warnings: Vec::new(),
    };
    for (code, value) in pairs {
        if code == 0 {
            if let Some((kind, values)) = current.take() {
                finish(&kind, &values, &mut document)?;
            }
            match value {
                "SECTION" => {}
                "ENDSEC" => in_entities = false,
                "EOF" => break,
                kind if in_entities => current = Some((kind.to_owned(), Vec::new())),
                _ => {}
            }
            continue;
        }
        if code == 2 && value == "ENTITIES" && !in_entities {
            in_entities = true;
            found_entities = true;
            continue;
        }
        if in_entities && let Some((_, values)) = &mut current {
            values.push((code, value.to_owned()));
        }
    }
    if let Some((kind, values)) = current.take() {
        finish(&kind, &values, &mut document)?;
    }
    if !found_entities {
        return Err(Error::MissingEntities);
    }
    Ok(document)
}

pub fn inspect(bytes: &[u8]) -> Result<Inspection, Error> {
    let document = parse(bytes)?;
    let mut bounds = None;
    let mut lines = 0;
    let mut circles = 0;
    for entity in &document.entities {
        bounds = Some(merge(bounds, bounds_for(entity)));
        match entity {
            Entity::Line { .. } => lines += 1,
            Entity::Circle { .. } => circles += 1,
        }
    }
    Ok(Inspection {
        lines,
        circles,
        bounds,
        warnings: document.warnings,
    })
}

pub fn render(bytes: &[u8]) -> Result<String, Error> {
    let document = parse(bytes)?;
    let inspection = inspect(bytes)?;
    let bounds = inspection.bounds.unwrap_or(Bounds {
        min_x: 0.0,
        min_y: 0.0,
        max_x: 1.0,
        max_y: 1.0,
    });
    let width = (bounds.max_x - bounds.min_x).max(1.0);
    let height = (bounds.max_y - bounds.min_y).max(1.0);
    let padding = width.max(height) * 0.01;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\">\n",
        bounds.min_x - padding,
        bounds.min_y - padding,
        width + 2.0 * padding,
        height + 2.0 * padding
    );
    for entity in document.entities {
        match entity {
            Entity::Line { start, end } => writeln!(
                svg,
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"black\" fill=\"none\"/>",
                start.x, start.y, end.x, end.y
            )
            .unwrap(),
            Entity::Circle { center, radius } => writeln!(
                svg,
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" stroke=\"black\" fill=\"none\"/>",
                center.x, center.y, radius
            )
            .unwrap(),
        }
        if svg.len() > MAX_SVG_BYTES {
            return Err(Error::SvgTooLarge);
        }
    }
    svg.push_str("</svg>\n");
    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;
    const DXF: &[u8] = b"0\nSECTION\n2\nENTITIES\n0\nLINE\n10\n0\n20\n0\n11\n10\n21\n5\n0\nCIRCLE\n10\n5\n20\n5\n40\n2\n0\nENDSEC\n0\nEOF\n";

    #[test]
    fn parses_and_renders_entities() {
        let info = inspect(DXF).unwrap();
        assert_eq!((info.lines, info.circles), (1, 1));
        let svg = render(DXF).unwrap();
        assert!(svg.contains("<line"));
        assert!(svg.contains("<circle"));
    }

    #[test]
    fn rejects_odd_pairs() {
        assert!(matches!(parse(b"0\nSECTION\n2\n"), Err(Error::OddPair)));
    }
}
