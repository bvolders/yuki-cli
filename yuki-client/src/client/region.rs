use std::fmt;
use std::str::FromStr;

/// The Yuki country deployment an access key belongs to.
///
/// Yuki runs a separate API host per country. The services and their WSDLs are
/// the same; only the host differs, and an access key is valid on its own
/// country's host only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Region {
    /// The Netherlands: `api.yukiworks.nl`.
    #[default]
    Nl,
    /// Belgium: `api.yukiworks.be`.
    Be,
}

impl Region {
    /// Root under which every service lives, e.g. `{root}/Accounting.asmx`.
    pub fn api_root(self) -> &'static str {
        match self {
            Self::Nl => "https://api.yukiworks.nl/ws",
            Self::Be => "https://api.yukiworks.be/ws",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nl => "nl",
            Self::Be => "be",
        }
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Region {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nl" => Ok(Self::Nl),
            "be" => Ok(Self::Be),
            other => Err(format!("unknown region '{other}' (expected nl or be)")),
        }
    }
}

/// Full endpoint of `service` (e.g. `Accounting.asmx`) under `api_root`.
pub(crate) fn service_url(api_root: &str, service: &str) -> String {
    format!("{}/{service}", api_root.trim_end_matches('/'))
}
