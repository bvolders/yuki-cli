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
2. Run `yuki init` and paste your key when prompted. The CLI detects the key's region (Netherlands or Belgium), discovers your administrations and writes the config to `~/.config/yuki/config.toml`.

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

### Regions

Yuki runs a separate API host per country — `api.yukiworks.nl` and
`api.yukiworks.be` — and an access key only works on its own country's host.
`yuki init` finds out which by trying the key on every known host at once, and
records the answer in the config, so later commands need nothing extra:

```text
Detecting region... ✔
Detected Yuki Belgium (api.yukiworks.be)
```

A host answering "Invalid access key" is simply not the key's region. Any other
failure (unreachable, blocked, rate limited) is reported rather than skipped, and
`init` stops. If no known host accepts the key, or more than one does, `init` asks
`Yuki region [nl/be/other]:` with no default when run at a terminal; `other` takes a
full API root for a deployment outside the known ones, verifies the key there, and
stores it as `base_url`. Without a terminal it stops instead: an unknown key is the
usual authentication error (exit 2), an ambiguous one a configuration error asking
for `--region`.

Passing `--region nl|be` or `--base-url <root>` (or exporting `YUKI_REGION` /
`YUKI_BASE_URL`) skips detection. A fresh `init` — nothing exists yet to protect
from the environment — persists whichever endpoint was actually used, whether it
came from the flag or from `YUKI_REGION`/`YUKI_BASE_URL`, exactly like a typed
`--region`/`--base-url`; a URL matching no known region is stored as `base_url`,
one that does is stored as `region` instead. Re-running `init` on an *existing*
config, rotating the key, or `--add` is different: only the flag is persisted
there, since an exported `YUKI_REGION`/`YUKI_BASE_URL` is a per-shell choice and
must not silently rewrite a configuration that already exists. `yuki init --add` detects per
key and records the region (or `base_url`) on the administrations that key reaches,
so Dutch and Belgian books can live in one config. Re-running `yuki init` keeps the
per-administration settings (region, and any other keys in its table) of every
administration the key still reaches.

The endpoint is resolved in this order, highest first:

1. `--base-url` / `YUKI_BASE_URL`, then `--region` / `YUKI_REGION` — for this run,
   for every administration and every key (so `admin list` in a mixed config reports
   the other deployment's keys as failing);
2. the administration's own `base_url`, then its own `region`;
3. `base_url` in the config file (replaces the default endpoint only);
4. the top-level `region`;
5. `nl` — a legacy fallback, only for a config written before `init` recorded the
   region.

Country conventions (chart of accounts, bank formats) follow the same order; a base
URL that is not a known Yuki root (a proxy, a mock) is skipped for that purpose.

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

yuki accounts balance --account 11001 --period 2025-Q1  # Balance on 2025-03-31 (or today, if earlier)
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

`accounts balance` reports the balance at the **end** of `--period` (today, in
the local time zone, while the period is still running) and says which date in
an `As Of` column. Releases up to 0.1.13 reported the balance at the period's start,
without that column.

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
yuki check matches                        # Payments that may settle open purchase invoices
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
| `--output auto\|text\|json` | Output format (default auto: table on a TTY, JSON when piped) |
| `--quiet` | Suppress informational output |
| `--yes` | Confirm destructive operations |
| `--region nl\|be` | Yuki deployment; `init` detects it from the key when omitted (env `YUKI_REGION`; stored by `init` on a fresh config, flag-only on an existing one) |
| `--base-url <root>` | Full API root, overrides `--region` (env `YUKI_BASE_URL`; same storage rule as `--region`) |

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

### `check unmatched` per administration

`check unmatched` scans the bank GL account(s) given with `--bank-account`
(repeat it or comma-separate), else the administration's `bank_accounts`, else
the region default: `11001` (nl) or `550000` (be). For Belgian administrations it
also reads the supplier ledger (`440000`, three months back) to name the
counterparty and check whether a purchase invoice of that supplier covers the
payment, treats a same-day counter-entry on `580000` or another scanned bank
account as an own transfer, and skips loans, credit fees, card settlements,
salaries and tax payments by description. On the supplier ledger, purchase
credit notes reduce that supplier's open invoices and refunds received never
count as invoices; payments are matched oldest first, including those before the
period, so an invoice already paid cannot cover a later payment. A bank line
booked straight to a GL account is skipped only when that account never has a
document (`no_document_accounts`, default `65`: interest, bank costs `657xxx`,
exchange differences); one booked straight to any other account, such as a
`6xxxxx` expense, is reported. The archive is searched in the administration's
own domain (the session that ran `SetCurrentDomain`), whereas `documents` and
`upload` use the API key's default domain. All of it can be tuned per
administration:

```toml
[administrations.example_bv]
domain_id = "domain-uuid"
admin_id = "admin-uuid"
region = "be"
bank_accounts = ["550002", "550003"]
creditor_accounts = ["440000"]       # [] turns the supplier ledger off
transfer_accounts = ["580000"]
# Replaces the Belgian defaults; matched against the full bank description.
unmatched_ignore_descriptions = ["Lening op korte termijn", "Betaling lonen"]
# GL prefixes a bank line may be booked to without a document; [] reports all.
no_document_accounts = ["657", "650"]
```

Dutch administrations keep the original behaviour unless these are set.

### `check matches`: which open invoices are already paid

`check matches` pairs open purchase invoices (the outstanding creditor items,
as `invoices list --invoice-type purchase` shows them) with payments already
made. It only suggests: Yuki's API cannot link a payment to an invoice, so each
pair is confirmed in the Yuki UI. It reads the same accounts as `check
unmatched` (bank, supplier ledger, transfers) from the oldest open invoice's
date minus 90 days up to today; `--period` only narrows which invoices are
considered. A payment is a candidate when no closed invoice explains it: a bank
debit booked to the supplier but not linked to its invoice, a supplier-ledger
payment from an account that is not scanned (e.g. a credit card), or a bank debit
not booked to a supplier at all (a card payment or direct debit). Own
transfers, ignored descriptions and no-document bookings are skipped as in
`check unmatched`.

| Confidence | When |
|---|---|
| `high` | same amount and supplier, at most 30 days apart; or one payment to a supplier adding up several of its invoices (within 7 days) |
| `medium` | same amount and supplier, 31 to 90 days apart; several supplier payments adding up to one invoice; or one payment adding up invoices of several suppliers, at least one of them the payment's (its own within 7 days, the others dated that day, as with a marketplace order invoiced per seller); a payment naming another supplier never adds them up |
| `low` | same amount, the payment names no supplier, at most 30 days apart; or such a payment adding up invoices dated that day |
| `card` | no candidate, and either the invoice's payment method is a card (`Creditcard`), or the supplier ledger shows the supplier paid in the window from an account that is not scanned (in practice the credit card, whose purchases have no bank line of their own) |
| `none` | no candidate, but the supplier was paid from a scanned bank account in the window, so its payment would show: probably unpaid |
| `unseen` | no candidate, and no payment to the supplier at all in the window: a new supplier, or one paid by a card payment or direct debit whose bank line names nobody. Not evidence of either card or unpaid |

Amounts must match to the cent; sums add up to four items. The exception is an
invoice of a supplier outside the euro area (the open item carries the
supplier's country, not the currency): Yuki books it at its own exchange rate
and the card is charged at another, so a payment of that supplier within 3% or
1.00 (whichever is larger) and 30 days is suggested as `medium`, with a reason
starting `FX:`. Each payment and each
invoice is used once, strongest confidence first; at equal confidence an exact
1:1 match before an FX match or a sum, then closest in date.
`--unallocated` adds the supplier payments no open invoice took.

```sh
yuki check matches                     # every open purchase invoice
yuki check matches --period 2026-Q2    # only invoices dated in Q2
yuki check matches --unallocated -o json
```

Known limitation: individual credit-card purchases are not visible. The bank GL
account only shows the monthly card settlement ("Afrekening kredietkaarten"),
which is skipped, so a card purchase without an invoice is not reported.

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
