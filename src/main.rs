use std::fmt;
use std::process;

use clap::CommandFactory;
use owo_colors::OwoColorize;
use yuki_cli::cli::Cli;
use yuki_cli::cli::Commands;
use yuki_cli::cli::{
    AccountCommands, AdminCommands, AuthCommands, CheckCommands, ConfigCommands, ContactCommands,
    DocumentCommands, InvoiceCommands, ProfileCommands, ProjectCommands, UploadCommands,
    VatCommands,
};
use yuki_cli::config::Config;
use yuki_cli::error::YukiError;
use yuki_cli::output::{ListOptions, format_error_json, is_tty};

enum AppError {
    Yuki(YukiError),
    Other(anyhow::Error),
    ConfirmationRequired(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yuki(e) => write!(f, "{e}"),
            Self::Other(e) => write!(f, "{e}"),
            Self::ConfirmationRequired(message) => write!(f, "{message}"),
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
            Self::ConfirmationRequired(_) => 1,
        }
    }

    fn kind(&self) -> &str {
        match self {
            Self::Yuki(e) => match e {
                YukiError::AuthFailed(_) => "auth_failed",
                YukiError::NotFound(_) => "not_found",
                YukiError::RateLimited => "rate_limited",
                YukiError::Config(_) => "config_error",
                _ => "error",
            },
            Self::Other(_) => "error",
            Self::ConfirmationRequired(_) => "confirmation_required",
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse_tracked() {
        Ok(cli) => cli,
        Err(e) => {
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
    };

    if let Err(err) = run(cli).await {
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

async fn run(cli: Cli) -> Result<(), AppError> {
    let format = cli.output.as_deref();
    // An empty YUKI_BASE_URL means unset, not a root of "".
    let base_url = cli.base_url.clone().filter(|u| !u.trim().is_empty());
    let region = cli.region;
    let region_flag = cli.region_flag();
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
                ContactCommands::Search { query } => {
                    yuki_cli::cli::contacts::search(&config, admin, &query, format).await?;
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
                InvoiceCommands::Show { id } => {
                    yuki_cli::cli::invoices::show(&config, admin, &id, format).await?;
                }
                InvoiceCommands::Document { id } => {
                    yuki_cli::cli::invoices::document(&config, admin, &id, format).await?;
                }
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
                "areas": ["administrations", "vat", "contacts", "accounts", "projects", "invoices", "documents", "checks", "uploads"],
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
                    "API areas: administrations, VAT, contacts, accounts, projects, invoices, documents, checks, uploads\nDaily API limit: 1000"
                );
            }
        }

        Commands::Upload { command } => {
            let config = load()?;
            let admin = cli.admin.as_deref();
            match command {
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
                    if !is_tty() && !cli.yes {
                        return Err(AppError::ConfirmationRequired(
                            "upload file is a mutating operation; pass --yes to confirm in non-interactive mode".into(),
                        ));
                    }
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
                    yuki_cli::cli::upload::categories(&config, admin, format).await?;
                }
                UploadCommands::PaymentMethods => {
                    yuki_cli::cli::upload::payment_methods(&config, admin, format).await?;
                }
            }
        }
    }

    Ok(())
}
