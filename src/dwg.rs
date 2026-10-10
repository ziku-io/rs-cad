use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    R2000,
    R2004,
    R2007,
    R2010,
    R2013,
    R2018,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub code: String,
    pub version: Version,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("DWG header is shorter than six bytes")]
    ShortHeader,
    #[error("unsupported DWG version marker: {0}")]
    UnsupportedVersion(String),
}

pub fn inspect(bytes: &[u8]) -> Result<Inspection, Error> {
    let marker = bytes.get(..6).ok_or(Error::ShortHeader)?;
    let code = std::str::from_utf8(marker)
        .map_err(|_| Error::UnsupportedVersion("non-ASCII".to_owned()))?;
    let version = match code {
        "AC1015" => Version::R2000,
        "AC1018" => Version::R2004,
        "AC1021" => Version::R2007,
        "AC1024" => Version::R2010,
        "AC1027" => Version::R2013,
        "AC1032" => Version::R2018,
        _ => return Err(Error::UnsupportedVersion(code.to_owned())),
    };
    Ok(Inspection {
        code: code.to_owned(),
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_versions() {
        for (code, version) in [
            (b"AC1015", Version::R2000),
            (b"AC1018", Version::R2004),
            (b"AC1021", Version::R2007),
            (b"AC1024", Version::R2010),
            (b"AC1027", Version::R2013),
            (b"AC1032", Version::R2018),
        ] {
            assert_eq!(inspect(code).unwrap().version, version);
        }
    }

    #[test]
    fn rejects_unknown_version() {
        assert!(matches!(
            inspect(b"AC1033"),
            Err(Error::UnsupportedVersion(_))
        ));
    }
}
