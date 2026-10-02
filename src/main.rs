use std::fmt;
use std::process;

use clap::{CommandFactory, Parser};
use owo_colors::OwoColorize;
use yuki_cli::cli::Commands;
use yuki_cli::cli::{
    AccountCommands, AdminCommands, AuthCommands, CheckCommands, ConfigCommands, ContactCommands,
    DocumentCommands, InvoiceCommands, ProfileCommands, ProjectCommands, SalesCommands,
    SalesInvoiceCommands, UploadCommands, VatCommands,
};
use yuki_cli::cli::{Cli, RunEndpoint};
use yuki_cli::config::Config;
use yuki_cli::error::YukiError;
use yuki_cli::output::{ListOptions, format_error_json, is_tty};

enum AppError {
    Yuki(YukiError),
    Other(anyhow::Error),
    ConfirmationRequired(String),
    /// Yuki answered, but did not accept every invoice.
    InvoiceRejected(String),
    /// An invoice file or template that cannot be read or is invalid.
    InvalidInput(String),
    /// A write whose request went out without a usable answer.
    OutcomeUnknown(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yuki(e) => write!(f, "{e}"),
            Self::Other(e) => write!(f, "{e}"),
            Self::ConfirmationRequired(message)
            | Self::InvoiceRejected(message)
            | Self::InvalidInput(message)
            | Self::OutcomeUnknown(message) => {
                write!(f, "{message}")
            }
        }
    }
}

impl From<YukiError> for AppError {
    fn from(e: YukiError) -> Self {
        Self::Yuki(e)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl AppError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Yuki(e) => e.exit_code(),
            Self::Other(_) => 1,
            Self::ConfirmationRequired(_)
            | Self::InvoiceRejected(_)
            | Self::InvalidInput(_)
            | Self::OutcomeUnknown(_) => 1,
        }
    }

    fn kind(&self) -> &str {
        match self {
            Self::Yuki(e) => match e {
                YukiError::AuthFailed(_) | YukiError::Unauthorized(_) => "auth_failed",
                YukiError::NotFound(_) => "not_found",
                YukiError::RateLimited => "rate_limited",
                YukiError::Config(_) => "config_error",
                _ => "error",
            },
            Self::Other(_) => "error",
            Self::ConfirmationRequired(_) => "confirmation_required",
            Self::InvoiceRejected(_) => "invoice_rejected",
            Self::InvalidInput(_) => "invalid_input",
            Self::OutcomeUnknown(_) => "outcome_unknown",
        }
    }
}

/// A configuration error from reading an invoice is a problem with the input.
fn invalid_input(e: YukiError) -> AppError {
    match e {
        YukiError::Config(message) => AppError::InvalidInput(message),
        other => other.into(),
    }
}

/// Print a usage error the way clap does, plus the structured envelope, and exit.
fn exit_with_usage_error(e: clap::Error) -> ! {
    use clap::error::ErrorKind;
    // Help and version are informational exits, not errors.
    // Print them normally and exit without an error envelope.
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        process::exit(0);
    }
    // Print clap's formatted error message to stderr, then the structured envelope.
    eprintln!("{e}");
    eprintln!("{}", format_error_json(&e.to_string(), "error"));
    process::exit(e.exit_code());
}

#[tokio::main]
async fn main() {
    let cli = Cli::try_parse().unwrap_or_else(|e| exit_with_usage_error(e));
    let endpoint = cli
        .run_endpoint()
        .unwrap_or_else(|e| exit_with_usage_error(e));

    if let Err(err) = run(cli, endpoint).await {
        let code = err.exit_code();
        let kind = err.kind();
        // On TTY: print a human-friendly prefix first, then the structured error.
        if is_tty() {
            eprintln!("{} {err}", "error:".red().bold());
        }
        // Always emit the structured error envelope as the last line of stderr.
        eprintln!("{}", format_error_json(&err.to_string(), kind));
        process::exit(code.into());
    }
}

async fn run(cli: Cli, endpoint: RunEndpoint) -> Result<(), AppError> {
    let format = cli.output.as_deref();
    let RunEndpoint { region, base_url } = endpoint;
    // Only a typed --region is ever persisted; YUKI_REGION is for this run.
    let region_flag = cli.region;
    let load = || -> Result<Config, YukiError> {
        let mut config = Config::load()?;
        config.override_endpoint(region, base_url.as_deref());
        Ok(config)
    };

    match cli.command {
        Commands::Init {
            api_key,
            default_admin,
            add,
        } => {
            yuki_cli::cli::init::run(
                api_key.as_deref(),
                default_admin.as_deref().or(cli.admin.as_deref()),
                add,
                region,
                region_flag,
                base_url.as_deref(),
            )
            .await?;
        }

        Commands::Auth { command } => match command {
            AuthCommands::Login {
                api_key,
                default_admin,
                add,
            } => {
                yuki_cli::cli::init::run(
                    api_key.as_deref(),
                    default_admin.as_deref().or(cli.admin.as_deref()),
                    add,
                    region,
                    region_flag,
                    base_url.as_deref(),
                )
                .await?;
            }
            AuthCommands::Status { offline } => {
                let config = load()?;
                yuki_cli::cli::account::auth_status(
                    &config,
                    cli.admin.as_deref(),
                    offline,
                    format,
                    cli.quiet,
                )
                .await?;
            }
            AuthCommands::Logout => {
                let mut config = load()?;
                yuki_cli::cli::account::auth_logout(
                    &mut config,
                    cli.admin.as_deref(),
                    format,
                    cli.quiet,
                )?;
            }
        },

        Commands::Profile { command } => {
            let mut config = load()?;
            match command {
                ProfileCommands::List => {
                    yuki_cli::cli::account::profile_list(&config, format, cli.quiet);
                }
                ProfileCommands::Use { name } => {
                    yuki_cli::cli::account::profile_use(&mut config, &name, format, cli.quiet)?;
                }
                ProfileCommands::Remove { name } => {
                    if !cli.yes {
                        return Err(AppError::ConfirmationRequired(
                            "profile removal requires --yes".into(),
                        ));
                    }
                    yuki_cli::cli::account::profile_remove(&mut config, &name, format, cli.quiet)?;
                }
            }
        }

        Commands::Config { command } => match command {
            ConfigCommands::Show => {
                let config = load()?;
                yuki_cli::cli::account::config_show(&config, format, cli.quiet);
            }
            ConfigCommands::Path => {
                yuki_cli::cli::account::config_path(format, cli.quiet);
            }
        },

        Commands::Doctor { offline } => {
            let config = load()?;
            yuki_cli::cli::account::doctor(
                &config,
                cli.admin.as_deref(),
                offline,
                format,
                cli.quiet,
            )
            .await?;
        }

        Commands::Admin { command } => {
            let config = load()?;
            match command {
                AdminCommands::List {
                    local,
                    limit,
                    offset,
                    fields,
                } => {
                    yuki_cli::cli::admin::list(
                        &config,
                        local,
                        format,
                        ListOptions {
                            limit,
                            offset,
                            fields: fields.as_deref(),
                        },
                    )
                    .await?;
                }
                AdminCommands::Switch { name } => {
                    let mut config = config;
                    yuki_cli::cli::admin::switch(&mut config, &name)?;
                }
            }
        }

        Commands::Vat { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                VatCommands::Returns { year } => {
                    yuki_cli::cli::vat::returns(&config, admin, year.as_deref(), format).await?;
                }
                VatCommands::Codes => {
                    yuki_cli::cli::vat::codes(&config, admin, format).await?;
                }
            }
        }

        Commands::Contacts { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                ContactCommands::Search { query, by } => {
                    yuki_cli::cli::contacts::search(&config, admin, &query, &by, format).await?;
                }
                ContactCommands::List {
                    contact_type,
                    limit,
                    offset,
                    fields,
                } => {
                    yuki_cli::cli::contacts::list(
                        &config,
                        admin,
                        contact_type.as_deref(),
                        format,
                        ListOptions {
                            limit,
                            offset,
                            fields: fields.as_deref(),
                        },
                    )
                    .await?;
                }
            }
        }

        Commands::Accounts { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                AccountCommands::Balance { account, period } => {
                    yuki_cli::cli::accounts::balance(
                        &config,
                        admin,
                        account.as_deref(),
                        period.as_deref(),
                        format,
                    )
                    .await?;
                }
                AccountCommands::Transactions {
                    account,
                    period,
                    limit,
                    offset,
                    fields,
                } => {
                    yuki_cli::cli::accounts::transactions(
                        &config,
                        admin,
                        account.as_deref(),
                        period.as_deref(),
                        format,
                        ListOptions {
                            limit,
                            offset,
                            fields: fields.as_deref(),
                        },
                    )
                    .await?;
                }
                AccountCommands::Scheme => {
                    yuki_cli::cli::accounts::scheme(&config, admin, format).await?;
                }
                AccountCommands::Revenue { period } => {
                    yuki_cli::cli::accounts::revenue(&config, admin, period.as_deref(), format)
                        .await?;
                }
                AccountCommands::StartBalance { year } => {
                    yuki_cli::cli::accounts::start_balance(&config, admin, year.as_deref(), format)
                        .await?;
                }
            }
        }

        Commands::Projects { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                ProjectCommands::List => {
                    yuki_cli::cli::projects::list(&config, admin, format).await?;
                }
                ProjectCommands::Balance {
                    project,
                    account,
                    period,
                } => {
                    yuki_cli::cli::projects::balance(
                        &config,
                        admin,
                        &project,
                        account.as_deref(),
                        period.as_deref(),
                        format,
                    )
                    .await?;
                }
            }
        }

        Commands::Invoices { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                InvoiceCommands::List {
                    period,
                    invoice_type,
                    limit,
                    offset,
                    fields,
                } => {
                    yuki_cli::cli::invoices::list(
                        &config,
                        admin,
                        period.as_deref(),
                        invoice_type.as_deref(),
                        format,
                        ListOptions {
                            limit,
                            offset,
                            fields: fields.as_deref(),
                        },
                    )
                    .await?;
                }
                InvoiceCommands::Show {
                    id,
                    account,
                    period,
                } => {
                    yuki_cli::cli::invoices::show(
                        &config,
                        admin,
                        &id,
                        &account,
                        period.as_deref(),
                        format,
                    )
                    .await?;
                }
                InvoiceCommands::Document { id, out } => {
                    yuki_cli::cli::invoices::document(
                        &config,
                        admin,
                        &id,
                        out.as_deref(),
                        format,
                        cli.quiet,
                    )
                    .await?;
                }
            }
        }

        Commands::Sales { command } => {
            let admin = cli.admin.as_deref();
            match command {
                SalesCommands::Items {
                    limit,
                    offset,
                    fields,
                } => {
                    let config = load()?;
                    yuki_cli::cli::sales::items(
                        &config,
                        admin,
                        format,
                        ListOptions {
                            limit,
                            offset,
                            fields: fields.as_deref(),
                        },
                    )
                    .await?;
                }
                SalesCommands::Invoice { command } => match command {
                    SalesInvoiceCommands::Create {
                        inputs,
                        prepared,
                        pdf,
                        send,
                        book,
                        dry_run,
                    } => {
                        use yuki_cli::cli::invoice_ledger::InvoiceLedger;
                        use yuki_cli::cli::invoice_number::{self, NumberRequest};
                        use yuki_cli::cli::sales_invoice::{self, Invoice, SendMode};
                        let send = if book { Some(SendMode::Book) } else { send };
                        if pdf.is_some() && prepared.is_none() {
                            return Err(AppError::InvalidInput(
                                "--pdf needs --prepared: prepare the invoice with `sales invoice prepare --out <file.json>`, render the PDF from that file, then `create --prepared <file.json> --pdf <pdf>`".into(),
                            ));
                        }
                        // A prepared invoice: exactly that content, its number
                        // still reserved for it in this administration.
                        let mut binding: Option<String> = None;
                        let mut invoice = match &prepared {
                            Some(file) => {
                                let send = send.ok_or_else(|| {
                                    AppError::InvalidInput(
                                        "a prepared invoice is booked: add --send email|peppol|both, or --book".into(),
                                    )
                                })?;
                                let path = std::path::Path::new(file);
                                let (json, hash) =
                                    sales_invoice::read_prepared(path).map_err(invalid_input)?;
                                let (mut invoice, admin_id) =
                                    Invoice::from_prepared(&json, file, send)
                                        .map_err(invalid_input)?;
                                if let Some(pdf) = &pdf {
                                    invoice
                                        .attach_pdf(std::path::Path::new(pdf))
                                        .map_err(invalid_input)?;
                                }
                                let config = load()?;
                                if config.seller.is_none() {
                                    return Err(invalid_input(sales_invoice::seller_missing()));
                                }
                                let target = config.target(admin)?;
                                if target.admin_id != admin_id {
                                    return Err(AppError::InvalidInput(format!(
                                        "{file} was prepared for administration {admin_id}, not {} ({}): select it with --admin",
                                        target.config_name, target.admin_id
                                    )));
                                }
                                let number = invoice.number.as_deref().unwrap_or_default();
                                InvoiceLedger::peek()?
                                    .check_reserved(&admin_id, number, &hash)
                                    .map_err(invalid_input)?;
                                binding = Some(hash);
                                invoice
                            }
                            None => {
                                sales_invoice::load(&inputs.source(), &inputs.overrides(), send)
                                    .map_err(invalid_input)?
                            }
                        };
                        // A dry run makes no API call (and, unprepared, needs no
                        // configuration).
                        if dry_run {
                            if inputs.number == Some(NumberRequest::Auto) {
                                return Err(AppError::InvalidInput(
                                    "--number auto reads the sales archive, which a dry run does not: run `sales invoice prepare --number auto` for the number, then pass it".into(),
                                ));
                            }
                            if !cli.quiet {
                                eprintln!("{}\n", invoice.preview(None));
                                if let (Some(number), None) = (&invoice.number, &binding) {
                                    eprintln!(
                                        "Dry run: number {number} was not checked against the sales archive."
                                    );
                                }
                                eprintln!("Dry run: nothing was sent to Yuki. xmlDoc:");
                            }
                            println!("{}", invoice.to_display_xml());
                            return Ok(());
                        }
                        let config = load()?;
                        let target = config.target(admin)?;
                        if let Some(request) = &inputs.number {
                            invoice.number = Some(
                                invoice_number::resolve(&config, admin, request, &invoice.date)
                                    .await
                                    .map_err(invalid_input)?,
                            );
                        }
                        if !(cli.quiet && cli.yes) {
                            eprintln!("{}\n", invoice.preview(Some(target.config_name)));
                        } else if let Some(line) = invoice.booking_line() {
                            // Even quiet, a booking is announced.
                            eprintln!("{line}");
                        }
                        if !cli.yes {
                            if !sales_invoice::can_prompt() {
                                return Err(AppError::ConfirmationRequired(
                                    "sales invoice create writes to Yuki; pass --yes to confirm in non-interactive mode, or --dry-run to preview only".into(),
                                ));
                            }
                            if !sales_invoice::confirm(&invoice.question())? {
                                return Err(AppError::ConfirmationRequired(
                                    "not confirmed: nothing was sent to Yuki".into(),
                                ));
                            }
                        }
                        let import = sales_invoice::submit_numbered(
                            &config,
                            admin,
                            &invoice,
                            binding.as_deref(),
                            format,
                            cli.quiet,
                        )
                        .await
                        .map_err(|e| match e {
                            sales_invoice::SubmitError::Yuki(e) => AppError::from(e),
                            sales_invoice::SubmitError::OutcomeUnknown(message) => {
                                AppError::OutcomeUnknown(message)
                            }
                        })?;
                        if let Some(failure) = import
                            .failure()
                            .or_else(|| sales_invoice::unsent(&import, invoice.send))
                        {
                            return Err(AppError::InvoiceRejected(failure));
                        }
                    }
                    SalesInvoiceCommands::Prepare { inputs, out } => {
                        use yuki_cli::cli::{invoice_number, sales_invoice};
                        let overrides = sales_invoice::Overrides {
                            preparing: true,
                            ..inputs.overrides()
                        };
                        let mut invoice = sales_invoice::load(&inputs.source(), &overrides, None)
                            .map_err(invalid_input)?;
                        // The config, when there is one, gives the issuing firm.
                        let config = match &inputs.number {
                            Some(_) => Some(load()?),
                            None => load().ok(),
                        };
                        let seller = config.as_ref().and_then(|c| c.seller.as_ref());
                        if out.is_some() && seller.is_none() {
                            return Err(invalid_input(sales_invoice::seller_missing()));
                        }
                        if let (Some(request), Some(config)) = (&inputs.number, &config) {
                            config.target(admin)?;
                            invoice.number = Some(
                                invoice_number::resolve(config, admin, request, &invoice.date)
                                    .await
                                    .map_err(invalid_input)?,
                            );
                        }
                        let json = match (&out, &config, seller) {
                            (Some(out), Some(config), Some(seller)) => {
                                let admin_id = config.target(admin)?.admin_id;
                                let json = sales_invoice::write_prepared(
                                    &invoice,
                                    seller,
                                    admin_id,
                                    std::path::Path::new(out),
                                )
                                .map_err(invalid_input)?;
                                if !cli.quiet {
                                    let number = invoice.number.as_deref().unwrap_or_default();
                                    eprintln!(
                                        "Reserved invoice number {number} for this content in {out}. Render the PDF from it, then: yuki sales invoice create --prepared {out} --pdf <pdf> --send email (free the number instead with: yuki sales invoice numbers --release {number})"
                                    );
                                }
                                json
                            }
                            _ => invoice.prepared_for(seller),
                        };
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&json).expect("serialize invoice")
                        );
                    }
                    SalesInvoiceCommands::Numbers { resolve, release } => {
                        use yuki_cli::cli::invoice_ledger::{self, Settle, Status};
                        let resolve = match resolve.as_deref() {
                            Some([number, status]) => {
                                let status = match status.to_ascii_lowercase().as_str() {
                                    "booked" => Status::Booked,
                                    "rejected" => Status::Rejected,
                                    other => {
                                        return Err(AppError::InvalidInput(format!(
                                            "--resolve takes booked or rejected, not {other}"
                                        )));
                                    }
                                };
                                Some((number.clone(), status))
                            }
                            _ => None,
                        };
                        let settle = match (&resolve, &release) {
                            (Some((number, status)), _) => Some(Settle::Resolve(number, *status)),
                            (None, Some(number)) => Some(Settle::Release(number)),
                            (None, None) => None,
                        };
                        let config = load()?;
                        invoice_ledger::numbers(config.target(admin)?.admin_id, settle, format)?;
                    }
                    SalesInvoiceCommands::Templates => {
                        yuki_cli::cli::sales_invoice::templates(format)?;
                    }
                },
            }
        }

        Commands::Documents { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                DocumentCommands::List {
                    folder,
                    doc_type,
                    limit,
                    offset,
                    fields,
                } => {
                    yuki_cli::cli::documents::list(
                        &config,
                        admin,
                        folder.as_deref(),
                        doc_type.as_deref(),
                        format,
                        ListOptions {
                            limit,
                            offset,
                            fields: fields.as_deref(),
                        },
                    )
                    .await?;
                }
                DocumentCommands::Search { query } => {
                    yuki_cli::cli::documents::search(&config, admin, &query, format).await?;
                }
                DocumentCommands::Download { id, out } => {
                    yuki_cli::cli::documents::download(
                        &config,
                        admin,
                        &id,
                        out.as_deref(),
                        format,
                        cli.quiet,
                    )
                    .await?;
                }
                DocumentCommands::Exists {
                    amount,
                    date,
                    contact,
                } => {
                    yuki_cli::cli::documents::exists(
                        &config,
                        admin,
                        amount,
                        &date,
                        contact.as_deref(),
                        format,
                    )
                    .await?;
                }
            }
        }

        Commands::Check { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
                CheckCommands::Btw { period } => {
                    yuki_cli::cli::check::btw(&config, admin, period.as_deref(), format, cli.quiet)
                        .await?;
                }
                CheckCommands::Unmatched {
                    period,
                    bank_account,
                } => {
                    yuki_cli::cli::check::unmatched(
                        &config,
                        admin,
                        period.as_deref(),
                        &bank_account,
                        format,
                        cli.quiet,
                    )
                    .await?;
                }
                CheckCommands::Matches {
                    period,
                    bank_account,
                    unallocated,
                } => {
                    yuki_cli::cli::check::matches(
                        &config,
                        admin,
                        period.as_deref(),
                        &bank_account,
                        unallocated,
                        format,
                        cli.quiet,
                    )
                    .await?;
                }
                CheckCommands::Outstanding { reference } => {
                    yuki_cli::cli::check::outstanding(&config, admin, &reference, format).await?;
                }
            }
        }

        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                env!("CARGO_PKG_NAME"),
                &mut std::io::stdout(),
            );
        }

        Commands::Schema => {
            yuki_cli::schema::print_schema();
        }

        Commands::Capabilities => {
            let value = serde_json::json!({
                "areas": ["administrations", "vat", "contacts", "accounts", "projects", "invoices", "sales", "documents", "checks", "uploads"],
                "structured_output": true,
                "daily_api_limit": 1000
            });
            if matches!(
                yuki_cli::output::OutputFormat::from_flag(format, is_tty()),
                yuki_cli::output::OutputFormat::Json
            ) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).expect("serialize capabilities")
                );
            } else {
                println!(
                    "API areas: administrations, VAT, contacts, accounts, projects, invoices, sales, documents, checks, uploads\nDaily API limit: 1000"
                );
            }
        }

        Commands::Upload { command } => {
            let admin = cli.admin.as_deref();
            match command {
                UploadCommands::Dir {
                    path,
                    folder,
                    exclude,
                    max,
                    dry_run,
                    seed_from_yuki,
                    seed_folder,
                } => {
                    use yuki_cli::cli::upload_dir::{Confirm, DirOptions, Outcome};
                    let confirm = if cli.yes {
                        Confirm::Yes
                    } else if yuki_cli::cli::interactive() {
                        Confirm::Prompt
                    } else {
                        Confirm::Refuse
                    };
                    let options = DirOptions {
                        path: &path,
                        folder: &folder,
                        excludes: &exclude,
                        max,
                        dry_run,
                        seed: seed_from_yuki,
                        seed_folders: &seed_folder,
                    };
                    match yuki_cli::cli::upload_dir::dir(
                        load, admin, options, confirm, format, cli.quiet,
                    )
                    .await?
                    {
                        Outcome::Done => {}
                        Outcome::NeedsConfirmation(message) => {
                            return Err(AppError::ConfirmationRequired(message));
                        }
                        Outcome::NeedsAttention(message) => {
                            return Err(AppError::Other(anyhow::anyhow!(message)));
                        }
                    }
                }
                UploadCommands::Mark {
                    file,
                    doc_id,
                    skip,
                    forget,
                    folder,
                    note,
                    dir,
                    force,
                } => {
                    use yuki_cli::cli::upload_dir::{Mark, MarkOptions};
                    let mark = match (doc_id.as_deref(), skip, forget) {
                        (Some(id), _, _) => Mark::Document(id),
                        (None, true, _) => Mark::Skip,
                        _ => Mark::Forget,
                    };
                    yuki_cli::cli::upload_dir::mark(
                        MarkOptions {
                            file: &file,
                            mark,
                            folder: folder.as_deref(),
                            note: note.as_deref(),
                            dir: dir.as_deref(),
                            force,
                        },
                        format,
                        cli.quiet,
                    )?;
                }
                UploadCommands::File {
                    file,
                    folder,
                    amount,
                    category,
                    payment_method,
                    project,
                    remarks,
                    currency,
                } => {
                    // Require explicit confirmation for non-interactive uploads.
                    if !yuki_cli::cli::interactive() && !cli.yes {
                        return Err(AppError::ConfirmationRequired(
                            "upload file is a mutating operation; pass --yes to confirm in non-interactive mode".into(),
                        ));
                    }
                    let config = load()?;
                    let options = yuki_cli::cli::upload::UploadOptions {
                        folder: &folder,
                        amount,
                        category: category.as_deref(),
                        payment_method: payment_method.as_deref(),
                        project: project.as_deref(),
                        remarks: remarks.as_deref(),
                        currency: &currency,
                    };
                    yuki_cli::cli::upload::run(&config, admin, &file, options, format, cli.quiet)
                        .await?;
                }
                UploadCommands::Categories => {
                    let config = load()?;
                    yuki_cli::cli::upload::categories(&config, admin, format).await?;
                }
                UploadCommands::PaymentMethods => {
                    let config = load()?;
                    yuki_cli::cli::upload::payment_methods(&config, admin, format).await?;
                }
            }
        }
    }

    Ok(())
}
