use std::fmt;
use std::str::FromStr;

/// The Yuki country deployment an access key belongs to.
///
/// Yuki runs a separate API host per country. The services and their WSDLs are
/// the same; only the host differs, and an access key is valid on its own
/// country's host only. [`Region::ALL`] lists every known deployment, and
/// [`Region::info`] is the one table of what each is; adding a country means a
/// variant, its entry in `ALL`, and its row in `info`.
///
/// With the `serde` feature it (de)serializes as its `as_str` form, e.g. `"be"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "String", into = "&'static str")
)]
pub enum Region {
    /// The Netherlands: `api.yukiworks.nl`. Also the legacy fallback when no
    /// region is given, which is all `Default` means here: callers and
    /// configurations from before regions existed talked to this host.
    #[default]
    Nl,
    /// Belgium: `api.yukiworks.be`.
    Be,
}

/// What a [`Region`] is: its code, its country, and its API host.
struct RegionInfo {
    code: &'static str,
    country: &'static str,
    api_root: &'static str,
}

impl Region {
    /// Every known deployment.
    pub const ALL: [Region; 2] = [Region::Nl, Region::Be];

    const fn info(self) -> RegionInfo {
        match self {
            Self::Nl => RegionInfo {
                code: "nl",
                country: "Netherlands",
                api_root: "https://api.yukiworks.nl/ws",
            },
            Self::Be => RegionInfo {
                code: "be",
                country: "Belgium",
                api_root: "https://api.yukiworks.be/ws",
            },
        }
    }

    /// Root under which every service lives, e.g. `{root}/Accounting.asmx`.
    pub fn api_root(self) -> &'static str {
        self.info().api_root
    }

    /// The API host, e.g. `api.yukiworks.be`.
    pub fn host(self) -> &'static str {
        let root = self.api_root();
        let host = root.strip_prefix("https://").unwrap_or(root);
        host.split('/').next().unwrap_or(host)
    }

    /// The country in English, e.g. `Belgium`.
    pub fn country(self) -> &'static str {
        self.info().country
    }

    /// The deployment whose root `url` is, ignoring a trailing slash and case.
    /// `None` for anything else, such as a proxy or a local mock.
    pub fn from_api_root(url: &str) -> Option<Self> {
        let url = url.trim().trim_end_matches('/');
        Self::ALL
            .into_iter()
            .find(|r| r.api_root().eq_ignore_ascii_case(url))
    }

    /// The region whose code is exactly `code`, e.g. `be`.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == code)
    }

    pub fn as_str(self) -> &'static str {
        self.info().code
    }

    /// Every region code, e.g. `["nl", "be"]`.
    pub fn codes() -> [&'static str; Self::ALL.len()] {
        Self::ALL.map(Self::as_str)
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
        let code = s.trim().to_ascii_lowercase();
        Self::from_code(&code).ok_or_else(|| {
            let codes = Self::codes();
            let (last, rest) = codes.split_last().expect("at least one region");
            let expected = if rest.is_empty() {
                (*last).to_string()
            } else {
                format!("{} or {last}", rest.join(", "))
            };
            format!("unknown region '{code}' (expected {expected})")
        })
    }
}

impl TryFrom<String> for Region {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Region> for &'static str {
    fn from(region: Region) -> Self {
        region.as_str()
    }
}

/// Full endpoint of `service` (e.g. `Accounting.asmx`) under `api_root`.
pub(crate) fn service_url(api_root: &str, service: &str) -> String {
    format!("{}/{service}", api_root.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_api_root_recognises_only_known_roots() {
        assert_eq!(
            Region::from_api_root("https://api.yukiworks.be/ws/"),
            Some(Region::Be)
        );
        assert_eq!(
            Region::from_api_root("https://api.yukiworks.nl/ws"),
            Some(Region::Nl)
        );
        assert_eq!(Region::from_api_root("http://127.0.0.1:1/ws"), None);
    }
}
