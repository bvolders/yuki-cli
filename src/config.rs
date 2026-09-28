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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,

    /// Bank GL accounts `check unmatched` scans when `--bank-account` is not
    /// given, e.g. `["550002", "550003"]`. Empty means the region default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bank_accounts: Vec<String>,

    /// Creditors control accounts (e.g. `440000`) `check unmatched` reads to
    /// learn who a bank payment went to and whether a purchase invoice of that
    /// supplier covers it. Absent means the region default; empty turns it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creditor_accounts: Option<Vec<String>>,

    /// Internal-transfer accounts (e.g. `580000`): a bank debit with a same-day,
    /// same-amount counter-entry here needs no invoice. Absent means the region
    /// default; empty turns it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer_accounts: Option<Vec<String>>,

    /// Description patterns (case-insensitive substrings of the full bank
    /// description, which includes the bank's transaction type) that
    /// `check unmatched` skips: loans, salaries, tax payments and the like.
    /// Absent means the region default; an empty list switches it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmatched_ignore_descriptions: Option<Vec<String>>,

    /// GL account prefixes (e.g. `657` for bank costs) that never come with a
    /// document: a bank line booked straight to one needs no invoice, so
    /// `check unmatched` skips it. A line booked straight to any other account
    /// is reported. Absent means the region default (be: `65`, financial
    /// charges); an empty list reports every GL-booked line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_document_accounts: Option<Vec<String>>,

    /// Per-administration settings this build does not know about, kept
    /// verbatim so an older or newer `yuki` never drops them on save.
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl AdminEntry {
    pub fn new(domain_id: impl Into<String>, admin_id: impl Into<String>) -> Self {
        Self {
            domain_id: domain_id.into(),
            admin_id: admin_id.into(),
            name: None,
            api_key: None,
            region: None,
            bank_accounts: Vec::new(),
            creditor_accounts: None,
            transfer_accounts: None,
            unmatched_ignore_descriptions: None,
            no_document_accounts: None,
            extra: toml::Table::new(),
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

    /// Fold a freshly discovered entry for the same administration into this one.
    ///
    /// Discovery only knows identifiers, the display name and (for `--add`) the
    /// key and an explicit region; everything else the user configured survives.
    pub fn refresh(&mut self, discovered: AdminEntry) {
        self.domain_id = discovered.domain_id;
        self.admin_id = discovered.admin_id;
        if discovered.name.is_some() {
            self.name = discovered.name;
        }
        self.api_key = discovered.api_key;
        if discovered.region.is_some() {
            self.region = discovered.region;
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,
    /// Full API root (e.g. `https://api.yukiworks.be/ws`) that overrides every
    /// region. An escape hatch for new deployments and local mocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Endpoint from `--region`/`--base-url` or `YUKI_REGION`/`YUKI_BASE_URL`.
    /// Wins over everything in the file and is never written back to it.
    #[serde(skip)]
    pub endpoint_override: Option<EndpointOverride>,
}

/// A run-scoped endpoint: where to send requests and, when known, which
/// country's conventions apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointOverride {
    pub api_root: String,
    /// The region given explicitly, or implied by a URL that is a known
    /// deployment's root. `None` for a proxy or mock URL.
    pub region: Option<Region>,
}

/// The resolved endpoint for one administration or key; see [`Config::endpoint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint<'a> {
    pub api_root: &'a str,
    pub region: Region,
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
    /// A base URL beats a region for the endpoint; the region, when given, still
    /// decides the country conventions.
    pub fn override_endpoint(&mut self, region: Option<Region>, base_url: Option<&str>) {
        let url = base_url.map(str::trim).filter(|u| !u.is_empty());
        self.endpoint_override = match (url, region) {
            (Some(url), region) => Some(EndpointOverride {
                api_root: url.to_string(),
                region: region.or_else(|| Region::from_api_root(url)),
            }),
            (None, Some(region)) => Some(EndpointOverride {
                api_root: region.api_root().to_string(),
                region: Some(region),
            }),
            (None, None) => None,
        };
    }

    /// Where requests for `entry` (or the shared key, when `None`) go, and
    /// which country's conventions (chart of accounts, bank formats) apply.
    ///
    /// Layers, highest first:
    /// 1. the runtime override (`--base-url`/`--region` or their env vars), for
    ///    every administration and every key alike;
    /// 2. the administration's own `region`;
    /// 3. the top-level `base_url`;
    /// 4. the top-level `region`;
    /// 5. the Netherlands.
    ///
    /// The API root comes from the first layer present. The region comes from
    /// the first layer that names one: a URL that is not a known deployment's
    /// root (a proxy, a mock) says nothing about the country and is skipped.
    /// A saved `base_url` replaces the default endpoint only, so it cannot pull
    /// an administration that names its own region onto another deployment.
    pub fn endpoint(&self, entry: Option<&AdminEntry>) -> Endpoint<'_> {
        let region_layer = |r: Region| (r.api_root(), Some(r));
        let layers = [
            self.endpoint_override
                .as_ref()
                .map(|o| (o.api_root.as_str(), o.region)),
            entry.and_then(|e| e.region).map(region_layer),
            self.base_url
                .as_deref()
                .map(|url| (url, Region::from_api_root(url))),
            self.region.map(region_layer),
            Some(region_layer(Region::default())),
        ];
        let mut present = layers.into_iter().flatten();
        let (api_root, region) = present.next().expect("the default layer is present");
        let region = region
            .or_else(|| present.find_map(|(_, region)| region))
            .unwrap_or_default();
        Endpoint { api_root, region }
    }

    /// API root for `entry`; see [`Config::endpoint`].
    pub fn api_root(&self, entry: Option<&AdminEntry>) -> &str {
        self.endpoint(entry).api_root
    }

    /// Region for `entry`; see [`Config::endpoint`].
    pub fn region(&self, entry: Option<&AdminEntry>) -> Region {
        self.endpoint(entry).region
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
            match self.administrations.get_mut(&name) {
                Some(existing) => {
                    existing.refresh(entry);
                    updated.push(name);
                }
                None => {
                    self.administrations.insert(name.clone(), entry);
                    added.push(name);
                }
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
