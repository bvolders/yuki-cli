pub mod account;
pub mod accounts;
pub mod admin;
pub mod check;
pub mod contacts;
pub mod documents;
pub mod init;
pub mod invoice_ledger;
pub mod invoice_number;
pub mod invoices;
pub mod projects;
pub mod sales;
pub mod sales_invoice;
pub mod upload;
pub mod upload_dir;
pub mod vat;

use std::ffi::OsString;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{ArgGroup, CommandFactory, Parser, Subcommand};

use crate::client::Region;
use crate::client::accounting::AccountingClient;
use crate::config::{Config, Target};
use crate::error::YukiError;

/// Whether a confirmation can be asked: stdin is a terminal to answer on.
/// Without one, a mutating command needs `--yes`.
pub fn interactive() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}

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

    /// Save the document linked to a transaction.
    ///
    /// Written under the document's own file name in the working directory,
    /// or to --out (a file, or a directory to put it in); never over an
    /// existing file.
    Document {
        /// Transaction ID.
        id: String,

        /// File or directory to write to.
        #[arg(long, value_name = "PATH")]
        out: Option<String>,
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

    /// Create sales invoices from a file or a saved template.
    Invoice {
        #[command(subcommand)]
        command: SalesInvoiceCommands,
    },
}

#[derive(Subcommand)]
pub enum SalesInvoiceCommands {
    /// Create a sales invoice in Yuki: a draft, unless --send or --book books it.
    ///
    /// A prepared invoice (--prepared, written by `prepare --out`) is booked
    /// exactly as prepared, under the number reserved for it. A TOML file
    /// (--file) or a saved template (--template, read from
    /// ~/.config/yuki/invoices/<name>.toml) becomes a draft, or a booking
    /// Yuki numbers itself, confirmed at the prompt. A preview of the
    /// customer, lines and totals is printed first, then confirmed on a
    /// terminal; --yes skips the prompt and is required when not on a
    /// terminal. --dry-run prints the preview and the exact xmlDoc without
    /// contacting Yuki. Booking is immediate: there is no draft to review.
    #[command(group(
        clap::ArgGroup::new("source").required(true).args(["file", "template", "prepared"])
    ))]
    Create {
        #[command(flatten)]
        inputs: InvoiceInputs,

        /// A prepared invoice (`prepare --out`) to book exactly: its number
        /// must still be reserved for this content. Takes no other invoice
        /// inputs; needs --send or --book.
        #[arg(long, value_name = "FILE", conflicts_with_all = ["qty", "price", "date", "subject"])]
        prepared: Option<String>,

        /// Custom invoice PDF (max 3 MB) rendered from the --prepared file.
        /// Yuki stores it, as `Invoice <number>.pdf`, instead of the invoice it
        /// would generate; the lines still set the booked amounts.
        #[arg(long, value_name = "PATH")]
        pdf: Option<String>,

        /// Book the invoice and send it: email, peppol, or both. Without it
        /// (or --book), the invoice is created as a draft in "To be sent".
        #[arg(long, value_enum)]
        send: Option<sales_invoice::SendMode>,

        /// Book the invoice without sending it, for invoices you send yourself.
        #[arg(long, conflicts_with = "send")]
        book: bool,

        /// Print the preview and the xmlDoc XML; make no API call at all.
        #[arg(long)]
        dry_run: bool,

        /// The invoice number being booked, repeated: required with --yes
        /// when booking (--send or --book), so an unattended run books only
        /// the number it meant to.
        #[arg(long, value_name = "NUMBER")]
        confirm: Option<String>,
    },

    /// Print the fully resolved invoice as JSON, for rendering its PDF.
    ///
    /// The totals per VAT rate are the CLI's computation; Yuki books its own.
    /// Dates come as ISO and Dutch text, with the Belgian structured payment
    /// reference and the issuing firm ([seller] in the config). With --number
    /// it reads the sales archive and the local number ledger. With --out it
    /// also writes the invoice to a file and reserves its number for exactly
    /// that content; `create --prepared <file>` books it.
    #[command(group(
        clap::ArgGroup::new("source").required(true).args(["file", "template"])
    ))]
    Prepare {
        #[command(flatten)]
        inputs: InvoiceInputs,

        /// Invoice number (Yuki's Reference), or `auto`: the lowest
        /// <year>-<seq> of the invoice date's year above the sales archive's
        /// highest that the local ledger does not hold. Refused when either
        /// has it. Yuki's own counter does not learn numbers given here, so
        /// once you start, number every invoice this way.
        #[arg(long, value_name = "REF|auto", value_parser = invoice_number::parse_number_request)]
        number: Option<invoice_number::NumberRequest>,

        /// Write the prepared invoice here (never over an existing file) and
        /// reserve its number in the ledger. Needs --number and [seller].
        #[arg(long, value_name = "FILE", requires = "number")]
        out: Option<String>,
    },

    /// List the invoice numbers given out, from the local ledger.
    ///
    /// `prepare --out` reserves a number; it is pending from just before Yuki
    /// is called until its answer marks it booked or rejected. One left
    /// pending (no answer came back) stays taken: check "To be sent"/Sales in
    /// Yuki, then settle it with --resolve <NUMBER> --as booked|rejected. A
    /// reservation that will not be sent is freed with --resolve <NUMBER>
    /// --as rejected. A rejected number is given out again.
    Numbers {
        /// Settle this number by hand, as --as says.
        #[arg(long, value_name = "NUMBER", requires = "resolution")]
        resolve: Option<String>,

        /// What --resolve settles the number as: booked (pending only) or
        /// rejected (pending or reserved).
        #[arg(long = "as", id = "resolution", value_enum, requires = "resolve")]
        resolution: Option<invoice_ledger::Resolution>,
    },

    /// List saved invoice templates (~/.config/yuki/invoices/*.toml).
    Templates,
}

/// What an invoice is made of: `create` and `prepare` take the same.
#[derive(clap::Args)]
pub struct InvoiceInputs {
    /// Invoice described in a TOML file.
    #[arg(long, value_name = "PATH")]
    pub file: Option<String>,

    /// Saved template name (see `sales invoice templates`).
    #[arg(long, value_name = "NAME")]
    pub template: Option<String>,

    /// Quantity of the invoice's only line, e.g. 7.5.
    #[arg(long, value_parser = sales_invoice::parse_quantity)]
    pub qty: Option<sales_invoice::Quantity>,

    /// Unit price excluding VAT of the invoice's only line, e.g. 1250.00.
    #[arg(long, value_parser = sales_invoice::parse_price)]
    pub price: Option<crate::money::Cents>,

    /// Invoice date, YYYY-MM-DD. Default: the file's date, else today.
    #[arg(long, value_parser = sales_invoice::parse_date)]
    pub date: Option<String>,

    /// Subject (title) of the invoice, replacing the file's.
    #[arg(long)]
    pub subject: Option<String>,
}

impl InvoiceInputs {
    /// The invoice file or template; `None` when neither is given.
    pub fn source(&self) -> Option<sales_invoice::Source> {
        match (&self.file, &self.template) {
            (Some(file), _) => Some(sales_invoice::Source::File(file.into())),
            (None, Some(name)) => Some(sales_invoice::Source::Template(name.clone())),
            (None, None) => None,
        }
    }

    pub fn overrides(&self) -> sales_invoice::Overrides<'_> {
        sales_invoice::Overrides {
            qty: self.qty,
            price: self.price,
            date: self.date.as_deref(),
            subject: self.subject.as_deref(),
        }
    }
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

    /// Save an archive document's file.
    ///
    /// Written under the document's own file name in the working directory,
    /// or to --out (a file, or a directory to put it in); never over an
    /// existing file.
    Download {
        /// Document ID (as shown by `documents list`).
        id: String,

        /// File or directory to write to.
        #[arg(long, value_name = "PATH")]
        out: Option<String>,
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
    /// Search contacts, active or not, by any field or by one (--by).
    Search {
        /// Search value.
        query: String,

        /// Field to search: All (default) or one of Yuki's search options,
        /// e.g. Name, City, VATNumber, Code, HID. Case-insensitive.
        #[arg(
            long,
            default_value = "All",
            ignore_case = true,
            value_parser = PossibleValuesParser::new(crate::client::contact::SEARCH_OPTIONS),
        )]
        by: String,
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

    /// Upload the receipts in a directory that are not in Yuki yet, once each.
    ///
    /// State is kept in PATH/.yuki-sync.json, keyed by content hash. Prints the
    /// plan and asks first (--yes without a terminal). An upload whose outcome
    /// is uncertain stays pending and is never retried: resolve it with
    /// `upload mark`. Exits 1 when any file needs attention. See the README.
    Dir {
        /// Directory to upload from.
        path: String,

        /// Target folder: uitzoeken (default), inkoop, verkoop, bank, personeel, belasting, overig-financieel.
        #[arg(long, default_value = "uitzoeken")]
        folder: String,

        /// Skip paths matching this case-insensitive glob; repeatable. A name
        /// without / matches any path component. _to_delete and .* always apply.
        #[arg(long = "exclude")]
        exclude: Vec<String>,

        /// Upload at most this many files in this run.
        #[arg(long, default_value_t = 25)]
        max: usize,

        /// Print the plan only: no API calls, nothing written.
        #[arg(long)]
        dry_run: bool,

        /// Upload nothing; record the files whose file name matches exactly one
        /// Yuki document, claimed by no other file, as already-in-yuki, and
        /// list ambiguous and near matches for review. Asks before writing.
        #[arg(long)]
        seed_from_yuki: bool,

        /// Yuki folder to look in when seeding; repeatable. Defaults to
        /// --folder and inkoop.
        #[arg(long = "seed-folder", requires = "seed_from_yuki")]
        seed_folder: Vec<String>,
    },

    /// Record by hand that a file is in Yuki, should be skipped, or is to be forgotten.
    ///
    /// Updates the .yuki-sync.json of the synced directory without contacting
    /// Yuki: --dir, else the nearest directory above FILE that has one.
    /// Resolves files `upload dir` reports as pending or changed: --doc-id when
    /// Yuki has the file, --forget to upload it (again), --skip to keep it out.
    #[command(group(ArgGroup::new("record").required(true).args(["doc_id", "skip", "forget"])))]
    Mark {
        /// The file to record.
        file: String,

        /// The Yuki document ID the file was uploaded as.
        #[arg(long)]
        doc_id: Option<String>,

        /// Never upload this file.
        #[arg(long)]
        skip: bool,

        /// Remove the record, so the next run treats the file as new. For a
        /// changed file, removes the record of the earlier content at its path.
        #[arg(long)]
        forget: bool,

        /// The Yuki folder the document is in.
        #[arg(long)]
        folder: Option<String>,

        /// Note to keep with the record.
        #[arg(long)]
        note: Option<String>,

        /// The synced directory (its root), needed when it has no
        /// .yuki-sync.json yet.
        #[arg(long)]
        dir: Option<String>,

        /// Replace an existing record, or record a document ID already
        /// recorded for another file.
        #[arg(long)]
        force: bool,
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
