use std::collections::BTreeMap;
use std::fmt::Display;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;
use tokio::task::JoinSet;

use crate::client::Region;
use crate::client::accounting::{AccountingClient, Administration};
use crate::config::{AdminEntry, Config};
use crate::error::YukiError;
use crate::output::is_tty;

fn sym_ok() -> String {
    if is_tty() {
        "✔".green().to_string()
    } else {
        "✔".to_owned()
    }
}

fn sym_fail() -> String {
    if is_tty() {
        "✖".red().to_string()
    } else {
        "✖".to_owned()
    }
}

fn bold(s: &str) -> String {
    if is_tty() {
        s.bold().to_string()
    } else {
        s.to_owned()
    }
}

fn dim(s: &str) -> String {
    if is_tty() {
        s.dimmed().to_string()
    } else {
        s.to_owned()
    }
}

/// Convert an administration name to a safe config key.
///
/// Lowercases the name and replaces any non-alphanumeric character with an underscore.
fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Index discovered administrations by their config key.
fn to_entries(admins: &[Administration]) -> BTreeMap<String, AdminEntry> {
    admins
        .iter()
        .map(|a| {
            (
                safe_name(&a.name),
                AdminEntry::new(&a.domain_id, &a.id).with_name(&a.name),
            )
        })
        .collect()
}

/// The next line of `input`, trimmed; `None` at end of input.
fn next_line(input: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

fn read_line(input: &mut impl BufRead) -> String {
    next_line(input).unwrap_or_default()
}

/// Tell the user an environment region was used for this run but not stored.
fn note_unstored_region(
    region: Option<Region>,
    region_flag: Option<Region>,
    stored: Option<Region>,
) {
    if let Some(region) = region
        && region_flag.is_none()
        && Some(region) != stored
    {
        eprintln!(
            "{}",
            dim(&format!(
                "note: YUKI_REGION={region} applies to this run only; \
                 pass --region {region} to store it."
            ))
        );
    }
}

/// Administrations for a plain `yuki init`: what discovery found, keeping every
/// per-administration setting of an administration that was configured before.
///
/// An earlier entry is matched by `admin_id`, preferring the same config name.
/// The key init was given becomes the shared one, so a matched entry's own key
/// is dropped (discovery entries carry none).
fn rebuild_administrations(
    previous: Option<&BTreeMap<String, AdminEntry>>,
    discovered: BTreeMap<String, AdminEntry>,
) -> BTreeMap<String, AdminEntry> {
    discovered
        .into_iter()
        .map(|(name, entry)| {
            let earlier = previous.and_then(|p| {
                p.get(&name)
                    .filter(|e| e.admin_id == entry.admin_id)
                    .or_else(|| p.values().find(|e| e.admin_id == entry.admin_id))
            });
            let merged = match earlier {
                Some(earlier) => {
                    let mut merged = earlier.clone();
                    merged.refresh(entry);
                    merged
                }
                None => entry,
            };
            (name, merged)
        })
        .collect()
}

/// Where `yuki init` reads its answers, and where it looks for a key's region.
pub struct InitIo<R> {
    /// The configuration file to create or update.
    pub path: PathBuf,
    /// The key, when not passed as a flag, and every answer are read from here.
    pub input: R,
    /// Whether `input` is a person at a terminal, who may be asked a question
    /// that has no safe default. Piped input is never asked one.
    pub interactive: bool,
    /// Every region a key is tried on, with the root to try it at.
    pub probes: Vec<(Region, String)>,
}

/// Hidden: replaces the roots region detection tries, as `code=root,...`, so
/// tests can stand one local mock in for every region. It only redirects where
/// the key is sent, as `YUKI_BASE_URL` already can.
const PROBE_ROOTS_VAR: &str = "YUKI_PROBE_ROOTS";

/// The roots region detection tries: every known region's public host, or
/// exactly the ones `spec` (the value of `YUKI_PROBE_ROOTS`) lists.
pub fn probe_roots(spec: Option<&str>) -> Result<Vec<(Region, String)>, YukiError> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Region::ALL
            .iter()
            .map(|r| (*r, r.api_root().to_string()))
            .collect());
    };
    spec.split(',')
        .map(|pair| {
            let bad = || {
                YukiError::Config(format!(
                    "{PROBE_ROOTS_VAR}: expected code=root, got '{pair}'"
                ))
            };
            let (code, root) = pair.split_once('=').ok_or_else(bad)?;
            let region = Region::from_code(code.trim()).ok_or_else(bad)?;
            Ok((region, root.trim().to_string()))
        })
        .collect()
}

/// Where a key was found to work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deployment<T> {
    /// One of the candidates [`detect`] was given.
    Known(T),
    /// A root the user typed, for a deployment outside the known ones.
    BaseUrl(String),
}

/// The outcome of [`detect`]: where the key works, and a client already
/// authenticated there, so discovery does not authenticate a second time.
pub struct Detected<T> {
    pub deployment: Deployment<T>,
    pub client: AccountingClient,
}

enum Probe {
    Accepted(AccountingClient),
    /// The host answered that it does not know this key.
    Rejected,
    /// The host could not say: unreachable, blocked, rate limited, and so on.
    Failed(YukiError),
}

/// Whether `e` is Yuki saying the key does not exist on the host asked,
/// which is what every other region's host answers for a valid key.
fn rejects_key(e: &YukiError) -> bool {
    matches!(e, YukiError::AuthFailed(m) if m.to_ascii_lowercase().contains("invalid access key"))
}

/// Authenticate `api_key` at every candidate root at once.
async fn probe_all<T>(api_key: &str, candidates: &[(T, String)]) -> Vec<Probe> {
    let mut tasks = JoinSet::new();
    for (i, (_, root)) in candidates.iter().enumerate() {
        let (key, root) = (api_key.to_string(), root.clone());
        tasks.spawn(async move {
            let mut client = AccountingClient::new().with_api_root(&root);
            let probe = match client.authenticate(&key).await {
                Ok(_) => Probe::Accepted(client),
                Err(e) if rejects_key(&e) => Probe::Rejected,
                Err(e) => Probe::Failed(e),
            };
            (i, probe)
        });
    }
    let mut probes: Vec<Option<Probe>> = candidates.iter().map(|_| None).collect();
    while let Some(done) = tasks.join_next().await {
        let (i, probe) = done.expect("region probe task");
        probes[i] = Some(probe);
    }
    probes
        .into_iter()
        .map(|p| p.expect("every probe ran"))
        .collect()
}

/// A root the user typed, without its trailing slash, if it is an http(s) URL.
fn parse_base_url(text: &str) -> Option<String> {
    let url = text.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split('/').next().unwrap_or_default();
    (!host.is_empty() && !url.chars().any(char::is_whitespace)).then(|| url.to_string())
}

fn joined<T: Display>(labels: &[T], sep: &str) -> String {
    labels
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(sep)
}

/// Find where `api_key` works by trying it on every candidate root at once.
///
/// A root that answers "Invalid access key" is not the key's region. Anything
/// else going wrong on a root is reported, never skipped: the key might belong
/// there. With exactly one region accepting the key, that is the answer, and
/// `input` is not touched. With none or several, a person at a terminal
/// (`interactive`) picks one of them, or `other` for a root of their own,
/// which is verified with one Authenticate; there is no default. Without a
/// terminal, none is the auth error and several a configuration error, each
/// naming the flag that settles it.
pub async fn detect<T, R>(
    api_key: &str,
    candidates: &[(T, String)],
    input: &mut R,
    interactive: bool,
) -> Result<Detected<T>, YukiError>
where
    T: Copy + Eq + Display,
    R: BufRead,
{
    eprint!("Detecting region...");
    io::stderr().flush().ok();
    let mut accepted = Vec::new();
    let mut failed = Vec::new();
    let probes = probe_all(api_key, candidates).await;
    for ((label, root), probe) in candidates.iter().zip(probes) {
        match probe {
            Probe::Accepted(client) => accepted.push((*label, client)),
            Probe::Rejected => {}
            Probe::Failed(e) => failed.push((root.as_str(), e)),
        }
    }

    if !failed.is_empty() {
        eprintln!(" {}", sym_fail());
        for (root, e) in &failed {
            eprintln!("  could not check {root}: {e}");
        }
        for (label, _) in &accepted {
            eprintln!("  region {label} accepts the key, but the check is incomplete.");
        }
        eprintln!(
            "  {}",
            dim("Pass --region (or --base-url) to skip region detection.")
        );
        // The first failure decides the exit code, e.g. 4 when rate limited.
        return Err(failed.swap_remove(0).1);
    }

    if accepted.len() == 1 {
        eprintln!(" {}", sym_ok());
        let (label, client) = accepted.remove(0);
        return Ok(Detected {
            deployment: Deployment::Known(label),
            client,
        });
    }

    let tried: Vec<&str> = candidates.iter().map(|(_, root)| root.as_str()).collect();
    let rejected = || {
        YukiError::AuthFailed(format!(
            "Invalid access key: rejected by {}. \
             For a Yuki deployment outside these, pass --base-url <root>.",
            tried.join(", ")
        ))
    };
    let offered: Vec<T> = if accepted.is_empty() {
        eprintln!(" {}", sym_fail());
        eprintln!("No known Yuki region accepts this key.");
        candidates.iter().map(|(label, _)| *label).collect()
    } else {
        let labels: Vec<T> = accepted.iter().map(|(label, _)| *label).collect();
        eprintln!(" {}", sym_ok());
        eprintln!(
            "This key is accepted by more than one Yuki region: {}.",
            joined(&labels, ", ")
        );
        labels
    };
    let codes = joined(&offered, "/");
    let no_choice = |accepted_any: bool| {
        if accepted_any {
            YukiError::Config(format!(
                "this key is accepted by more than one Yuki region ({}); \
                 pass --region <{codes}> or --base-url <root>",
                joined(&offered, ", ")
            ))
        } else {
            rejected()
        }
    };
    let accepted_any = !accepted.is_empty();
    if !interactive {
        return Err(no_choice(accepted_any));
    }

    loop {
        eprint!("Yuki region [{codes}/other]: ");
        io::stderr().flush().ok();
        let Some(answer) = next_line(input) else {
            eprintln!();
            return Err(no_choice(accepted_any));
        };
        let answer = answer.to_ascii_lowercase();
        if answer == "other" {
            let url = loop {
                eprint!("API root (e.g. https://api.yukiworks.example/ws): ");
                io::stderr().flush().ok();
                let Some(text) = next_line(input) else {
                    eprintln!();
                    return Err(no_choice(accepted_any));
                };
                match parse_base_url(&text) {
                    Some(url) => break url,
                    None => eprintln!("  not an http(s) URL: {text}"),
                }
            };
            let client = authenticate(api_key, &url).await?;
            return Ok(Detected {
                deployment: Deployment::BaseUrl(url),
                client,
            });
        }
        match offered.iter().position(|label| label.to_string() == answer) {
            // Every offered region already rejected the key.
            Some(_) if !accepted_any => return Err(rejected()),
            Some(i) => {
                let (label, client) = accepted.swap_remove(i);
                return Ok(Detected {
                    deployment: Deployment::Known(label),
                    client,
                });
            }
            None => eprintln!("  answer {codes} or other"),
        }
    }
}

/// Say where the key was found, and return the endpoint to record for it:
/// a region, or a root outside every known one. A typed root that is a known
/// region's host counts as that region.
fn announce(deployment: Deployment<Region>) -> (Option<Region>, Option<String>) {
    let region = match &deployment {
        Deployment::Known(region) => Some(*region),
        Deployment::BaseUrl(url) => Region::from_api_root(url),
    };
    match (region, deployment) {
        (Some(region), _) => {
            eprintln!("Detected Yuki {} ({})", region.country(), region.host());
            (Some(region), None)
        }
        (None, Deployment::BaseUrl(url)) => {
            eprintln!("Using {url}");
            (None, Some(url))
        }
        (None, Deployment::Known(_)) => unreachable!("a known deployment has a region"),
    }
}

/// Authenticate with `api_key` at `api_root`.
async fn authenticate(api_key: &str, api_root: &str) -> Result<AccountingClient, YukiError> {
    eprint!("Authenticating...");
    io::stderr().flush().ok();
    let mut client = AccountingClient::new().with_api_root(api_root);
    match client.authenticate(api_key).await {
        Ok(_) => {
            eprintln!(" {}", sym_ok());
            Ok(client)
        }
        Err(e) => {
            eprintln!(" {}", sym_fail());
            Err(e)
        }
    }
}

/// List the administrations an authenticated `client` reaches.
async fn discover(client: &AccountingClient) -> Result<Vec<Administration>, YukiError> {
    eprint!("Fetching administrations...");
    io::stderr().flush().ok();
    match client.administrations().await {
        Ok(admins) => {
            eprintln!(" {}", sym_ok());
            Ok(admins)
        }
        Err(e) => {
            eprintln!(" {}", sym_fail());
            Err(e)
        }
    }
}

/// Run `yuki init`.
///
/// `region` is the deployment for this run, from `--region` or `YUKI_REGION`;
/// `region_flag` is the same value only when `--region` was typed on the command
/// line. `base_url` (flag or env) is resolved the same way.
///
/// A fresh init — no configuration exists yet — has nothing to protect from the
/// environment, so it persists whichever endpoint `region`/`base_url` actually
/// resolved to, exactly as a typed `--region`/`--base-url` would be. Re-running
/// `init` on an existing configuration (or rotating the key, or `--add`) only
/// ever persists the flag: an exported environment variable is a per-shell
/// choice there and must not silently rewrite the configuration.
///
/// With neither given, the region is detected from the key (see [`detect`]) and
/// recorded, whichever it is.
pub async fn run(
    api_key: Option<&str>,
    default_admin: Option<&str>,
    add: bool,
    region: Option<Region>,
    region_flag: Option<Region>,
    base_url: Option<&str>,
) -> Result<(), YukiError> {
    let stdin = io::stdin();
    let io = InitIo {
        path: Config::default_path(),
        interactive: stdin.is_terminal(),
        input: stdin.lock(),
        probes: probe_roots(std::env::var(PROBE_ROOTS_VAR).ok().as_deref())?,
    };
    run_with(
        io,
        api_key,
        default_admin,
        add,
        region,
        region_flag,
        base_url,
    )
    .await
}

/// [`run`] over explicit input, configuration path and probe roots.
pub async fn run_with<R: BufRead>(
    mut io: InitIo<R>,
    api_key: Option<&str>,
    default_admin: Option<&str>,
    add: bool,
    region: Option<Region>,
    region_flag: Option<Region>,
    base_url: Option<&str>,
) -> Result<(), YukiError> {
    let path = io.path.clone();

    // Rotating the shared key touches nothing else, so it skips discovery entirely.
    if !add
        && let (Some(key), None) = (api_key, default_admin)
        && let Ok(mut config) = Config::load_from(&path)
    {
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(YukiError::Config("API key cannot be empty".to_string()));
        }

        if let Some(region) = region_flag {
            config.region = Some(region);
        }
        note_unstored_region(region, region_flag, config.region);
        config.override_endpoint(region, base_url);

        eprintln!("Verifying new API key...");
        let mut client = AccountingClient::new().with_api_root(config.api_root(None));
        client.authenticate(&key).await?;

        config.api_key = key;
        config.save_to(&path)?;
        eprintln!("{} API key updated in {}", sym_ok(), path.display());
        return Ok(());
    }

    let api_key = match api_key {
        Some(k) => k.trim().to_string(),
        None => {
            eprintln!("  {} Yuki Portal → Settings → API keys", dim("→"));
            eprint!("Yuki API key: ");
            io::stderr().flush().ok();
            read_line(&mut io.input)
        }
    };

    if api_key.is_empty() {
        return Err(YukiError::Config("API key cannot be empty".to_string()));
    }

    if add {
        return add_key(
            &mut io,
            &api_key,
            default_admin,
            region,
            region_flag,
            base_url,
        )
        .await;
    }

    let existing = Config::load_from(&path).ok();
    let saved_url = existing.as_ref().and_then(|c| c.base_url.clone());
    // Start from what will be saved, so discovery resolves the endpoint exactly
    // as later commands will. The run override is never saved.
    let mut config = Config {
        api_key,
        default_admin: String::new(),
        administrations: BTreeMap::new(),
        // Preserve unmatched_ignore from the existing config if present.
        unmatched_ignore: existing
            .as_ref()
            .map(|c| c.unmatched_ignore.clone())
            .unwrap_or_default(),
        region: None,
        base_url: saved_url.clone(),
        endpoint_override: None,
    };

    // A saved base_url is an endpoint the user chose, like a flag: keep it.
    let client = if region.is_some() || base_url.is_some() || saved_url.is_some() {
        if existing.is_none() {
            // A fresh init has nothing to overwrite, so the endpoint this run
            // actually used — from a flag or its environment variable alike —
            // is the one to record, exactly as a typed --region/--base-url
            // would be. Otherwise the next run without the environment variable
            // would silently fall back to the legacy `nl` default.
            config.override_endpoint(region, base_url);
            let resolved = config
                .endpoint_override
                .clone()
                .expect("region or base_url was given");
            config.region = resolved.region;
            // A URL matching no known region is recorded as `base_url`, like
            // `detect`'s "other" answer; one that does names its region, so the
            // runtime URL (possibly a test mock, not the region's real host) is
            // dropped rather than baked into the config.
            config.base_url = resolved.region.is_none().then_some(resolved.api_root);
        } else {
            // A re-run keeps the stored region unless the flag says otherwise:
            // an exported variable must not silently rewrite an existing config.
            config.region = region_flag.or(existing.as_ref().and_then(|c| c.region));
            note_unstored_region(region, region_flag, config.region);
            config.override_endpoint(region, base_url);
        }
        authenticate(&config.api_key, config.api_root(None)).await?
    } else {
        let found = detect(&config.api_key, &io.probes, &mut io.input, io.interactive).await?;
        (config.region, config.base_url) = announce(found.deployment);
        found.client
    };

    let admins = discover(&client).await?;
    if admins.is_empty() {
        return Err(YukiError::NotFound(
            "no administrations found for this API key".to_string(),
        ));
    }

    eprintln!("Found {} administration(s):", admins.len());
    for (i, a) in admins.iter().enumerate() {
        eprintln!("  [{}] {}", i + 1, a.name);
    }

    let default_name = if let Some(name) = default_admin {
        // Use the provided name directly, verifying it exists.
        let key = safe_name(name);
        if !admins.iter().any(|a| safe_name(&a.name) == key) {
            return Err(YukiError::Config(format!(
                "administration not found: {name}"
            )));
        }
        eprintln!("Using \"{name}\" as the default administration.");
        key
    } else if admins.len() == 1 {
        eprintln!(
            "Using \"{}\" as the default administration.",
            admins[0].name
        );
        safe_name(&admins[0].name)
    } else {
        eprint!("Select default administration [1]: ");
        io::stderr().flush().ok();

        let choice = read_line(&mut io.input);
        let idx: usize = if choice.is_empty() {
            1
        } else {
            choice
                .parse::<usize>()
                .map_err(|_| YukiError::Config(format!("invalid selection: {choice}")))?
        };

        if idx == 0 || idx > admins.len() {
            return Err(YukiError::Config(format!("selection out of range: {idx}")));
        }
        safe_name(&admins[idx - 1].name)
    };

    let administrations = rebuild_administrations(
        existing.as_ref().map(|c| &c.administrations),
        to_entries(&admins),
    );

    // A plain init replaces the administration map, so anything this key cannot reach
    // is about to disappear. Say which, rather than let a second set of books vanish.
    if let Some(previous) = &existing {
        let dropped: Vec<&str> = previous
            .administrations
            .iter()
            .filter(|(name, entry)| {
                administrations
                    .get(*name)
                    .is_none_or(|new| new.admin_id != entry.admin_id)
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if !dropped.is_empty() {
            eprintln!();
            eprintln!(
                "{} this key does not reach {}, which init removes from the config.",
                sym_fail(),
                dropped.join(", ")
            );
            eprintln!(
                "  {}",
                dim("Run 'yuki init --add --api-key <key>' instead to keep both.")
            );
            eprintln!();
        }
    }

    config.default_admin = default_name;
    config.administrations = administrations;
    config.save_to(&path)?;

    eprintln!();
    eprintln!("{} Configuration saved to {}", sym_ok(), path.display());
    eprintln!();
    eprintln!("{}:", bold("Next steps"));
    eprintln!(
        "  yuki documents list  {}",
        dim("# list archived documents")
    );
    eprintln!("  yuki contacts list   {}", dim("# list contacts"));
    eprintln!("  yuki invoices list   {}", dim("# list invoices"));
    eprintln!("  yuki completions zsh {}", dim("# shell completions"));
    eprintln!();

    Ok(())
}

/// Merge a second key's administrations into an existing configuration.
///
/// A Yuki access key is scoped to the administration it was created in, so this is
/// how a second set of books becomes reachable: the key is stored against the
/// administrations it opens, and every command that targets one authenticates with it.
async fn add_key<R: BufRead>(
    io: &mut InitIo<R>,
    api_key: &str,
    default_admin: Option<&str>,
    region: Option<Region>,
    region_flag: Option<Region>,
    base_url: Option<&str>,
) -> Result<(), YukiError> {
    let path: &Path = &io.path;
    // Distinguish "nothing to add to" from a config that exists but will not parse;
    // the second is a different problem and keeps its own error.
    if !path.exists() {
        return Err(YukiError::Config(format!(
            "no configuration at {}. Run 'yuki init' first; --add extends an existing one.",
            path.display()
        )));
    }
    let mut config = Config::load_from(path)?;

    // The key's administrations record the endpoint it was found on, so Dutch
    // and Belgian books (or another deployment's) can coexist in one config.
    // Only the flag stamps a region: an exported YUKI_REGION is for this run.
    let (client, stamp, stamp_url) = if region.is_some() || base_url.is_some() {
        note_unstored_region(region, region_flag, config.region);
        // Resolve the endpoint as for an administration carrying the stamp.
        let probe = AdminEntry {
            region: region_flag,
            ..AdminEntry::new("", "")
        };
        config.override_endpoint(region, base_url);
        let api_root = config.api_root(Some(&probe)).to_string();
        (authenticate(api_key, &api_root).await?, region_flag, None)
    } else {
        let found = detect(api_key, &io.probes, &mut io.input, io.interactive).await?;
        let (stamp, stamp_url) = announce(found.deployment);
        (found.client, stamp, stamp_url)
    };

    let admins = discover(&client).await?;
    if admins.is_empty() {
        return Err(YukiError::NotFound(
            "no administrations found for this API key".to_string(),
        ));
    }

    let mut entries = to_entries(&admins);
    for entry in entries.values_mut() {
        entry.region = stamp;
        entry.base_url = stamp_url.clone();
    }
    let (added, updated) = config.merge_administrations(entries, api_key);

    if let Some(name) = default_admin {
        let key = safe_name(name);
        if !config.administrations.contains_key(&key) {
            return Err(YukiError::Config(format!(
                "administration not found: {name}"
            )));
        }
        config.default_admin = key;
    }

    config.save_to(path)?;

    eprintln!();
    for name in &added {
        eprintln!("{} added {name}", sym_ok());
    }
    for name in &updated {
        eprintln!("{} updated {name}", sym_ok());
    }
    eprintln!(
        "{} Configuration saved to {} (default: {})",
        sym_ok(),
        path.display(),
        config.default_admin
    );
    eprintln!();
    eprintln!("{}:", bold("Next steps"));
    eprintln!("  yuki admin list                {}", dim("# verify both"));
    for name in added.iter().chain(updated.iter()) {
        eprintln!("  yuki --admin {name} documents list");
    }
    eprintln!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_lowercases_and_replaces_punctuation() {
        assert_eq!(safe_name("Example Holding B.V."), "example_holding_b_v_");
        // Digits survive; only non-alphanumerics become underscores.
        assert_eq!(safe_name("Acme 42 B.V."), "acme_42_b_v_");
    }

    #[test]
    fn to_entries_records_the_display_name() {
        let admins = vec![Administration {
            name: "Example Holding B.V.".into(),
            id: "admin-1".into(),
            domain_id: "domain-1".into(),
        }];
        let entries = to_entries(&admins);
        let entry = &entries["example_holding_b_v_"];
        assert_eq!(entry.name.as_deref(), Some("Example Holding B.V."));
        assert_eq!(entry.admin_id, "admin-1");
        assert_eq!(entry.domain_id, "domain-1");
        // Discovery does not decide which key an entry belongs to; merging does.
        assert_eq!(entry.api_key, None);
        assert_eq!(entry.region, None);
    }

    #[test]
    fn a_plain_reinit_keeps_the_settings_of_administrations_that_still_exist() {
        let mut earlier = AdminEntry::new("domain-old", "admin-be").with_api_key("stale-key");
        earlier.region = Some(Region::Be);
        earlier.bank_accounts = vec!["550002".into()];
        earlier.creditor_accounts = Some(vec!["440000".into()]);
        earlier.unmatched_ignore_descriptions = Some(vec![]);
        let previous = BTreeMap::from([("voorbeeld_bv".to_string(), earlier)]);
        let discovered = to_entries(&[Administration {
            name: "Voorbeeld BV".into(),
            id: "admin-be".into(),
            domain_id: "domain-be".into(),
        }]);

        let rebuilt = rebuild_administrations(Some(&previous), discovered);
        let entry = &rebuilt["voorbeeld_bv"];
        assert_eq!(entry.domain_id, "domain-be");
        assert_eq!(entry.region, Some(Region::Be));
        assert_eq!(entry.bank_accounts, ["550002"]);
        assert_eq!(entry.creditor_accounts, Some(vec!["440000".to_string()]));
        assert_eq!(entry.unmatched_ignore_descriptions, Some(vec![]));
        // The new key is the shared one now, so the stale per-admin key goes.
        assert_eq!(entry.api_key, None);
    }

    #[test]
    fn probe_roots_default_to_every_known_host() {
        let roots = probe_roots(None).unwrap();
        assert_eq!(roots.len(), Region::ALL.len());
        for (region, root) in &roots {
            assert_eq!(root, region.api_root());
        }
        assert_eq!(probe_roots(Some(" ")).unwrap(), roots);
    }

    #[test]
    fn probe_roots_take_exactly_the_listed_regions() {
        assert_eq!(
            probe_roots(Some("be=http://127.0.0.1:1/be/ws")).unwrap(),
            [(Region::Be, "http://127.0.0.1:1/be/ws".to_string())]
        );
        for bad in ["be", "de=http://x/ws"] {
            assert!(probe_roots(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_typed_root_must_be_an_http_url() {
        assert_eq!(
            parse_base_url(" https://api.yukiworks.example/ws/ ").as_deref(),
            Some("https://api.yukiworks.example/ws")
        );
        assert_eq!(
            parse_base_url("http://127.0.0.1:8080/ws").as_deref(),
            Some("http://127.0.0.1:8080/ws")
        );
        for bad in [
            "",
            "other",
            "api.yukiworks.be/ws",
            "https://",
            "https:///ws",
            "https://a b/ws",
        ] {
            assert_eq!(parse_base_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_typed_root_of_a_known_host_is_recorded_as_its_region() {
        assert_eq!(
            announce(Deployment::BaseUrl("https://api.yukiworks.be/ws".into())),
            (Some(Region::Be), None)
        );
        assert_eq!(
            announce(Deployment::BaseUrl("http://proxy/ws".into())),
            (None, Some("http://proxy/ws".into()))
        );
    }

    #[test]
    fn only_invalid_access_key_means_another_region() {
        assert!(rejects_key(&YukiError::AuthFailed(
            "Invalid access key".into()
        )));
        // A WAF or proxy refusing the request says nothing about the key.
        assert!(!rejects_key(&YukiError::AuthFailed("HTTP 403".into())));
        assert!(!rejects_key(&YukiError::RateLimited));
    }
}
