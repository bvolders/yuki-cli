pub mod account;
pub mod accounts;
pub mod admin;
pub mod check;
pub mod contacts;
pub mod documents;
pub mod init;
pub mod invoices;
pub mod projects;
pub mod sales;
pub mod upload;
pub mod vat;

use std::ffi::OsString;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{CommandFactory, Parser, Subcommand};

use crate::client::Region;
use crate::client::accounting::AccountingClient;
use crate::config::{Config, Target};
use crate::error::YukiError;

/// Authenticate a client and set the active administration domain.
///
/// Returns both the configured client and the resolved `Target` so callers can pass
/// `admin_id` to operations that require `administrationID`.
pub async fn setup_domain<'a>(
    config: &'a Config,
    admin: Option<&str>,
) -> Result<(AccountingClient, Target<'a>), YukiError> {
    let target = config.target(admin)?;
    let mut client = AccountingClient::new().with_api_root(target.api_root);
    client.authenticate(target.api_key).await?;
    client.set_current_domain(target.domain_id).await?;
    Ok((client, target))
}

/// Top-level CLI entry point for the Yuki bookkeeping API client.
#[derive(Parser)]
#[command(
    name = "yuki",
    version,
    about = "CLI client for the Yuki bookkeeping API"
)]
pub struct Cli {
    /// Override the active administration by name.
    #[arg(long = "profile", visible_alias = "admin", global = true)]
    pub admin: Option<String>,

    /// Output format: auto, text, or json.
    #[arg(long = "output", short = 'o', global = true)]
    pub output: Option<String>,

    /// Suppress all output except errors.
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Skip confirmation prompts (for use in scripts and pipelines).
    #[arg(long = "yes", short = 'y', global = true)]
    pub yes: bool,

    /// Yuki deployment, e.g. be (api.yukiworks.be) or nl (api.yukiworks.nl).
    /// `yuki init` detects it from the key when omitted, and stores it.
    /// Overrides the configured region for this run. YUKI_REGION does the
    /// same when the flag is absent. `init` stores whichever was used on a
    /// fresh config; re-running it on an existing one never lets the
    /// environment variable rewrite the stored region.
    #[arg(
        long,
        global = true,
        value_parser = PossibleValuesParser::new(Region::codes())
            .map(|s| s.parse::<Region>().expect("validated by PossibleValuesParser")),
    )]
    pub region: Option<Region>,

    /// Full API root, e.g. https://api.yukiworks.be/ws. Overrides --region.
    /// YUKI_BASE_URL does the same when the flag is absent. `init` stores it
    /// (as `base_url`) only on a fresh config and only when it names no known
    /// region; re-running `init` on an existing config never stores it.
    #[arg(long, global = true)]
    pub base_url: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

/// The endpoint requested for this run, before the configuration is consulted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunEndpoint {
    /// `--region`, else `YUKI_REGION`.
    pub region: Option<Region>,
    /// `--base-url`, else `YUKI_BASE_URL`; an empty value means unset.
    pub base_url: Option<String>,
}

impl Cli {
    /// The flags, each falling back to its environment variable.
    pub fn run_endpoint(&self) -> Result<RunEndpoint, clap::Error> {
        self.run_endpoint_with(|name| std::env::var_os(name))
    }

    /// [`Cli::run_endpoint`] over an explicit environment lookup.
    ///
    /// A set `YUKI_REGION` must be `nl` or `be`, exactly as the flag, and a bad
    /// value is a usage error like a bad flag. A variable that is not valid
    /// UTF-8 is a usage error too, as it was while clap read these variables,
    /// rather than being silently treated as unset.
    pub fn run_endpoint_with(
        &self,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<RunEndpoint, clap::Error> {
        let var = |name: &str| -> Result<Option<String>, clap::Error> {
            env(name)
                .map(|value| {
                    value.into_string().map_err(|_| {
                        Self::command().error(
                            clap::error::ErrorKind::InvalidUtf8,
                            format!("{name} is not valid UTF-8"),
                        )
                    })
                })
                .transpose()
        };
        // The environment is read only when the flag is absent, as clap did.
        let region = match self.region {
            Some(region) => Some(region),
            None => match var("YUKI_REGION")? {
                Some(value) => Some(Region::from_code(&value).ok_or_else(|| {
                    Self::command().error(
                        clap::error::ErrorKind::InvalidValue,
                        format!(
                            "invalid value '{value}' for YUKI_REGION [possible values: {}]",
                            Region::codes().join(", ")
                        ),
                    )
                })?),
                None => None,
            },
        };
        let base_url = self
            .base_url
            .clone()
            .map_or_else(|| var("YUKI_BASE_URL"), |url| Ok(Some(url)))?
            .filter(|u| !u.trim().is_empty());
        Ok(RunEndpoint { region, base_url })
    }
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize yuki configuration for this machine.
    ///
    /// The key's region is detected by trying it on every known Yuki host,
    /// unless --region or --base-url is given, and recorded in the config.
    Init {
        /// API key (skips interactive prompt if provided).
        #[arg(long)]
        api_key: Option<String>,

        /// Default administration name (auto-selects if only one available).
        #[arg(long)]
        default_admin: Option<String>,

        /// Merge the key's administrations into the existing config instead of
        /// replacing it. Use this to reach a second administration, which Yuki
        /// exposes only through a key created inside it.
        #[arg(long)]
        add: bool,
    },

    /// Manage authentication.
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },

    /// Manage configuration profiles (Yuki administrations).
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },

    /// Inspect configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Check configuration and Yuki connectivity.
    Doctor {
        /// Check local configuration without contacting Yuki.
        #[arg(long)]
        offline: bool,
    },

    /// Manage Yuki administrations.
    Admin {
        #[command(subcommand)]
        command: AdminCommands,
    },

    /// Work with outstanding sales and purchase invoices.
    Invoices {
        #[command(subcommand)]
        command: InvoiceCommands,
    },

    /// Work with archived documents.
    Documents {
        #[command(subcommand)]
        command: DocumentCommands,
    },

    /// Work with contacts (customers and suppliers).
    Contacts {
        #[command(subcommand)]
        command: ContactCommands,
    },

    /// Work with general ledger accounts.
    Accounts {
        #[command(subcommand)]
        command: AccountCommands,
    },

    /// Work with VAT returns and codes.
    Vat {
        #[command(subcommand)]
        command: VatCommands,
    },

    /// Work with the sales catalogue.
    Sales {
        #[command(subcommand)]
        command: SalesCommands,
    },

    /// Work with projects.
    Projects {
        #[command(subcommand)]
        command: ProjectCommands,
    },

    /// Run compliance and period checks.
    Check {
        #[command(subcommand)]
        command: CheckCommands,
    },

    /// Upload documents to the Yuki archive.
    Upload {
        #[command(subcommand)]
        command: UploadCommands,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },

    /// Output JSON schema for agent integration
    Schema,

    /// Describe supported API areas and safety behavior without loading configuration
    Capabilities,
}

#[derive(Subcommand)]
pub enum AuthCommands {
    /// Configure an API key and discover its administrations.
    Login {
        /// API key (skips interactive prompt if provided).
        #[arg(long)]
        api_key: Option<String>,

        /// Default administration name (auto-selects if only one available).
        #[arg(long)]
        default_admin: Option<String>,

        /// Merge the key's administrations into the existing configuration.
        #[arg(long)]
        add: bool,
    },

    /// Show whether the selected profile is configured and valid.
    Status {
        /// Check local configuration without contacting Yuki.
        #[arg(long)]
        offline: bool,
    },

    /// Remove credentials for the selected profile.
    Logout,
}

#[derive(Subcommand)]
pub enum ProfileCommands {
    /// List locally configured administration profiles.
    List,
    /// Select the default administration profile.
    Use {
        /// Profile name to select.
        name: String,
    },
    /// Remove an administration profile.
    Remove {
        /// Profile name to remove.
        name: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show configuration without revealing API keys.
    Show,
    /// Print the configuration file path.
    Path,
}

#[derive(Subcommand)]
pub enum AdminCommands {
    /// List all available administrations.
    List {
        /// Report what is configured without contacting the API.
        #[arg(long)]
        local: bool,

        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<usize>,

        /// Number of results to skip (for pagination).
        #[arg(long)]
        offset: Option<usize>,

        /// Comma-separated list of fields to include in output.
        #[arg(long)]
        fields: Option<String>,
    },

    /// Switch the active administration.
    Switch {
        /// Name of the administration to activate.
        name: String,
    },
}

#[derive(Subcommand)]
pub enum InvoiceCommands {
    /// List outstanding (open) invoices, optionally filtered by period and type.
    List {
        /// Only items dated within this period (e.g. 2025, 2025-Q1, 2025-01).
        #[arg(long)]
        period: Option<String>,

        /// Invoice type: sales (default, debtor items) or purchase (creditor items).
        #[arg(long)]
        invoice_type: Option<String>,

        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<usize>,

        /// Number of results to skip (for pagination).
        #[arg(long)]
        offset: Option<usize>,

        /// Comma-separated list of fields to include in output.
        #[arg(long)]
        fields: Option<String>,
    },

    /// Show one transaction by ID.
    ///
    /// Yuki cannot look a transaction up by ID, so this fetches every line on
    /// --account within --period (one API call) and keeps the matching one.
    Show {
        /// Transaction ID (as shown by `accounts transactions`).
        id: String,

        /// GL account code the transaction is booked on.
        #[arg(long)]
        account: String,

        /// Period to search (e.g. 2025, 2025-Q1, 2025-01). Defaults to the current year.
        #[arg(long)]
        period: Option<String>,
    },

    /// Show the document linked to a transaction.
    Document {
        /// Transaction ID.
        id: String,
    },
}

#[derive(Subcommand)]
pub enum SalesCommands {
    /// List sales items (the products and services you invoice).
    Items {
        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<usize>,

        /// Number of results to skip (for pagination).
        #[arg(long)]
        offset: Option<usize>,

        /// Comma-separated list of fields to include in output.
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DocumentCommands {
    /// List documents in a folder or of a given type.
    List {
        /// Archive folder: uitzoeken, inkoop, verkoop, bank, personeel, belasting,
        /// overig-financieel, or a numeric folder ID.
        #[arg(long)]
        folder: Option<String>,

        /// Document type filter (numeric document type ID).
        #[arg(long)]
        doc_type: Option<String>,

        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<usize>,

        /// Number of results to skip (for pagination).
        #[arg(long)]
        offset: Option<usize>,

        /// Comma-separated list of fields to include in output.
        #[arg(long)]
        fields: Option<String>,
    },

    /// Search documents by a query string.
    Search {
        /// Search query.
        query: String,
    },

    /// Check if an invoice exists in the archive (by amount, date, and optional contact).
    Exists {
        /// Invoice amount to search for.
        #[arg(long)]
        amount: f64,
        /// Invoice date: YYYY-MM-DD matches within +/-7 days; a period (2025, 2025-Q1, 2025-03) matches the whole period.
        #[arg(long)]
        date: String,
        /// Contact/supplier name to narrow the search.
        #[arg(long)]
        contact: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ContactCommands {
    /// Search contacts by name or other criteria.
    Search {
        /// Search query.
        query: String,
    },

    /// List contacts filtered by type.
    List {
        /// Contact type (e.g. customer, supplier).
        #[arg(long)]
        contact_type: Option<String>,

        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<usize>,

        /// Number of results to skip (for pagination).
        #[arg(long)]
        offset: Option<usize>,

        /// Comma-separated list of fields to include in output.
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AccountCommands {
    /// Show GL account balances at the end of a period (or today, if it is still running).
    Balance {
        /// GL account code.
        #[arg(long)]
        account: Option<String>,

        /// Accounting period (e.g. 2025-01); the balance is taken on its last day, clamped to today.
        #[arg(long)]
        period: Option<String>,
    },

    /// List transactions for a general ledger account.
    Transactions {
        /// GL account code.
        #[arg(long)]
        account: Option<String>,

        /// Accounting period (e.g. 2025-01).
        #[arg(long)]
        period: Option<String>,

        /// Maximum number of results to return.
        #[arg(long)]
        limit: Option<usize>,

        /// Number of results to skip (for pagination).
        #[arg(long)]
        offset: Option<usize>,

        /// Comma-separated list of fields to include in output.
        #[arg(long)]
        fields: Option<String>,
    },

    /// Show the chart of accounts (GL account scheme).
    Scheme,

    /// Show net revenue for a period.
    Revenue {
        /// Accounting period (e.g. 2025, 2025-Q1, 2025-01).
        #[arg(long)]
        period: Option<String>,
    },

    /// Show opening balances per GL account for a book year.
    StartBalance {
        /// Book year (e.g. 2025).
        #[arg(long)]
        year: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ProjectCommands {
    /// List all projects.
    List,

    /// Show balance for a project.
    Balance {
        /// Project code.
        project: String,

        /// GL account code filter.
        #[arg(long)]
        account: Option<String>,

        /// Accounting period (e.g. 2025, 2025-Q1).
        #[arg(long)]
        period: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum VatCommands {
    /// List VAT returns for a given year.
    Returns {
        /// Fiscal year (e.g. 2025).
        year: Option<String>,
    },

    /// List active VAT codes.
    Codes,
}

#[derive(Subcommand)]
pub enum CheckCommands {
    /// Check outstanding BTW (VAT) items for a period.
    Btw {
        /// Accounting period (e.g. 2025-01).
        period: Option<String>,
    },

    /// Find bank transactions without matching booked invoices.
    Unmatched {
        /// Accounting period (e.g. 2025-Q1).
        #[arg(long)]
        period: Option<String>,
        /// Bank GL account(s) to scan; repeat or comma-separate for several.
        /// Defaults to the administration's `bank_accounts`, else the region
        /// default (nl: 11001, be: 550000).
        #[arg(long, value_delimiter = ',')]
        bank_account: Vec<String>,
    },

    /// Suggest which payments already made settle the open purchase invoices.
    ///
    /// Read-only: Yuki's API cannot link a payment to an invoice, so confirm
    /// each suggestion in the Yuki UI.
    Matches {
        /// Only open invoices dated in this period (e.g. 2026-Q2); payments
        /// are still read up to today.
        #[arg(long)]
        period: Option<String>,
        /// Bank GL account(s) to read payments from; repeat or comma-separate.
        /// Defaults as for `check unmatched`.
        #[arg(long, value_delimiter = ',')]
        bank_account: Vec<String>,
        /// Also list payments booked to a supplier that no open invoice takes.
        #[arg(long)]
        unallocated: bool,
    },

    /// Check if a specific invoice reference is still outstanding.
    Outstanding {
        /// Invoice reference to check.
        reference: String,
    },
}

#[derive(Subcommand)]
pub enum UploadCommands {
    /// Upload a document with optional invoice metadata.
    File {
        /// Path to the file to upload.
        file: String,

        /// Target folder: uitzoeken (default), inkoop, verkoop, bank, personeel, belasting, overig-financieel.
        #[arg(long, default_value = "uitzoeken")]
        folder: String,

        /// Invoice amount (e.g. 114.27); enables richer metadata upload.
        #[arg(long)]
        amount: Option<f64>,

        /// Cost category ID (e.g. 45100).
        #[arg(long)]
        category: Option<String>,

        /// Payment method ID (e.g. 4 for pinpas).
        #[arg(long = "payment-method")]
        payment_method: Option<String>,

        /// Project ID.
        #[arg(long)]
        project: Option<String>,

        /// Remarks or notes.
        #[arg(long)]
        remarks: Option<String>,

        /// Currency code (default: EUR).
        #[arg(long, default_value = "EUR")]
        currency: String,
    },

    /// List available cost categories.
    Categories,

    /// List available payment methods.
    PaymentMethods,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(args: &[&str], env: &[(&str, &str)]) -> Result<RunEndpoint, clap::Error> {
        let env: Vec<(&str, OsString)> = env.iter().map(|(k, v)| (*k, (*v).into())).collect();
        endpoint_os(args, &env)
    }

    fn endpoint_os(args: &[&str], env: &[(&str, OsString)]) -> Result<RunEndpoint, clap::Error> {
        Cli::try_parse_from(args)
            .expect("parses")
            .run_endpoint_with(|name| env.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()))
    }

    #[test]
    fn flags_win_over_the_environment() {
        let env = [("YUKI_REGION", "nl"), ("YUKI_BASE_URL", "http://env/ws")];
        let got = endpoint(
            &[
                "yuki",
                "--region",
                "be",
                "--base-url",
                "http://flag/ws",
                "init",
            ],
            &env,
        );
        assert_eq!(
            got.unwrap(),
            RunEndpoint {
                region: Some(Region::Be),
                base_url: Some("http://flag/ws".into())
            }
        );
        let got = endpoint(&["yuki", "init"], &env).unwrap();
        assert_eq!(got.region, Some(Region::Nl));
        assert_eq!(got.base_url.as_deref(), Some("http://env/ws"));
    }

    #[test]
    fn an_empty_base_url_is_unset_and_a_bad_region_is_a_usage_error() {
        let got = endpoint(&["yuki", "init"], &[("YUKI_BASE_URL", " ")]).unwrap();
        assert_eq!(got, RunEndpoint::default());
        for bad in ["", "BE", "de"] {
            let err = endpoint(&["yuki", "init"], &[("YUKI_REGION", bad)]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue, "{bad:?}");
        }
    }

    /// A non-UTF-8 value is a usage error (exit 2), as it was when clap read
    /// the variables, not silently treated as unset.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_endpoint_variable_is_a_usage_error() {
        use std::os::unix::ffi::OsStringExt;
        for name in ["YUKI_REGION", "YUKI_BASE_URL"] {
            let env = [(name, OsString::from_vec(vec![b'b', 0xff]))];
            let err = endpoint_os(&["yuki", "init"], &env).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::InvalidUtf8, "{name}");
            assert_eq!(err.exit_code(), 2, "{name}");
            assert!(err.to_string().contains(name), "{name}: {err}");
        }
    }
}
