# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Breaking Changes

- **accounts**: `accounts balance` now reports the balance at the end of `--period` (or today, if the period is still running) instead of at its start, and adds an `As Of` column with that date. Scripts reading the old start-of-period figure, or indexing columns, must adjust.
- **yuki-client**: HTTP 401/403 is now `YukiError::Unauthorized(status)` instead of `AuthFailed("HTTP …")`; `AuthFailed` remains for authentication faults.
- **contacts**: `contacts search` shows `HID`, `Code`, `City` and `VAT Number` columns (the fields an invoice template takes) between the existing ones; scripts indexing columns must adjust.
- **invoices**: `invoices document` saves the linked file (under its own name, or `--out`) instead of printing its name and base64 glued into one column; its output is now `Transaction`, `Path`, `Bytes`.
- **yuki-client**: `AccountingInfoClient::get_transaction_document` returns a parsed `TransactionDocument { file_name, data_base64 }` instead of the raw response.
- **yuki-client**: `ContactClient::search_contacts` takes `(domain_id, option, value)`, `option` being one of the new `contact::SEARCH_OPTIONS`, and `get_suppliers_and_customers(_page)` takes `domain_id` first; `Contact` gains `code`, `hid`, `city` and `vat_number` and derives `Default`.
- **yuki-client**: 0.3.0. `AccountingInfoClient::get_transaction_details` now takes `(administration_id, gl_account_code, start_date, end_date)` instead of a transaction ID, because `GetTransactionDetails` has no transaction-ID parameter; `TransactionDetail` gains `contact_name`.

### Added

- **upload**: `upload dir <path>` uploads the receipts in a directory that are not in Yuki yet, tracked by content hash in `<path>/.yuki-sync.json`. Each upload is recorded as `pending` before it is sent, so an uncertain outcome is never retried silently. `--seed-from-yuki` records files Yuki already has; `upload mark` records one by hand.
- **sales**: `sales invoice create` creates a sales invoice through `ProcessSalesInvoices`, from a TOML file (`--file`) or a saved per-customer template (`--template`, in `~/.config/yuki/invoices/`). It is a draft in "To be sent" by default; `--send email|peppol|both` books and sends it. A preview (customer, lines, net, VAT, gross, mode) is confirmed at a prompt, `--yes` is required off a terminal, and `--dry-run` prints the preview and the `xmlDoc` without any API call. Exits 1 with kind `invoice_rejected` when Yuki fails or skips the invoice. `sales invoice templates` lists the templates, each validated. `pdf = "..."` in the file, or `--pdf`, sends your own PDF as `DocumentFileName`/`DocumentBase64`, which Yuki stores instead of its generated invoice (checked for the `%PDF-` header, at most 3 MB for Yuki's request limit, `.pdf` appended to a name without it; not allowed in a template, which is reused every month; the dry run shows its size instead of the base64, and the result names it in a `PDF` column). A request that went out without a usable answer exits 1 with kind `outcome_unknown`, warning that the invoice may already exist in Yuki.
- **sales**: `sales invoice create --book` books without sending. `--number <REF>` sets the invoice number (`Reference`), and `--number auto` takes one past the highest `<year>-<seq>` in the sales archive's file names for the invoice date's year; a number the archive has is refused. A custom PDF now needs `--send` or `--book`, since Yuki rejects one on a draft ("When supplying a document process must be set to true"), and `--number`, the number printed on it. `sales invoice prepare` prints the fully resolved invoice as JSON (number, ISO and Dutch dates, customer, lines, totals per rate as the CLI computes them, Belgian structured payment reference) for rendering a PDF; it ignores any `pdf`. `--number` needs `--send` or `--book`; with `--pdf` it must be the explicit number `prepare` printed, with `--date`, and Yuki stores the PDF as `Invoice <number>.pdf`. Numbers given out are kept in a local ledger (`invoice-numbers.json`, written atomically under a lock): pending before the call, then booked or rejected; one left pending by an unknown outcome stays taken until `sales invoice numbers --resolve <n> booked|rejected`. `auto` takes one past the highest of the archive (only `Invoice`/`Factuur <year>-<seq>.pdf`, read for the invoice year after SetCurrentDomain) and the ledger. Lines take `remarks` (sent as `InvoiceLine/Remarks`) and `unit` (prepare only). The preview warns when VAT rounded per line would differ, and `--quiet --yes` still prints a one-line booking notice.
- **documents**: `documents download <id> [--out PATH]` saves an archive document's file (`DocumentBinaryData`), named after its archive file name (`FindDocument`) unless `--out` names a file; it never overwrites. `ArchiveClient::find_document` and `document_binary_data` back it.
- **sales**: `sales invoice prepare --number auto|N --out <file.json>` writes the prepared invoice and reserves its number in the ledger for exactly that content (status `reserved`, with its content hash and administration), so preparing two invoices gives two numbers. `sales invoice create --prepared <file.json> [--pdf <pdf>] --send …|--book` books exactly that content, refused alongside `--file`/`--template`/`--qty`/`--price`/`--date`/`--subject`/`--number`, or when the reservation is gone or was for other content; the number goes reserved → pending → booked. `--pdf` needs `--prepared`, and a `pdf` key in an invoice file is refused. `sales invoice numbers --release <n>` frees a reservation (as does `--resolve <n> rejected`). The ledger is format version 2 and reads version 1.
- **sales**: a booking with `--number` checks the `Reference` Yuki returns; a different or missing one exits 1 with kind `reference_mismatch` (on stderr even with `--quiet`) and leaves the number pending with a note naming both.
- **sales**: an invoice Yuki booked but did not email exits 1 with the new kind `send_incomplete` (the number is booked, with a note) instead of `invoice_rejected`. The result's new `Peppol` column, and the preview, say `requested (Yuki does not report delivery)`: Peppol delivery is never claimed.
- **sales**: booking with `--yes` (no prompt) needs `--confirm <number>` equal to the invoice number, so a booking Yuki numbers itself is confirmed at the prompt only; the prompt names the number.
- **sales**: placeholders in the subject and a line's description and remarks: `{month}` (Dutch month name), `{year}`, `{month_num}` and `{pct_of_net:25}` (25% of the line's net, Belgian notation); an unknown one is an error.
- **sales**: `[seller]` in the config is emitted as `firm` by `prepare` (required for `--out` and `--prepared`); `vat_mention` passes through to the JSON, with a preview warning for a 0% line without one; a booking needs a due date.
- **sales**: the invoice ledger keeps each administration's numbers apart (`admin_id`) and compares them like the archive check (`2026-01` is `2026-1`); it waits for its lock instead of refusing. `--number auto` reads the year's sales folder strictly (pages until an empty one; a repeated document is an error).
- **yuki-client**: `SalesClient::process_sales_invoices` and `SalesInvoicesImport`, which read the import response whether Yuki sends it as elements or as escaped text; `SoapEnvelope::param_xml` for raw-XML (`s:any`) parameters.

### Fixed

- **yuki-client**: `SoapEnvelope::param` XML-escapes its value (`escape_text`). Requests time out (connect 15s, total 60s; uploads longer by size), `YukiError::delivery()` says whether a failed request may have been processed, and `ArchiveClient::documents_in_folder_all` reads a whole folder strictly.
- **contacts**: `contacts search` returned every contact whatever the query, because it sent a `searchQuery` parameter `SearchContacts` does not have. It now sends the schema's `searchOption`, `searchValue`, `sortOrder`, `modifiedAfter` (as `xsi:nil`), `active` and `pageNumber`, searches all fields by default or the one `--by` names, includes inactive contacts, and follows pagination.
- **contacts**: `contacts search` and `contacts list` ignored `--admin` and read the key's default administration; they now send its `domainID`. Both listings stop at a page whose first contact repeats, and after 50 pages (5000 contacts) with a warning, so a listing always ends. `SoapEnvelope::nil_param` sends such nillable parameters.
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
