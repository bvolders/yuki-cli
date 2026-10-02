# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Breaking Changes

- **accounts**: `accounts balance` now reports the balance at the end of `--period` (or today, if the period is still running) instead of at its start, and adds an `As Of` column with that date. Scripts reading the old start-of-period figure, or indexing columns, must adjust.
- **yuki-client**: 0.3.0. `AccountingInfoClient::get_transaction_details` now takes `(administration_id, gl_account_code, start_date, end_date)` instead of a transaction ID, because `GetTransactionDetails` has no transaction-ID parameter; `TransactionDetail` gains `contact_name`.

### Added

- **sales**: `sales invoice create` creates a sales invoice through `ProcessSalesInvoices`, from a TOML file (`--file`) or a saved per-customer template (`--template`, in `~/.config/yuki/invoices/`). It is a draft in "To be sent" by default; `--send email|peppol|both` books and sends it. A preview (customer, lines, net, VAT, gross, mode) is confirmed at a prompt, `--yes` is required off a terminal, and `--dry-run` prints the preview and the `xmlDoc` without any API call. Exits 1 with kind `invoice_rejected` when Yuki fails or skips the invoice. `sales invoice templates` lists the templates, each validated.
- **yuki-client**: `SalesClient::process_sales_invoices` and `SalesInvoicesImport`, which read the import response whether Yuki sends it as elements or as escaped text; `SoapEnvelope::param_xml` for raw-XML (`s:any`) parameters.

### Fixed

- **sales**: `sales items` now honours `--region`, `--base-url` and their environment variables, like every other command.

## [0.1.13](https://github.com/rvben/yuki-cli/compare/v0.1.12...v0.1.13) - 2026-09-28

### Fixed

- **deps**: update quick-xml to 0.42 for RUSTSEC-2026-0194 and RUSTSEC-2026-0195 ([39dd114](https://github.com/rvben/yuki-cli/commit/39dd114dc3775bcf1874d4bfef09c41479af8a02))
- **deps**: update rustls to 0.23.45 for RUSTSEC-2026-0285 ([766d56d](https://github.com/rvben/yuki-cli/commit/766d56d23ab0c8904c67670d9eb2f32849580c20))

## [0.1.12](https://github.com/rvben/yuki-cli/compare/v0.1.11...v0.1.12) - 2026-09-03

### Added

- **auth**: standardize authentication workflow ([61d85fb](https://github.com/rvben/yuki-cli/commit/61d85fb2d0d1eefe9a699509187f4200cce5ec13))
- reach administrations that need their own access key ([115e95d](https://github.com/rvben/yuki-cli/commit/115e95d5e139e7c8dbd1cd2f3f117084a43447ef))

## [0.1.11](https://github.com/rvben/yuki-cli/compare/v0.1.10...v0.1.11) - 2026-08-26

### Added

- **packaging**: add package-named launcher ([61dc1d9](https://github.com/rvben/yuki-cli/commit/61dc1d94e0baf76df8b6dac5eb14e18a416243a6))

### Fixed

- **release**: use package version in dry runs ([5961da1](https://github.com/rvben/yuki-cli/commit/5961da15854f1fc91c68f1b2c47f9c3c2f185d6a))
- **ci**: install pinned Rust components ([608925e](https://github.com/rvben/yuki-cli/commit/608925ed92833a9593e07e35796afb8208c0d53b))
- **release**: scope assets to the current tag ([5625350](https://github.com/rvben/yuki-cli/commit/5625350b657acee8a14a440ffabea6bfd11c3feb))






## [0.1.8](https://github.com/rvben/yuki-cli/compare/v0.1.7...v0.1.8) - 2026-07-09

### Fixed

- **cli**: validate list filters and return complete result sets ([beaadb2](https://github.com/rvben/yuki-cli/commit/beaadb26f947228e169c2996ab463962c11589c3))

## [0.1.7](https://github.com/rvben/yuki-cli/compare/v0.1.6...v0.1.7) - 2026-06-20

### Fixed

- **schema**: correct exit-code declarations ([e53abec](https://github.com/rvben/yuki-cli/commit/e53abec7184e46e5edb7e059636eb1c8656be917))

## [0.1.6](https://github.com/rvben/yuki-cli/compare/v0.1.5...v0.1.6) - 2026-06-11

### Added

- add CLI Spec v0.2 compliance ([70a849a](https://github.com/rvben/yuki-cli/commit/70a849a65ce68110ef186144c6c00434d2fbf893))

### Fixed

- **client**: correct GL account balance, scheme, and start-balance parsing ([9e5ff99](https://github.com/rvben/yuki-cli/commit/9e5ff99b1ef9c6ac8d59c5bfa12b3cd9652e18bc))

## [0.1.5](https://github.com/rvben/yuki-cli/compare/v0.1.4...v0.1.5) - 2026-04-10

## [0.1.4](https://github.com/rvben/yuki-cli/compare/v0.1.3...v0.1.4) - 2026-04-03

### Added

- **init**: add API key guidance, colored status, and next steps ([4a00b76](https://github.com/rvben/yuki-cli/commit/4a00b769c2529284f9b0d3a1d969873ed5d815fc))

## [0.1.3](https://github.com/rvben/yuki-cli/compare/v0.1.2...v0.1.3) - 2026-04-03

## [0.1.2](https://github.com/rvben/yuki-cli/compare/v0.1.1...v0.1.2) - 2026-04-03

### Added

- add schema, completions, and colored output ([68de24c](https://github.com/rvben/yuki-cli/commit/68de24ce0b2af57d14161a518c4bdccb90ee8f1d))
