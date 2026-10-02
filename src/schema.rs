use serde_json::{Value, json};

use crate::client::Region;

pub fn generate() -> Value {
    let mut schema = json!({
        "clispec": "0.3",
        "name": "yuki",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "CLI client for the Yuki bookkeeping API",
        "global_args": [
            {
                "name": "--output",
                "type": "string",
                "description": "Output format: auto, text, or json.",
                "enum": ["auto", "text", "json"],
                "default": "auto"
            },
            {
                "name": "--profile",
                "type": "string",
                "description": "Override the active administration profile by name (alias: --admin)."
            },
            {
                "name": "--quiet",
                "type": "boolean",
                "description": "Suppress all output except errors.",
                "default": false
            },
            {
                "name": "--yes",
                "type": "boolean",
                "description": "Skip confirmation prompts (for use in scripts and pipelines).",
                "default": false
            },
            {
                "name": "--region",
                "type": "string",
                "description": "Yuki deployment (env: YUKI_REGION). init detects it from the key when omitted and stores it; otherwise it overrides the configured region for this run, and init stores it only when passed as a flag.",
                "enum": Region::codes()
            },
            {
                "name": "--base-url",
                "type": "url",
                "description": "Full API root, e.g. https://api.yukiworks.be/ws (env: YUKI_BASE_URL). Overrides --region."
            }
        ],
        "commands": [
            {
                "name": "admin list",
                "description": "List administrations, reconciling the configuration against every configured access key. Every configured administration is reported, so one no key reaches is visible rather than absent.",
                "mutating": false,
                "args": [
                    {"name": "--local", "type": "boolean", "required": false, "description": "Report what is configured without contacting the API."},
                    {"name": "--limit", "type": "integer", "required": false, "description": "Maximum number of results to return."},
                    {"name": "--offset", "type": "integer", "required": false, "description": "Number of results to skip (for pagination)."},
                    {"name": "--fields", "type": "string", "required": false, "description": "Comma-separated list of fields to include in output."}
                ],
                "output_fields": [
                    {"name": "name", "type": "string", "description": "Display name as Yuki reports it, or - when unknown."},
                    {"name": "config", "type": "string", "description": "Configuration name, i.e. what --admin accepts."},
                    {"name": "domain_id", "type": "string"},
                    {"name": "admin_id", "type": "string"},
                    {"name": "default", "type": "string", "description": "Yes for the administration used when --admin is omitted, otherwise No."},
                    {"name": "status", "type": "string", "description": "ok, auth failed, unreachable, not configured, or not checked with --local."}
                ]
            },
            {
                "name": "admin switch",
                "description": "Switch the active administration.",
                "mutating": true,
                "args": [
                    {"name": "name", "type": "string", "required": true, "description": "Name of the administration to activate."}
                ]
            },
            {
                "name": "vat returns",
                "description": "List VAT returns for a given year.",
                "mutating": false,
                "args": [
                    {"name": "year", "type": "string", "required": false, "description": "Fiscal year (e.g. 2025)."}
                ],
                "output_fields": [
                    {"name": "period", "type": "string"},
                    {"name": "status", "type": "string"},
                    {"name": "amount", "type": "string"}
                ]
            },
            {
                "name": "vat codes",
                "description": "List active VAT codes.",
                "mutating": false,
                "args": [],
                "output_fields": [
                    {"name": "code", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "percentage", "type": "string"}
                ]
            },
            {
                "name": "contacts search",
                "description": "Search contacts, active or not, by any field (default) or by the one --by names.",
                "mutating": false,
                "args": [
                    {"name": "query", "type": "string", "required": true, "description": "Search value."},
                    {"name": "--by", "type": "string", "required": false, "enum": crate::client::contact::SEARCH_OPTIONS, "default": "All", "description": "Field to search (case-insensitive)."}
                ],
                "output_fields": [
                    {"name": "ID", "type": "string"},
                    {"name": "HID", "type": "string"},
                    {"name": "Code", "type": "string"},
                    {"name": "Name", "type": "string"},
                    {"name": "Type", "type": "string"},
                    {"name": "City", "type": "string"},
                    {"name": "Country", "type": "string"},
                    {"name": "VAT Number", "type": "string"},
                    {"name": "Supplier", "type": "string"},
                    {"name": "Customer", "type": "string"}
                ]
            },
            {
                "name": "contacts list",
                "description": "List contacts filtered by type.",
                "mutating": false,
                "args": [
                    {"name": "--contact-type", "type": "string", "required": false, "description": "Contact type (e.g. customer, supplier)."},
                    {"name": "--limit", "type": "integer", "required": false, "description": "Maximum number of results to return."},
                    {"name": "--offset", "type": "integer", "required": false, "description": "Number of results to skip (for pagination)."},
                    {"name": "--fields", "type": "string", "required": false, "description": "Comma-separated list of fields to include in output."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "name", "type": "string"},
                    {"name": "type", "type": "string"},
                    {"name": "email", "type": "string"}
                ]
            },
            {
                "name": "accounts balance",
                "description": "Show GL account balances at the end of a period (or today, if it is still running).",
                "mutating": false,
                "args": [
                    {"name": "--account", "type": "string", "required": false, "description": "GL account code."},
                    {"name": "--period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025-01); the balance is taken on its last day, clamped to today."}
                ],
                "output_fields": [
                    {"name": "account", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "balance", "type": "string"},
                    {"name": "as_of", "type": "string"}
                ]
            },
            {
                "name": "accounts transactions",
                "description": "List transactions for a general ledger account.",
                "mutating": false,
                "args": [
                    {"name": "--account", "type": "string", "required": false, "description": "GL account code."},
                    {"name": "--period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025-01)."},
                    {"name": "--limit", "type": "integer", "required": false, "description": "Maximum number of results to return."},
                    {"name": "--offset", "type": "integer", "required": false, "description": "Number of results to skip (for pagination)."},
                    {"name": "--fields", "type": "string", "required": false, "description": "Comma-separated list of fields to include in output."}
                ],
                "output_fields": [
                    {"name": "date", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "amount", "type": "string"},
                    {"name": "reference", "type": "string"}
                ]
            },
            {
                "name": "accounts scheme",
                "description": "Show the chart of accounts (GL account scheme).",
                "mutating": false,
                "args": [],
                "output_fields": [
                    {"name": "code", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "type", "type": "string"}
                ]
            },
            {
                "name": "accounts revenue",
                "description": "Show net revenue for a period.",
                "mutating": false,
                "args": [
                    {"name": "--period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025, 2025-Q1, 2025-01)."}
                ],
                "output_fields": [
                    {"name": "period", "type": "string"},
                    {"name": "revenue", "type": "string"}
                ]
            },
            {
                "name": "accounts start-balance",
                "description": "Show opening balances per GL account for a book year.",
                "mutating": false,
                "args": [
                    {"name": "--year", "type": "string", "required": false, "description": "Book year (e.g. 2025)."}
                ],
                "output_fields": [
                    {"name": "account", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "balance", "type": "string"}
                ]
            },
            {
                "name": "projects list",
                "description": "List all projects.",
                "mutating": false,
                "args": [],
                "output_fields": [
                    {"name": "code", "type": "string"},
                    {"name": "name", "type": "string"},
                    {"name": "status", "type": "string"}
                ]
            },
            {
                "name": "projects balance",
                "description": "Show balance for a project.",
                "mutating": false,
                "args": [
                    {"name": "project", "type": "string", "required": true, "description": "Project code."},
                    {"name": "--account", "type": "string", "required": false, "description": "GL account code filter."},
                    {"name": "--period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025, 2025-Q1)."}
                ],
                "output_fields": [
                    {"name": "account", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "balance", "type": "string"}
                ]
            },
            {
                "name": "invoices list",
                "description": "List outstanding (open) invoices, optionally filtered by period and type.",
                "mutating": false,
                "args": [
                    {"name": "--period", "type": "string", "required": false, "description": "Only items dated within this period (e.g. 2025, 2025-Q1, 2025-01)."},
                    {"name": "--invoice-type", "type": "string", "required": false, "enum": ["sales", "debtor", "purchase", "creditor"], "default": "sales", "description": "Invoice type: sales (default, debtor items) or purchase (creditor items)."},
                    {"name": "--limit", "type": "integer", "required": false, "description": "Maximum number of results to return."},
                    {"name": "--offset", "type": "integer", "required": false, "description": "Number of results to skip (for pagination)."},
                    {"name": "--fields", "type": "string", "required": false, "description": "Comma-separated list of fields to include in output."}
                ],
                "output_fields": [
                    {"name": "contact", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "date", "type": "string"},
                    {"name": "amount", "type": "string"},
                    {"name": "open", "type": "string"}
                ]
            },
            {
                "name": "sales items",
                "description": "List sales items (the products and services you invoice).",
                "mutating": false,
                "args": [
                    {"name": "--limit", "type": "integer", "required": false, "description": "Maximum number of results to return."},
                    {"name": "--offset", "type": "integer", "required": false, "description": "Number of results to skip (for pagination)."},
                    {"name": "--fields", "type": "string", "required": false, "description": "Comma-separated list of fields to include in output."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "description", "type": "string"}
                ]
            },
            {
                "name": "sales invoice create",
                "description": "Create a sales invoice in Yuki from a TOML file or a saved template: a draft in \"To be sent\" unless --send books and sends it. Prints a preview to stderr and asks for confirmation on a terminal; --yes is required otherwise. --dry-run prints the preview and the xmlDoc and makes no API call. Exits 1 with invalid_input for a bad file, confirmation_required when not confirmed, outcome_unknown when the request went out without a usable answer (check Yuki before retrying), and invoice_rejected when Yuki fails or skips the invoice or does not book or email what --send asked.",
                "mutating": true,
                "args": [
                    {"name": "--file", "type": "path", "required": false, "description": "Invoice described in a TOML file. One of --file or --template is required."},
                    {"name": "--template", "type": "string", "required": false, "description": "Saved template name, read from ~/.config/yuki/invoices/<name>.toml."},
                    {"name": "--qty", "type": "number", "required": false, "description": "Quantity of the invoice's only line, up to 4 decimals."},
                    {"name": "--price", "type": "number", "required": false, "description": "Unit price excluding VAT of the invoice's only line, up to 2 decimals."},
                    {"name": "--date", "type": "string", "required": false, "description": "Invoice date, YYYY-MM-DD. Default: the file's date, else today."},
                    {"name": "--subject", "type": "string", "required": false, "description": "Subject (title) of the invoice, replacing the file's."},
                    {"name": "--pdf", "type": "path", "required": false, "description": "Custom invoice PDF (max 3 MB; not allowed in a template), stored in Yuki instead of the generated invoice; replaces the file's pdf."},
                    {"name": "--send", "type": "string", "required": false, "enum": ["email", "peppol", "both"], "description": "Book the invoice and send it. Without it (or --book), the invoice is a draft."},
                    {"name": "--book", "type": "boolean", "required": false, "description": "Book the invoice without sending it."},
                    {"name": "--number", "type": "string", "required": false, "description": "Invoice number (Reference) of a booked invoice (needs --send or --book), or auto: one past the highest <year>-<seq> in the sales archive (Invoice/Factuur <year>-<seq>.pdf) and the local ledger for the invoice date's year. Refused when either has it. With --pdf, required and explicit (not auto), with --date."},
                    {"name": "--dry-run", "type": "boolean", "required": false, "description": "Print the preview and the xmlDoc XML; make no API call."}
                ],
                "output_fields": [
                    {"name": "Succeeded", "type": "string"},
                    {"name": "Processed", "type": "string"},
                    {"name": "Email Sent", "type": "string"},
                    {"name": "Reference", "type": "string"},
                    {"name": "Subject", "type": "string"},
                    {"name": "PDF", "type": "string"},
                    {"name": "Message", "type": "string"}
                ]
            },
            {
                "name": "sales invoice prepare",
                "description": "Print the fully resolved invoice as JSON (number, ISO and Dutch dates, customer, lines, totals per VAT rate, Belgian structured payment reference) for rendering a PDF; create sends the same number, dates and lines for the same inputs, while the totals are the CLI's computation (Yuki books its own). Ignores any pdf. Writes nothing; reads the sales archive and the local number ledger with --number.",
                "mutating": false,
                "args": [
                    {"name": "--file", "type": "path", "required": false, "description": "Invoice described in a TOML file. One of --file or --template is required."},
                    {"name": "--template", "type": "string", "required": false, "description": "Saved template name."},
                    {"name": "--qty", "type": "number", "required": false, "description": "Quantity of the invoice's only line."},
                    {"name": "--price", "type": "number", "required": false, "description": "Unit price excluding VAT of the invoice's only line."},
                    {"name": "--date", "type": "string", "required": false, "description": "Invoice date, YYYY-MM-DD."},
                    {"name": "--subject", "type": "string", "required": false, "description": "Subject of the invoice."},
                    {"name": "--number", "type": "string", "required": false, "description": "Invoice number, or auto."}
                ],
                "output_kind": "data",
                "stdout_schema": {"type": "object", "required": ["number", "date", "customer", "lines", "totals", "payment_reference"]}
            },
            {
                "name": "sales invoice numbers",
                "description": "List the invoice numbers given out, from the local ledger (invoice-numbers.json next to the config): pending from just before Yuki is called, then booked or rejected. --resolve settles a pending number by hand after checking Yuki; makes no API call.",
                "mutating": false,
                "args": [
                    {"name": "--resolve", "type": "string[]", "required": false, "description": "NUMBER STATUS: settle a pending number as booked or rejected (a local write)."}
                ],
                "output_fields": [
                    {"name": "Number", "type": "string"},
                    {"name": "Date", "type": "string"},
                    {"name": "Customer", "type": "string"},
                    {"name": "Gross", "type": "string"},
                    {"name": "Status", "type": "string"},
                    {"name": "Recorded", "type": "string"},
                    {"name": "Booked", "type": "string"}
                ]
            },
            {
                "name": "sales invoice templates",
                "description": "List saved invoice templates (~/.config/yuki/invoices/*.toml), each validated; makes no API call.",
                "mutating": false,
                "output_fields": [
                    {"name": "Name", "type": "string"},
                    {"name": "Customer", "type": "string"},
                    {"name": "Subject", "type": "string"},
                    {"name": "Lines", "type": "string"},
                    {"name": "Net", "type": "string"},
                    {"name": "Path", "type": "string"},
                    {"name": "Status", "type": "string"}
                ]
            },
            {
                "name": "invoices show",
                "description": "Show one transaction by ID. Yuki cannot look a transaction up by ID, so this fetches every line on --account within --period (one API call) and keeps the matching one.",
                "mutating": false,
                "args": [
                    {"name": "id", "type": "string", "required": true, "description": "Transaction ID (as shown by `accounts transactions`)."},
                    {"name": "--account", "type": "string", "required": true, "description": "GL account code the transaction is booked on."},
                    {"name": "--period", "type": "string", "required": false, "description": "Period to search (e.g. 2025, 2025-Q1, 2025-01). Defaults to the current year."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "date", "type": "string"},
                    {"name": "amount", "type": "string"},
                    {"name": "currency", "type": "string"},
                    {"name": "gl_account", "type": "string"},
                    {"name": "contact", "type": "string"},
                    {"name": "description", "type": "string"}
                ]
            },
            {
                "name": "invoices document",
                "description": "Save the document linked to a transaction under its own file name, or to --out (a file or a directory); never over an existing file. Read-only towards Yuki.",
                "mutating": false,
                "args": [
                    {"name": "id", "type": "string", "required": true, "description": "Transaction ID."},
                    {"name": "--out", "type": "path", "required": false, "description": "File or directory to write to."}
                ],
                "output_fields": [
                    {"name": "Transaction", "type": "string"},
                    {"name": "Path", "type": "string"},
                    {"name": "Bytes", "type": "string"}
                ]
            },
            {
                "name": "documents download",
                "description": "Save an archive document's file under its own file name, or to --out (a file or a directory); never over an existing file. Read-only towards Yuki.",
                "mutating": false,
                "args": [
                    {"name": "id", "type": "string", "required": true, "description": "Document ID, as shown by documents list."},
                    {"name": "--out", "type": "path", "required": false, "description": "File or directory to write to."}
                ],
                "output_fields": [
                    {"name": "Document", "type": "string"},
                    {"name": "Path", "type": "string"},
                    {"name": "Bytes", "type": "string"}
                ]
            },
            {
                "name": "documents list",
                "description": "List documents in a folder or of a given type.",
                "mutating": false,
                "args": [
                    {"name": "--folder", "type": "string", "required": false, "description": "Archive folder name."},
                    {"name": "--doc-type", "type": "string", "required": false, "description": "Document type filter."},
                    {"name": "--limit", "type": "integer", "required": false, "description": "Maximum number of results to return."},
                    {"name": "--offset", "type": "integer", "required": false, "description": "Number of results to skip (for pagination)."},
                    {"name": "--fields", "type": "string", "required": false, "description": "Comma-separated list of fields to include in output."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "filename", "type": "string"},
                    {"name": "folder", "type": "string"},
                    {"name": "date", "type": "string"}
                ]
            },
            {
                "name": "documents search",
                "description": "Search documents by a query string.",
                "mutating": false,
                "args": [
                    {"name": "query", "type": "string", "required": true, "description": "Search query."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "filename", "type": "string"},
                    {"name": "folder", "type": "string"},
                    {"name": "date", "type": "string"}
                ]
            },
            {
                "name": "documents exists",
                "description": "Check if an invoice exists in the archive (by amount, date, and optional contact).",
                "mutating": false,
                "args": [
                    {"name": "--amount", "type": "number", "required": true, "description": "Invoice amount to search for."},
                    {"name": "--date", "type": "string", "required": true, "description": "Invoice date: YYYY-MM-DD matches within +/-7 days; a period (2025, 2025-Q1, 2025-03) matches the whole period."},
                    {"name": "--contact", "type": "string", "required": false, "description": "Contact/supplier name to narrow the search."}
                ],
                "output_fields": [
                    {"name": "exists", "type": "boolean"},
                    {"name": "id", "type": "string"},
                    {"name": "filename", "type": "string"}
                ]
            },
            {
                "name": "check btw",
                "description": "Check outstanding BTW (VAT) items for a period.",
                "mutating": false,
                "args": [
                    {"name": "period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025-01)."}
                ],
                "output_fields": [
                    {"name": "account", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "amount", "type": "string"}
                ]
            },
            {
                "name": "check unmatched",
                "description": "Find bank transactions without matching booked invoices.",
                "mutating": false,
                "args": [
                    {"name": "--period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025-Q1)."},
                    {"name": "--bank-account", "type": "string[]", "required": false, "description": "Bank GL account(s); repeat the flag or comma-separate values. Defaults to the administration's bank_accounts, else 11001 (nl) or 550000 (be)."}
                ],
                "output_fields": [
                    {"name": "date", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "amount", "type": "string"}
                ]
            },
            {
                "name": "check matches",
                "description": "Suggest which payments already made settle the open purchase invoices (read-only; confirm in the Yuki UI).",
                "mutating": false,
                "args": [
                    {"name": "--period", "type": "string", "required": false, "description": "Only open invoices dated in this period (e.g. 2026-Q2); payments are read up to today."},
                    {"name": "--bank-account", "type": "string[]", "required": false, "description": "Bank GL account(s); repeat the flag or comma-separate values. Defaults as for check unmatched."},
                    {"name": "--unallocated", "type": "boolean", "required": false, "description": "Also list payments booked to a supplier that no open invoice takes."}
                ],
                "output_fields": [
                    {"name": "supplier", "type": "string"},
                    {"name": "invoice_date", "type": "string"},
                    {"name": "open", "type": "string"},
                    {"name": "confidence", "type": "string", "description": "high, medium, low, card (no candidate; the supplier was paid from an account not scanned, e.g. a credit card), none (no candidate, though the supplier is paid from the bank: probably unpaid), unseen (no candidate and no payment to the supplier seen), credit (an open credit note not netted into a suggestion) or unallocated."},
                    {"name": "payment_date", "type": "string"},
                    {"name": "paid", "type": "string"},
                    {"name": "bank", "type": "string"},
                    {"name": "payment_id", "type": "string"},
                    {"name": "counterparty", "type": "string"},
                    {"name": "reason", "type": "string"}
                ]
            },
            {
                "name": "check outstanding",
                "description": "Check if a specific invoice reference is still outstanding.",
                "mutating": false,
                "args": [
                    {"name": "reference", "type": "string", "required": true, "description": "Invoice reference to check."}
                ],
                "output_fields": [
                    {"name": "reference", "type": "string"},
                    {"name": "outstanding", "type": "boolean"},
                    {"name": "amount", "type": "string"}
                ]
            },
            {
                "name": "upload file",
                "description": "Upload a document with optional invoice metadata.",
                "mutating": true,
                "args": [
                    {"name": "file", "type": "path", "required": true, "description": "Path to the file to upload."},
                    {"name": "--folder", "type": "string", "required": false, "description": "Target folder.", "default": "uitzoeken"},
                    {"name": "--amount", "type": "number", "required": false, "description": "Invoice amount (e.g. 114.27)."},
                    {"name": "--category", "type": "string", "required": false, "description": "Cost category ID (e.g. 45100)."},
                    {"name": "--payment-method", "type": "string", "required": false, "description": "Payment method ID."},
                    {"name": "--project", "type": "string", "required": false, "description": "Project ID."},
                    {"name": "--remarks", "type": "string", "required": false, "description": "Remarks or notes."},
                    {"name": "--currency", "type": "string", "required": false, "description": "Currency code.", "default": "EUR"}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "filename", "type": "string"},
                    {"name": "folder", "type": "string"}
                ]
            },
            {
                "name": "upload dir",
                "description": "Upload the pdf/jpg/jpeg/png files under a directory that are not in Yuki yet, tracked by content hash in <path>/.yuki-sync.json. Uploads are recorded as pending before they are sent and never retried automatically when the outcome is uncertain; --seed-from-yuki records files Yuki already has.",
                "mutating": true,
                "args": [
                    {"name": "path", "type": "path", "required": true, "description": "Directory to upload from."},
                    {"name": "--folder", "type": "string", "required": false, "description": "Target folder.", "default": "uitzoeken"},
                    {"name": "--exclude", "type": "string[]", "required": false, "description": "Case-insensitive glob of paths to skip; repeatable. Without / it matches any path component (a directory name skips its whole subtree); with / the whole relative path. _to_delete and .* are always skipped; symbolic links are never followed."},
                    {"name": "--max", "type": "integer", "required": false, "description": "Upload at most this many files in this run.", "default": 25},
                    {"name": "--dry-run", "type": "boolean", "required": false, "description": "Print the plan only: no API calls, nothing written."},
                    {"name": "--seed-from-yuki", "type": "boolean", "required": false, "description": "Upload nothing; record files whose file name matches exactly one unclaimed Yuki document as already-in-yuki, after confirmation."},
                    {"name": "--seed-folder", "type": "string[]", "required": false, "description": "Yuki folder to look in when seeding; repeatable. Defaults to --folder and inkoop."}
                ],
                "output_fields": [
                    {"name": "Path", "type": "string", "description": "Path relative to the directory."},
                    {"name": "Action", "type": "string", "description": "uploaded, pending, failed, changed, error, not-attempted, deferred, synced, duplicate, excluded; would-upload or would-seed on --dry-run; already-in-yuki, ambiguous, possible-match or not-in-yuki when seeding."},
                    {"name": "Doc ID", "type": "string"},
                    {"name": "Error", "type": "string"},
                    {"name": "Note", "type": "string"}
                ]
            },
            {
                "name": "upload mark",
                "description": "Record by hand, in the synced directory's .yuki-sync.json, that a file is in Yuki as a document (--doc-id), must never be uploaded (--skip), or is to be forgotten (--forget). Does not contact Yuki.",
                "mutating": true,
                "args": [
                    {"name": "file", "type": "path", "required": true, "description": "The file to record."},
                    {"name": "--doc-id", "type": "string", "required": false, "description": "The Yuki document ID the file was uploaded as."},
                    {"name": "--skip", "type": "boolean", "required": false, "description": "Never upload this file."},
                    {"name": "--forget", "type": "boolean", "required": false, "description": "Remove the record, so the next run treats the file as new; for a changed file, the record of the earlier content at its path."},
                    {"name": "--folder", "type": "string", "required": false, "description": "The Yuki folder the document is in."},
                    {"name": "--note", "type": "string", "required": false, "description": "Note to keep with the record."},
                    {"name": "--dir", "type": "path", "required": false, "description": "The synced directory (its root); defaults to the nearest directory above the file with a .yuki-sync.json."},
                    {"name": "--force", "type": "boolean", "required": false, "description": "Replace an existing record, or record a document ID already recorded for another file."}
                ],
                "output_fields": [
                    {"name": "Path", "type": "string"},
                    {"name": "Action", "type": "string", "description": "recorded, unchanged, or forgotten."},
                    {"name": "Doc ID", "type": "string"},
                    {"name": "Error", "type": "string"},
                    {"name": "Note", "type": "string"}
                ]
            },
            {
                "name": "upload categories",
                "description": "List available cost categories.",
                "mutating": false,
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "description", "type": "string"}
                ]
            },
            {
                "name": "upload payment-methods",
                "description": "List available payment methods.",
                "mutating": false,
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "description", "type": "string"}
                ]
            },
            {
                "name": "init",
                "description": "Initialize yuki configuration for this machine. The key's region is detected by trying it on every known Yuki host, unless --region or --base-url is given, and recorded. An access key reaches only the administrations it was created inside, so a second administration needs its own key added with --add.",
                "mutating": true,
                "args": [
                    {"name": "--api-key", "type": "string", "required": false, "description": "API key (skips interactive prompt if provided)."},
                    {"name": "--default-admin", "type": "string", "required": false, "description": "Default administration name (auto-selects if only one available)."},
                    {"name": "--add", "type": "boolean", "required": false, "description": "Merge the key's administrations into the existing configuration instead of replacing it."}
                ]
            },
            {
                "name": "auth login",
                "description": "Configure an API key and discover its administrations. Equivalent to init.",
                "mutating": true,
                "args": [
                    {"name": "--api-key", "type": "string", "required": false, "description": "API key (skips interactive prompt if provided)."},
                    {"name": "--default-admin", "type": "string", "required": false, "description": "Default administration name."},
                    {"name": "--add", "type": "boolean", "required": false, "description": "Merge the key's administrations into the existing configuration."}
                ]
            },
            {
                "name": "auth status",
                "description": "Show whether the selected administration profile is configured and valid.",
                "mutating": false,
                "args": [
                    {"name": "--offline", "type": "boolean", "required": false, "description": "Check local configuration without contacting Yuki."}
                ],
                "output_fields": [
                    {"name": "profile", "type": "string"},
                    {"name": "status", "type": "string"},
                    {"name": "configured", "type": "boolean"},
                    {"name": "verified", "type": "boolean"},
                    {"name": "credential_source", "type": "string"}
                ]
            },
            {
                "name": "auth logout",
                "description": "Disable the stored API key for the selected administration profile without affecting other profiles.",
                "mutating": true,
                "output_fields": [
                    {"name": "profile", "type": "string"},
                    {"name": "logged_out", "type": "boolean"},
                    {"name": "credential_removed", "type": "boolean"},
                    {"name": "environment_override", "type": "boolean"}
                ]
            },
            {
                "name": "profile list",
                "description": "List locally configured administration profiles.",
                "mutating": false,
                "output_fields": [
                    {"name": "items", "type": "array", "items": {"type": "object", "fields": [
                        {"name": "name", "type": "string"},
                        {"name": "display_name", "type": "string"},
                        {"name": "active", "type": "boolean"},
                        {"name": "admin_id", "type": "string"},
                        {"name": "domain_id", "type": "string"},
                        {"name": "configured", "type": "boolean"},
                        {"name": "credential_source", "type": "string"}
                    ]}},
                    {"name": "total", "type": "integer"}
                ]
            },
            {
                "name": "profile use",
                "description": "Select the default administration profile. Equivalent to admin switch.",
                "mutating": true,
                "args": [
                    {"name": "name", "type": "string", "required": true, "description": "Profile name to select."}
                ],
                "output_fields": [
                    {"name": "profile", "type": "string"},
                    {"name": "active", "type": "boolean"}
                ]
            },
            {
                "name": "profile remove",
                "description": "Remove an administration profile.",
                "mutating": true,
                "args": [
                    {"name": "name", "type": "string", "required": true, "description": "Profile name to remove."}
                ],
                "output_fields": [
                    {"name": "profile", "type": "string"},
                    {"name": "removed", "type": "boolean"}
                ]
            },
            {
                "name": "config show",
                "description": "Show configuration without revealing API keys.",
                "mutating": false,
                "output_fields": [
                    {"name": "config_file", "type": "string"},
                    {"name": "file_exists", "type": "boolean"},
                    {"name": "active_profile", "type": "string"},
                    {"name": "profiles", "type": "object"},
                    {"name": "shared_api_key_configured", "type": "boolean"}
                ]
            },
            {
                "name": "config path",
                "description": "Print the configuration file path.",
                "mutating": false,
                "output_fields": [
                    {"name": "config_path", "type": "string"}
                ]
            },
            {
                "name": "doctor",
                "description": "Check configuration and Yuki connectivity.",
                "mutating": false,
                "args": [
                    {"name": "--offline", "type": "boolean", "required": false, "description": "Check local configuration without contacting Yuki."}
                ],
                "output_fields": [
                    {"name": "ok", "type": "boolean"},
                    {"name": "offline", "type": "boolean"},
                    {"name": "checks", "type": "array", "items": {"type": "object", "fields": [
                        {"name": "name", "type": "string"},
                        {"name": "ok", "type": "boolean"},
                        {"name": "detail", "type": "string"}
                    ]}}
                ]
            },
            {
                "name": "schema",
                "description": "Output JSON schema for agent integration.",
                "mutating": false
            },
            {
                "name": "capabilities",
                "description": "Describe supported API areas and safety behavior without loading configuration.",
                "mutating": false,
                "output_fields": [
                    {"name":"areas","type":"array","items":{"type":"string"}},
                    {"name":"structured_output","type":"boolean"},
                    {"name":"daily_api_limit","type":"integer"}
                ]
            },
            {
                "name": "completions",
                "description": "Generate shell completions.",
                "mutating": false,
                "args": [
                    {"name": "shell", "type": "string", "required": true, "description": "Shell to generate completions for.", "enum": ["bash", "fish", "zsh", "powershell", "elvish"]}
                ]
            }
        ],
        "errors": [
            {
                "kind": "auth_failed",
                "exit_code": 2,
                "retryable": false,
                "description": "Authentication failed: invalid or expired API key."
            },
            {
                "kind": "not_found",
                "exit_code": 3,
                "retryable": false,
                "description": "The requested resource was not found."
            },
            {
                "kind": "rate_limited",
                "exit_code": 4,
                "retryable": true,
                "description": "API rate limit exceeded (1000 calls/day)."
            },
            {
                "kind": "config_error",
                "exit_code": 1,
                "retryable": false,
                "description": "Configuration error: missing or invalid config file."
            },
            {
                "kind": "confirmation_required",
                "exit_code": 1,
                "retryable": false,
                "description": "A mutating command was invoked non-interactively without --yes."
            },
            {
                "kind": "invalid_input",
                "exit_code": 1,
                "retryable": false,
                "description": "An invoice file or template is missing or invalid; every problem is listed."
            },
            {
                "kind": "outcome_unknown",
                "exit_code": 1,
                "retryable": false,
                "description": "The invoice request went out but no usable answer came back: it may already exist in Yuki, so check before retrying."
            },
            {
                "kind": "invoice_rejected",
                "exit_code": 1,
                "retryable": false,
                "description": "Yuki answered, but failed or skipped an invoice, or did not book or email it as --send asked; its message is in the output and the error."
            },
            {
                "kind": "error",
                "exit_code": 1,
                "retryable": false,
                "description": "An unexpected error occurred."
            }
        ]
    });
    enrich_v0_3(&mut schema);
    schema
}

fn enrich_v0_3(schema: &mut Value) {
    schema["output"] = json!({"tty":"text","piped":"json"});
    let Some(commands) = schema["commands"].as_array_mut() else {
        return;
    };
    for command in commands {
        let Some(object) = command.as_object_mut() else {
            continue;
        };
        let name = object["name"].as_str().unwrap_or_default().to_string();
        let mutating = object["mutating"].as_bool().unwrap_or(false);
        object.insert(
            "effects".into(),
            json!(if !mutating {
                "read_only"
            } else if matches!(name.as_str(), "upload file" | "sales invoice create") {
                "non_idempotent"
            } else {
                "idempotent"
            }),
        );
        if name == "completions" {
            object.insert("output_kind".into(), json!("opaque"));
            object.insert("media_type".into(), json!("text/plain"));
            continue;
        }
        let unbounded = object
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|args| {
                args.iter().any(|arg| arg["name"] == "--limit")
                    && args.iter().any(|arg| arg["name"] == "--offset")
                    && args.iter().any(|arg| arg["name"] == "--fields")
            });
        object.insert(
            "cardinality".into(),
            json!(if unbounded { "unbounded" } else { "bounded" }),
        );
        if unbounded {
            object.insert(
                "pagination".into(),
                json!({"style":"offset","limit_arg":"--limit","offset_arg":"--offset"}),
            );
            object.insert("fields_arg".into(), json!("--fields"));
        }
        if matches!(
            name.as_str(),
            "upload file" | "upload dir" | "profile remove" | "sales invoice create"
        ) {
            object.insert("confirmation_bypass_arg".into(), json!("--yes"));
        }
        if name == "capabilities" {
            object.insert("example".into(), json!({"args":["capabilities"]}));
        }
        if name == "schema" {
            object.insert("cardinality".into(), json!("single"));
            object.insert(
                "stdout_schema".into(),
                json!({"$ref":"https://clispec.dev/schema/v0.3.json"}),
            );
        }
        if !object.contains_key("output_fields") && !object.contains_key("stdout_schema") {
            object.insert("stdout_schema".into(), json!({}));
        }
    }
}

pub fn print_schema() {
    let schema = generate();
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("serialize schema")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonschema::Validator;
    use serde_json::Value;

    #[test]
    fn schema_is_valid_json() {
        let schema = generate();
        let serialized = serde_json::to_string_pretty(&schema).unwrap();
        let _: Value = serde_json::from_str(&serialized).unwrap();
    }

    #[test]
    fn schema_has_required_top_level_keys() {
        let schema = generate();
        assert!(schema.get("clispec").is_some(), "missing clispec field");
        assert!(schema.get("name").is_some(), "missing name field");
        assert!(schema.get("version").is_some(), "missing version field");
        assert!(schema.get("commands").is_some(), "missing commands field");
        assert!(schema.get("errors").is_some(), "missing errors field");
        assert!(
            schema.get("global_args").is_some(),
            "missing global_args field"
        );
    }

    #[test]
    fn schema_clispec_version() {
        let schema = generate();
        assert_eq!(schema["clispec"], "0.3");
    }

    #[test]
    fn schema_commands_is_array() {
        let schema = generate();
        assert!(schema["commands"].is_array(), "commands must be an array");
    }

    #[test]
    fn schema_all_commands_have_mutating_field() {
        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        for cmd in commands {
            let name = cmd["name"].as_str().unwrap_or("unknown");
            assert!(
                cmd.get("mutating").is_some(),
                "command '{name}' is missing 'mutating' field"
            );
        }
    }

    #[test]
    fn schema_errors_have_exit_codes() {
        let schema = generate();
        let errors = schema["errors"].as_array().unwrap();
        for err in errors {
            let kind = err["kind"].as_str().unwrap_or("unknown");
            assert!(
                err.get("exit_code").is_some(),
                "error '{kind}' is missing 'exit_code'"
            );
        }
    }

    #[test]
    fn schema_global_args_has_output_flag() {
        let schema = generate();
        let global_args = schema["global_args"].as_array().unwrap();
        let has_output = global_args.iter().any(|a| a["name"] == "--output");
        assert!(has_output, "global_args must include --output flag");
    }

    #[test]
    fn schema_global_args_has_yes_flag() {
        let schema = generate();
        let global_args = schema["global_args"].as_array().unwrap();
        let has_yes = global_args.iter().any(|a| a["name"] == "--yes");
        assert!(has_yes, "global_args must include --yes flag");
    }

    #[test]
    fn schema_includes_leaf_commands() {
        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        let names: Vec<&str> = commands
            .iter()
            .map(|c| c["name"].as_str().unwrap_or(""))
            .collect();
        assert!(names.contains(&"admin list"), "missing 'admin list'");
        assert!(names.contains(&"vat returns"), "missing 'vat returns'");
        assert!(names.contains(&"invoices list"), "missing 'invoices list'");
        assert!(names.contains(&"auth status"), "missing 'auth status'");
        assert!(names.contains(&"profile list"), "missing 'profile list'");
        assert!(names.contains(&"config path"), "missing 'config path'");
        assert!(names.contains(&"doctor"), "missing 'doctor'");
    }

    /// Leaf commands and their arguments as clap defines them, keyed "group sub".
    fn clap_leaves() -> Vec<(String, Vec<String>)> {
        use clap::CommandFactory;
        fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<(String, Vec<String>)>) {
            for sub in cmd.get_subcommands() {
                let name = if prefix.is_empty() {
                    sub.get_name().to_string()
                } else {
                    format!("{prefix} {}", sub.get_name())
                };
                if sub.has_subcommands() {
                    walk(sub, &name, out);
                } else {
                    let mut args: Vec<String> = sub
                        .get_arguments()
                        .filter(|a| !a.is_global_set() && a.get_id() != "help")
                        .map(|a| match a.get_long() {
                            Some(long) => format!("--{long}"),
                            None => a.get_id().to_string(),
                        })
                        .collect();
                    args.sort();
                    out.push((name, args));
                }
            }
        }
        let mut out = Vec::new();
        walk(&crate::cli::Cli::command(), "", &mut out);
        out
    }

    #[test]
    fn schema_commands_and_args_match_clap() {
        let schema = generate();
        let documented: std::collections::BTreeMap<String, Vec<String>> = schema["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let mut args: Vec<String> = c["args"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|arg| arg["name"].as_str().unwrap().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                args.sort();
                (c["name"].as_str().unwrap().to_string(), args)
            })
            .collect();
        let mut problems = Vec::new();
        for (name, args) in clap_leaves() {
            if matches!(name.as_str(), "completions" | "schema" | "capabilities") {
                continue;
            }
            match documented.get(&name) {
                None => problems.push(format!("{name}: missing from schema")),
                Some(doc_args) if doc_args != &args => {
                    problems.push(format!("{name}: schema {doc_args:?} != clap {args:?}"))
                }
                Some(_) => {}
            }
        }
        assert!(
            problems.is_empty(),
            "schema drift:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn schema_validates_against_clispec_v03() {
        let schema_json: Value =
            serde_json::from_str(include_str!("../tests/fixtures/schema-v0.3.json"))
                .expect("parse clispec schema fixture");

        let validator = Validator::new(&schema_json).expect("compile clispec schema");
        let output = generate();
        if let Err(e) = validator.validate(&output) {
            panic!("Schema does not validate against clispec v0.3:\n{e}");
        }
    }

    #[test]
    fn schema_works_without_config() {
        // Must not panic or require any config file; generate() is purely static.
        let schema = generate();
        assert!(schema.get("name").is_some());
    }

    #[test]
    fn list_commands_have_limit_flag() {
        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        let list_cmd = commands
            .iter()
            .find(|c| c["name"] == "contacts list")
            .unwrap();
        let args = list_cmd["args"].as_array().unwrap();
        let has_limit = args.iter().any(|a| a["name"] == "--limit");
        assert!(has_limit, "contacts list is missing --limit arg");
    }

    #[test]
    fn list_commands_have_offset_flag() {
        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        let list_cmd = commands
            .iter()
            .find(|c| c["name"] == "contacts list")
            .unwrap();
        let args = list_cmd["args"].as_array().unwrap();
        let has_offset = args.iter().any(|a| a["name"] == "--offset");
        assert!(has_offset, "contacts list is missing --offset arg");
    }

    #[test]
    fn list_commands_have_fields_flag() {
        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        let list_cmd = commands
            .iter()
            .find(|c| c["name"] == "contacts list")
            .unwrap();
        let args = list_cmd["args"].as_array().unwrap();
        let has_fields = args.iter().any(|a| a["name"] == "--fields");
        assert!(has_fields, "contacts list is missing --fields arg");
    }

    #[test]
    fn output_fields_declared_on_commands() {
        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        let with_output_fields = commands
            .iter()
            .filter(|c| {
                c.get("output_fields")
                    .and_then(|f| f.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            })
            .count();
        assert!(
            with_output_fields > 0,
            "at least some commands must have output_fields"
        );
    }

    /// Every repeatable or delimited clap flag is declared as an array type
    /// (`"string[]"`), which is how clispec v0.3 marks a multi-value argument.
    #[test]
    fn multi_value_flags_are_declared_as_arrays() {
        use clap::{ArgAction, CommandFactory};

        fn walk(cmd: &clap::Command, path: &str, out: &mut Vec<(String, String)>) {
            for arg in cmd.get_arguments() {
                if arg.is_global_set() || arg.get_long().is_none() {
                    continue;
                }
                let multi = matches!(arg.get_action(), ArgAction::Append)
                    || arg.get_value_delimiter().is_some();
                if multi {
                    out.push((path.to_string(), format!("--{}", arg.get_long().unwrap())));
                }
            }
            for sub in cmd.get_subcommands() {
                let name = if path.is_empty() {
                    sub.get_name().to_string()
                } else {
                    format!("{path} {}", sub.get_name())
                };
                walk(sub, &name, out);
            }
        }

        let mut multi = Vec::new();
        walk(&crate::cli::Cli::command(), "", &mut multi);
        assert!(!multi.is_empty(), "expected at least --bank-account");

        let schema = generate();
        let commands = schema["commands"].as_array().unwrap();
        for (command, flag) in multi {
            let Some(cmd) = commands.iter().find(|c| c["name"] == command.as_str()) else {
                continue;
            };
            let arg = cmd["args"]
                .as_array()
                .and_then(|args| args.iter().find(|a| a["name"] == flag.as_str()))
                .unwrap_or_else(|| panic!("{command}: {flag} missing from schema"));
            assert_eq!(arg["type"], "string[]", "{command} {flag}");
        }
    }
}
