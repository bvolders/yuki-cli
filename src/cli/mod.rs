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

use clap::{Parser, Subcommand};

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
    let mut client = AccountingClient::new();
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

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize yuki configuration for this machine.
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
    /// Show the balance of a general ledger account for a period.
    Balance {
        /// GL account code.
        #[arg(long)]
        account: Option<String>,

        /// Accounting period (e.g. 2025-01).
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
        /// GL account code for the bank account (default: 11001).
        #[arg(long, default_value = "11001")]
        bank_account: String,
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
