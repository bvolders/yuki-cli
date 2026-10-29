# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```sh
make check          # Run clippy (warnings=errors) + fmt check + tests
make test           # cargo test
make lint           # clippy + fmt check
make fmt            # auto-format
make install        # cargo install --path . (installs to ~/.cargo/bin/yuki)
cargo test <name>   # Run a single test by name
```

After any code change that affects the binary, run `make install` to update `~/.cargo/bin/yuki`.

## Architecture

Three-layer design: **CLI** (clap) → **command handlers** → **typed SOAP clients**.

```
src/main.rs              Entry point, clap dispatch, error formatting
src/cli/mod.rs           Cli struct, Commands enum, setup_domain() helper
src/cli/*.rs             Command handlers (one per subcommand group)
src/client/mod.rs        local_name() helper, re-exports
src/client/soap_client.rs  SoapEnvelope builder + SoapClient (HTTP transport, XML parsing)
src/client/*.rs          Service-specific clients wrapping SoapClient
src/config.rs            TOML config (~/.config/yuki/config.toml)
src/error.rs             YukiError enum with exit codes (0/1/2/3/4)
src/output.rs            TTY-aware table (comfy-table) / JSON output
src/period.rs            Period string → (start_date, end_date) conversion
src/money.rs             Exact decimal amounts (Cents) for invoice totals
src/ledger.rs            Ledger<S: LedgerFormat>: local JSON record, OS lock, versioned load, atomic write, write-ahead
src/sync.rs              upload dir's .yuki-sync.json (a Ledger) + scan/excludes/hashing
src/cli/sales_invoice.rs   sales invoice create/prepare: TOML → xmlDoc for ProcessSalesInvoices
src/cli/invoice_ledger.rs  invoice-numbers.json (a Ledger), locked only per read-write
```

Local records go through `ledger::Ledger`: `open` (lock + load), `peek` (read only), `save`, and `write_ahead` → `commit`/`undo`. Record a call as pending before making it. A failed call is classified by `YukiError::delivery()`: `NotSent` (never left) and `Refused` (401/403/429) may be retried or freed; `Unknown` (timeout, fault, 5xx) stays pending until the user resolves it. Requests time out after 15s connect / 60s total; one carrying a file uses `call_with_timeout` with `payload_timeout` (60s + 30s/MB, max 10 min).

### SOAP Client Pattern

Each service client (accounting, archive, vat, contact, sales) wraps `SoapClient` with a service URL `{api_root}/{Service}.asmx`. Regions are list-driven: `Region::ALL` plus the one `Region::info()` table (code, country, host) in `yuki-client`; nothing else may hardcode an nl/be pair. `yuki init` detects a key's region by authenticating on every `Region::ALL` root concurrently (`cli::init::detect`; "Invalid access key" = not this region, any other error is reported) and records it; `--region`/`--base-url` skip detection. CLI handlers pass `target.api_root` via `.with_api_root()`, resolved by `Config::endpoint()` (override flag/env > admin `base_url` > admin `region` > top-level `base_url` > top-level `region` > nl, the last only as a legacy fallback for configs written before init recorded the region), which also yields the region for country conventions; `api_root()`/`region()` are its shorthands. Tests point detection at a local mock with the hidden `YUKI_PROBE_ROOTS=code=root,...`. The flow is always: `authenticate()` → `set_current_domain()` → operation calls. The `setup_domain()` helper in `cli/mod.rs` handles the first two steps.

`SoapEnvelope` is a builder: `.new("Op").session(sid).param("key", "val").build()` produces the XML envelope. All operations require `administrationID` as a parameter.

### Output Convention

Output is TTY-aware: tables for humans, JSON when piped. The `OutputFormat::from_flag()` method handles this. All command handlers follow the same pattern: build `headers` + `rows` vectors, then format with `format_table`/`format_json`.

## Yuki API Quirks

These are hard-won lessons from the actual API behavior:

- SOAP namespace: `http://www.theyukicompany.com/`
- SOAP faults return as HTTP 500 — must parse XML body, not just status code
- `SetCurrentDomain` needs `domainID` (not `administrationID`)
- Almost all operations need `administrationID` as a parameter
- Administration `ID` is an XML **attribute**, not a child element
- VAT operations use exact casing: `VATReturnList`, `ActiveVATCodesList`
- Real XML field names often differ from what you'd guess (lowercase, different names)
- `paymentMethod` must be int (`0` = unspecified), not empty string
- `SearchDocuments`: `folderID=-1, tabID=-1` means "all folders"
- Amount uses dot decimal separator (`695.74`), not comma

## Config

Stored at `~/.config/yuki/config.toml`. Each administration has both a `domain_id` (for `SetCurrentDomain`) and an `admin_id` (for operation parameters) — they are different UUIDs.
