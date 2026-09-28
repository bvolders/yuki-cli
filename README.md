# yuki

[![codecov](https://codecov.io/gh/rvben/yuki-cli/graph/badge.svg)](https://codecov.io/gh/rvben/yuki-cli)

CLI client for the [Yuki](https://www.yukiworks.nl) bookkeeping SOAP API.

[Yuki](https://www.yukiworks.nl) is a Dutch bookkeeping SaaS used for accounting, VAT returns, and document archiving. This CLI lets you query your administration, find missing invoices, and upload documents — from the terminal or as part of automated workflows.

> **Note:** This project is not affiliated with or endorsed by Yuki Software.

## Install

```sh
cargo install yuki-cli
```

Or via pip:

```sh
pip install yuki-cli

# Or run without installing
uvx yuki-cli --help
```

PyPI and Cargo installations provide both `yuki` and `yuki-cli` as executable names.

## Setup

1. Get a Yuki API key from your Yuki portal under **Settings > API keys**.
2. Run `yuki init` and paste your key when prompted. The CLI discovers your administrations and writes the config to `~/.config/yuki/config.toml`.

```sh
yuki init
```

`yuki auth login` is the canonical account command; `yuki init` remains a
compatible shortcut.

Non-interactive (for scripting):

```sh
yuki init --api-key <key> --default-admin <name>
```

To rotate your API key later:

```sh
yuki init --api-key <new-key>
```

### Reaching more than one administration

Yuki issues an access key *inside* one administration and scopes the session it opens
to that administration. A key created in company A therefore cannot see company B,
even when the same person owns both. Create a second key in the Yuki portal of the
other administration (**Settings > API keys**), then add it:

```sh
yuki init --add --api-key <second-key>
```

`--add` merges what the new key reaches into the existing config instead of replacing
it, and records the key on the administrations only that key can reach. After that a
single CLI covers both, and `--admin <name>` picks between them:

```sh
yuki admin list                           # every configured administration, with status
yuki documents search "loonstrook" --admin holding_b_v
```

Yuki calls these accounting scopes “administrations.” In the shared CLI
account workflow, an administration is a profile: `--profile` aliases
`--admin`, `profile list` is the local account view, and `profile use` aliases
`admin switch`.

`yuki admin list` contacts each configured key once and reports every configured
administration, so one that no key can reach shows up with a `Status` of `auth failed`
rather than silently disappearing from the list. Use `--local` to see the
configuration without any API call.

## Quick start: find missing invoices

The main workflow is finding bank transactions that don't have a matching invoice in Yuki:

```sh
# Show bank debits without matching invoices for Q1 2025
yuki check unmatched --period 2025-Q1
```

This cross-references bank transactions against outstanding creditor items, booked archive documents, and known counterparty names. The output shows unmatched transactions with their date, amount, counterparty, and description.

For each unmatched item, you can check if the invoice is already in the archive, and upload it if not:

```sh
# Check if an invoice already exists
yuki documents exists --amount 7.28 --date 2025-03

# Upload an invoice (Yuki auto-sorts it)
yuki upload file invoice.pdf

# Or upload to a specific folder with metadata
yuki upload file invoice.pdf --folder inkoop --amount 7.28 --remarks "Hetzner hosting"
```

## Commands

### Querying

```sh
yuki vat returns                          # List all VAT return periods
yuki vat returns 2025                     # Filter by year
yuki vat codes                            # List active VAT codes

yuki invoices list                        # Outstanding sales invoices (debtor items)
yuki invoices list --invoice-type purchase # Outstanding purchase invoices (creditor items)
yuki invoices list --period 2025-Q1       # Only items dated in the period
yuki invoices show <transaction-id> --account 400000 --period 2025-Q1  # One transaction
yuki invoices document <transaction-id>   # Document linked to a transaction
yuki sales items                          # Sales item catalogue (products/services)

yuki contacts search "Hetzner"            # Search contacts
yuki contacts list                        # List all suppliers and customers

yuki accounts balance --account 11001 --period 2025-Q1
yuki accounts transactions --account 11001 --period 2025-Q1
yuki accounts scheme                      # Chart of accounts (GL scheme)
yuki accounts revenue --period 2025-Q1    # Net revenue for a period
yuki accounts start-balance --year 2025   # Opening balances per GL account

yuki projects list                        # List all projects
yuki projects balance <code> --period 2025  # Project balance

yuki documents list --folder inkoop       # List documents in a folder
yuki documents search "factuur"           # Full-text search
yuki documents exists --amount 7.28 --date 2025-03  # Check if invoice exists

yuki admin list                           # List administrations
yuki admin switch <name>                  # Change default administration
```

`invoices show` needs the GL account the transaction is booked on: Yuki has no
lookup by transaction ID, so the CLI fetches that account's lines for `--period`
(default: the current year) in one API call and keeps the matching one. A
narrower period means a smaller response.

### Authentication and configuration

```sh
yuki init --profile <name>                # Compatible setup shortcut
yuki auth login --profile <name>          # Configure and verify an API key
yuki auth status [--offline] --profile <name>
yuki auth logout --profile <name>         # Disable only this administration's key
yuki profile list                         # Local; never contacts Yuki
yuki profile use <name>
yuki profile remove <name> --yes
yuki config show                          # Never reveals API keys
yuki config path
yuki doctor [--offline]
```

`auth status` and `doctor` contact Yuki by default and validate both the API
key and selected administration. `--offline` checks the stored configuration
only. Because a Yuki key can serve more than one administration, logout writes
an explicit disabled credential for the selected profile rather than removing
a shared key used by other profiles.

### Gap analysis

```sh
yuki check btw 2025-Q4                    # VAT period check: outstanding items
yuki check unmatched --period 2026-Q1     # Bank debits without matching invoices
yuki check outstanding <reference>        # Check if a reference is still outstanding
```

### Uploading

```sh
yuki upload file invoice.pdf                           # Upload to uitzoeken (auto-sorted)
yuki upload file invoice.pdf --folder inkoop            # Upload to specific folder
yuki upload file invoice.pdf --amount 114.27 \
  --category 45100 --payment-method 4 \
  --remarks "Hosting"                                   # Upload with metadata

yuki upload categories                                  # List cost category IDs
yuki upload payment-methods                             # List payment method IDs
```

### Global flags

| Flag | Description |
|------|-------------|
| `--profile <name>` / `--admin <name>` | Override default administration profile |
| `--output text\|json` | Output format (auto-detects TTY) |
| `--quiet` | Suppress informational output |
| `--yes` | Confirm destructive operations |

## Periods

The `--period` flag accepts:

- `2025` — full year
- `2025-Q1` — quarter
- `2025-03` — single month

## Agent use

When stdout is not a TTY (piped or called by an agent), output defaults to JSON. Errors are also structured JSON on stderr. Exit codes: 0 success, 1 general error, 2 auth error, 3 not found, 4 rate limited.

The `documents exists` command exits with code 3 when no matching document is found, making it easy to use in scripts and agent workflows.

## Config

`~/.config/yuki/config.toml`:

```toml
# Used by any administration that does not carry a key of its own.
api_key = "your-api-key"
default_admin = "company_name"

# Skip these counterparties in `check unmatched` (case-insensitive substring match)
unmatched_ignore = [
  "Belastingdienst",
  "ING bankkosten",
]

[administrations.company_name]
domain_id = "domain-uuid"
admin_id = "admin-uuid"
name = "Example Trading B.V."

[administrations.holding_b_v]
domain_id = "other-domain-uuid"
admin_id = "other-admin-uuid"
name = "Example Holding B.V."
# Written by `yuki init --add`, because the shared key above cannot reach this one.
api_key = "second-api-key"
```

`name` and the per-administration `api_key` are optional. An administration without
its own key uses the shared one, so rotating the shared key keeps reaching it.

## Development

```
make check    # Run clippy + fmt check + tests
make build    # Debug build
make release  # Release build
make fmt      # Format code
make install  # Install to ~/.cargo/bin/
```

## License

MIT

## Releasing

Vership owns versioning, changelog generation, release commits, and tags. See
[the release runbook](docs/releases.md) for the verified workflow and recovery policy.
