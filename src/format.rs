use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    Dwfx,
    Dxf,
    Dwg,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("input format is not supported")]
    Unknown,
    #[error("binary DXF is detected but not implemented")]
    BinaryDxf,
}

pub fn detect(bytes: &[u8]) -> Result<InputFormat, Error> {
    if matches!(
        bytes.get(..6),
        Some(b"AC1015" | b"AC1018" | b"AC1021" | b"AC1024" | b"AC1027" | b"AC1032")
    ) {
        return Ok(InputFormat::Dwg);
    }
    if bytes.starts_with(b"AutoCAD Binary DXF\r\n\x1a\0") {
        return Err(Error::BinaryDxf);
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Ok(InputFormat::Dwfx);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Unknown)?
        .trim_start_matches('\u{feff}')
        .trim_start();
    if text.starts_with("0\nSECTION")
        || text.starts_with("0\r\nSECTION")
        || text.starts_with("0\nENTITIES")
    {
        return Ok(InputFormat::Dxf);
    }
    Err(Error::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_formats_from_content() {
        assert_eq!(detect(b"AC1032").unwrap(), InputFormat::Dwg);
        assert_eq!(detect(b"PK\x03\x04test").unwrap(), InputFormat::Dwfx);
        assert_eq!(detect(b"0\nSECTION\n").unwrap(), InputFormat::Dxf);
    }
}
