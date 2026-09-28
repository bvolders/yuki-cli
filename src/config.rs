use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::Region;
use crate::error::YukiError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminEntry {
    pub domain_id: String,
    pub admin_id: String,

    /// Display name as Yuki reports it, e.g. "Example Holding B.V.".
    ///
    /// Written by `yuki init`; absent in configurations created before it was
    /// recorded, which is why nothing may rely on it being present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Access key scoped to this administration.
    ///
    /// Yuki issues an access key inside a single administration and scopes the
    /// session it opens to that administration, so a second administration is
    /// reachable only through its own key. When absent, the shared top-level
    /// `api_key` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// Yuki deployment this administration lives on, when it differs from the
    /// top-level `region`. Set by `yuki init --add --region ...`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "region_serde"
    )]
    pub region: Option<Region>,
}

impl AdminEntry {
    pub fn new(domain_id: impl Into<String>, admin_id: impl Into<String>) -> Self {
        Self {
            domain_id: domain_id.into(),
            admin_id: admin_id.into(),
            name: None,
            api_key: None,
            region: None,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

/// The administration a command runs against, together with the key that reaches it.
///
/// The key travels with the identifiers on purpose. A Yuki session is scoped by the
/// access key that opened it, so authenticating with one administration's key and
/// then passing another administration's `admin_id` queries the wrong books without
/// reporting an error.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// Configuration key for this administration, i.e. what `--admin` accepts.
    pub config_name: &'a str,
    pub domain_id: &'a str,
    pub admin_id: &'a str,
    pub api_key: &'a str,
    /// API root the key is valid on, e.g. `https://api.yukiworks.be/ws`.
    pub api_root: &'a str,
}

/// A distinct access key, with the administrations configured to use it.
#[derive(Debug, Clone)]
pub struct AccessKey<'a> {
    pub api_key: &'a str,
    pub api_root: &'a str,
    pub admins: Vec<&'a str>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Key used by any administration that does not carry one of its own.
    pub api_key: String,
    pub default_admin: String,
    pub administrations: BTreeMap<String, AdminEntry>,
    /// Counterparty name patterns to ignore in `check unmatched`.
    /// Matched case-insensitively as substrings against the counterparty name.
    #[serde(default)]
    pub unmatched_ignore: Vec<String>,
    /// Yuki deployment for administrations that do not name their own.
    /// Absent means the Netherlands, so existing configurations are unaffected.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "region_serde"
    )]
    pub region: Option<Region>,
    /// Full API root (e.g. `https://api.yukiworks.be/ws`) that overrides every
    /// region. An escape hatch for new deployments and local mocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// API root from `--region`/`--base-url` or `YUKI_REGION`/`YUKI_BASE_URL`.
    /// Wins over everything in the file and is never written back to it.
    #[serde(skip)]
    pub endpoint_override: Option<String>,
}

/// `Option<Region>` as a plain `"nl"`/`"be"` string.
mod region_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::client::Region;

    pub fn serialize<S: Serializer>(region: &Option<Region>, s: S) -> Result<S::Ok, S::Error> {
        match region {
            Some(r) => s.serialize_str(r.as_str()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Region>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|v| v.parse().map_err(serde::de::Error::custom))
            .transpose()
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        #[cfg(unix)]
        {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config/yuki/config.toml")
        }
        #[cfg(not(unix))]
        {
            directories::ProjectDirs::from("nl", "yukiworks", "yuki")
                .map(|d| d.config_dir().join("config.toml"))
                .unwrap_or_else(|| PathBuf::from("config.toml"))
        }
    }

    pub fn load() -> Result<Self, YukiError> {
        Self::load_from(&Self::default_path())
    }

    pub fn load_from(path: &Path) -> Result<Self, YukiError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| YukiError::Config(format!("failed to read {}: {e}", path.display())))?;
        toml::from_str(&content).map_err(|e| YukiError::Config(format!("invalid config: {e}")))
    }

    pub fn save_to(&self, path: &Path) -> Result<(), YukiError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| YukiError::Config(format!("cannot create config dir: {e}")))?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| YukiError::Config(format!("serialize error: {e}")))?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .map_err(|e| {
                    YukiError::Config(format!("failed to write {}: {e}", path.display()))
                })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                YukiError::Config(format!("failed to write {}: {e}", path.display()))
            })?;
            let mut permissions = file
                .metadata()
                .map_err(|e| YukiError::Config(format!("cannot inspect permissions: {e}")))?
                .permissions();
            permissions.set_mode(0o600);
            file.set_permissions(permissions)
                .map_err(|e| YukiError::Config(format!("cannot secure config file: {e}")))?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            std::fs::write(path, content)
                .map_err(|e| YukiError::Config(format!("failed to write {}: {e}", path.display())))
        }
    }

    /// Apply `--region`/`--base-url` (or their environment variables) for this run.
    /// A base URL beats a region.
    pub fn override_endpoint(&mut self, region: Option<Region>, base_url: Option<&str>) {
        if let Some(url) = base_url.filter(|u| !u.trim().is_empty()) {
            self.endpoint_override = Some(url.trim().to_string());
        } else if let Some(region) = region {
            self.endpoint_override = Some(region.api_root().to_string());
        }
    }

    /// API root for `entry`, or for the shared key when `None`.
    ///
    /// Precedence: runtime override, top-level `base_url`, the administration's
    /// `region`, the top-level `region`, and finally the Netherlands.
    pub fn api_root(&self, entry: Option<&AdminEntry>) -> &str {
        if let Some(url) = self
            .endpoint_override
            .as_deref()
            .or(self.base_url.as_deref())
        {
            return url;
        }
        entry
            .and_then(|e| e.region)
            .or(self.region)
            .unwrap_or_default()
            .api_root()
    }

    /// Resolve the administration a command should run against.
    ///
    /// Falls back to the shared `api_key` when the administration has no key of its
    /// own. A resolved key is never empty: authenticating with an empty string
    /// produces a confusing API-side fault rather than a configuration error.
    pub fn target(&self, override_name: Option<&str>) -> Result<Target<'_>, YukiError> {
        let requested = override_name.unwrap_or(&self.default_admin);
        let (name, entry) = self
            .administrations
            .get_key_value(requested)
            .ok_or_else(|| YukiError::Config(self.unknown_admin_message(requested)))?;

        let api_key = entry.api_key.as_deref().unwrap_or(&self.api_key);
        if api_key.is_empty() {
            return Err(YukiError::Config(format!(
                "no API key for administration {name}. \
                 Run 'yuki init --add --api-key <key>' with a key created inside it."
            )));
        }

        Ok(Target {
            config_name: name,
            domain_id: &entry.domain_id,
            admin_id: &entry.admin_id,
            api_key,
            api_root: self.api_root(Some(entry)),
        })
    }

    /// Every distinct access key in the configuration, each paired with the
    /// administrations configured to use it.
    ///
    /// The shared key comes first, then per-administration keys in name order, so
    /// callers walk them in a stable and predictable sequence. An administration
    /// with no usable key is omitted rather than reported under an empty key.
    pub fn access_keys(&self) -> Vec<AccessKey<'_>> {
        let mut keys: Vec<AccessKey<'_>> = Vec::new();
        if !self.api_key.is_empty() {
            keys.push(AccessKey {
                api_key: &self.api_key,
                api_root: self.api_root(None),
                admins: Vec::new(),
            });
        }

        for (name, entry) in &self.administrations {
            let api_key = entry.api_key.as_deref().unwrap_or(&self.api_key);
            if api_key.is_empty() {
                continue;
            }
            match keys.iter_mut().find(|k| k.api_key == api_key) {
                Some(existing) => existing.admins.push(name.as_str()),
                None => keys.push(AccessKey {
                    api_key,
                    api_root: self.api_root(Some(entry)),
                    admins: vec![name.as_str()],
                }),
            }
        }

        keys
    }

    /// Merge freshly discovered administrations into the configuration.
    ///
    /// Entries reached by a key other than the shared one are stamped with it, so a
    /// later lookup authenticates against the administration the entry belongs to.
    /// Returns the config names that were added and updated, in that order, which is
    /// what `yuki init --add` reports back.
    ///
    /// A name already held by a *different* administration is given a numeric suffix
    /// rather than overwritten: two Yuki tenants may use the same company name, and
    /// replacing one with the other would leave a set of books silently unreachable.
    pub fn merge_administrations<I, S>(
        &mut self,
        discovered: I,
        api_key: &str,
    ) -> (Vec<String>, Vec<String>)
    where
        I: IntoIterator<Item = (S, AdminEntry)>,
        S: Into<String>,
    {
        let mut added = Vec::new();
        let mut updated = Vec::new();

        for (name, mut entry) in discovered {
            let name = self.free_name(name.into(), &entry.admin_id);
            // The shared key stays implicit, so rotating it keeps reaching these.
            if api_key != self.api_key {
                entry.api_key = Some(api_key.to_string());
            }
            if self.administrations.insert(name.clone(), entry).is_some() {
                updated.push(name);
            } else {
                added.push(name);
            }
        }

        (added, updated)
    }

    /// The config name to store `admin_id` under.
    ///
    /// Returns `name` unchanged when it is free or already refers to this same
    /// administration, and otherwise the first free `name_2`, `name_3`, and so on.
    fn free_name(&self, name: String, admin_id: &str) -> String {
        match self.administrations.get(&name) {
            None => name,
            Some(existing) if existing.admin_id == admin_id => name,
            Some(_) => (2..)
                .map(|n| format!("{name}_{n}"))
                .find(|candidate| {
                    self.administrations
                        .get(candidate)
                        .is_none_or(|e| e.admin_id == admin_id)
                })
                .unwrap_or(name),
        }
    }

    fn unknown_admin_message(&self, requested: &str) -> String {
        if self.administrations.is_empty() {
            return format!(
                "unknown administration: {requested}. No administrations are configured; run 'yuki init'."
            );
        }
        let known: Vec<&str> = self.administrations.keys().map(String::as_str).collect();
        format!(
            "unknown administration: {requested} (configured: {}). \
             Run 'yuki admin list' to see what is available.",
            known.join(", ")
        )
    }
}
