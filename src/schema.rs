use serde_json::{Value, json};

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
                "description": "Search contacts by name or other criteria.",
                "mutating": false,
                "args": [
                    {"name": "query", "type": "string", "required": true, "description": "Search query."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "name", "type": "string"},
                    {"name": "type", "type": "string"}
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
                "description": "Show the balance of a general ledger account for a period.",
                "mutating": false,
                "args": [
                    {"name": "--account", "type": "string", "required": false, "description": "GL account code."},
                    {"name": "--period", "type": "string", "required": false, "description": "Accounting period (e.g. 2025-01)."}
                ],
                "output_fields": [
                    {"name": "account", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "balance", "type": "string"}
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
                "name": "invoices show",
                "description": "Show details for a single invoice.",
                "mutating": false,
                "args": [
                    {"name": "id", "type": "string", "required": true, "description": "Invoice ID."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "date", "type": "string"},
                    {"name": "contact", "type": "string"},
                    {"name": "amount", "type": "string"},
                    {"name": "status", "type": "string"}
                ]
            },
            {
                "name": "invoices document",
                "description": "Show the document linked to a transaction.",
                "mutating": false,
                "args": [
                    {"name": "id", "type": "string", "required": true, "description": "Transaction ID."}
                ],
                "output_fields": [
                    {"name": "id", "type": "string"},
                    {"name": "filename", "type": "string"},
                    {"name": "url", "type": "string"}
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
                    {"name": "--date", "type": "string", "required": true, "description": "Invoice date (YYYY-MM-DD). Matches within +/-7 days."},
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
                    {"name": "--bank-account", "type": "string", "required": false, "description": "GL account code for the bank account.", "default": "11001"}
                ],
                "output_fields": [
                    {"name": "date", "type": "string"},
                    {"name": "description", "type": "string"},
                    {"name": "amount", "type": "string"}
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
                "description": "Initialize yuki configuration for this machine. An access key reaches only the administrations it was created inside, so a second administration needs its own key added with --add.",
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
            } else if name == "upload file" {
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
        if matches!(name.as_str(), "upload file" | "profile remove") {
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
}
