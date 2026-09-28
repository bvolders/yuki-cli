//! The matching engine behind `yuki check unmatched`: which bank debits have
//! no purchase invoice, from ledgers, open items and the archive.

use std::collections::{HashMap, HashSet};

use super::resolve_period;
use crate::cli::setup_domain;
use crate::client::Region;
use crate::client::accounting::{GlTransactionWithContact, OutstandingItem};
use crate::client::archive::{ArchiveClient, ArchiveDocument};
use crate::config::Config;
use crate::error::YukiError;
use crate::output::{OutputFormat, format_json, format_table, is_tty};

/// Segments of a Belgian CODA description that carry payment details, not a name.
const CODA_DETAIL_PREFIXES: &[&str] = &[
    "netto bedrag",
    "klantreferentie",
    "europese domiciliëring",
    "debet atm/pos",
    "afloss.",
    "betaalde rente",
    "vast voorschot",
];

/// Extract the counterparty name from a bank transaction description.
///
/// Dutch SEPA descriptions encode it as `/CNTP/<iban>/<bic>/<name>/`. Belgian
/// CODA descriptions are ` | `-separated segments, `<type> : <subtype> | Netto
/// bedrag: ... | ... | <name>`, with the name (when the bank sends one) last.
/// Card payments and direct debits often carry no name at all, in which case
/// the CODA transaction type is returned so the row still says what it is.
///
/// Falls back to the first 50 characters of the description otherwise.
fn parse_counterparty(description: &str) -> String {
    if let Some(cntp_start) = description.find("/CNTP/") {
        let after_cntp = &description[cntp_start + 6..];
        let parts: Vec<&str> = after_cntp.splitn(4, '/').collect();
        if parts.len() >= 3 {
            return parts[2].trim().to_string();
        }
    }
    if let Some(name) = parse_coda_counterparty(description) {
        return name;
    }
    description
        .chars()
        .take(50)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Counterparty from a Belgian CODA description, or `None` when it is not one.
fn parse_coda_counterparty(description: &str) -> Option<String> {
    if !description.contains("Netto bedrag:") {
        return None;
    }
    let segments: Vec<&str> = description
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let (kind, rest) = segments.split_first()?;
    let name = rest.iter().skip(1).rev().find(|seg| {
        let lower = seg.to_lowercase();
        !CODA_DETAIL_PREFIXES.iter().any(|p| lower.starts_with(p))
    });
    let name = name.map_or(*kind, |n| n);
    // Squeeze the fixed-width padding CODA uses inside name/address fields.
    Some(name.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Whether an API contact value is a GL account code rather than a name.
///
/// Yuki fills `Contact` with the GL code when a bank line was booked straight to
/// a ledger account (e.g. by a bank rule), so the value is no counterparty.
fn is_gl_code(contact: &str) -> bool {
    !contact.is_empty() && contact.chars().all(|c| c.is_ascii_digit())
}

/// Description patterns skipped by default for Belgian administrations: bank
/// movements that never come with a purchase invoice.
const BE_IGNORE_DESCRIPTIONS: &[&str] = &[
    "Lening op korte termijn",
    "Afloss. kapitaal",
    "Vast voorschot kredieten",
    "Kosten i.v.m. kredieten",
    "Afrekening kredietkaarten",
    "Betaling lonen",
    "BTW ONTVANGSTEN",
    "FOD FINANCI",
];

/// GL prefixes skipped by default for Belgian administrations when a bank line
/// is booked straight to them: class 65, financial charges (interest, bank
/// costs 657xxx, exchange differences), which never come with an invoice.
const BE_NO_DOCUMENT_ACCOUNTS: &[&str] = &["65"];

/// Matching rules for `check unmatched`, from region defaults and config.
#[derive(Debug, Clone, Default)]
struct UnmatchedRules {
    /// Counterparty substrings to skip (`unmatched_ignore`).
    ignore_counterparties: Vec<String>,
    /// Full-description substrings to skip.
    ignore_descriptions: Vec<String>,
    /// GL account prefixes that never carry a document. A bank line booked
    /// straight to one (its API contact is that GL code) is skipped; a line
    /// booked straight to any other account is still reported.
    no_document_accounts: Vec<String>,
    /// Names of the administration itself; a payment to one is an own transfer.
    own_names: Vec<String>,
}

/// Region defaults and per-administration overrides for `check unmatched`.
struct UnmatchedSetup {
    bank_accounts: Vec<String>,
    creditor_accounts: Vec<String>,
    transfer_accounts: Vec<String>,
    rules: UnmatchedRules,
}

impl UnmatchedSetup {
    fn resolve(config: &Config, admin_name: &str, bank_flag: &[String]) -> Self {
        let entry = config.administrations.get(admin_name);
        let be = config.region(entry) == Region::Be;
        // Region default: Belgium has a fixed chart (PCMN/MAR); the Dutch
        // default keeps the historic behaviour.
        let region_default = |be_value: &[&str]| -> Vec<String> {
            if be {
                be_value.iter().map(|s| (*s).to_string()).collect()
            } else {
                Vec::new()
            }
        };

        let bank_accounts = if !bank_flag.is_empty() {
            bank_flag.to_vec()
        } else if let Some(e) = entry.filter(|e| !e.bank_accounts.is_empty()) {
            e.bank_accounts.clone()
        } else if be {
            vec!["550000".to_string()]
        } else {
            vec!["11001".to_string()]
        };

        let own_names = entry
            .and_then(|e| e.name.as_deref())
            .map(normalize_name)
            .filter(|n| be && !n.is_empty())
            .into_iter()
            .collect();

        Self {
            bank_accounts,
            creditor_accounts: entry
                .and_then(|e| e.creditor_accounts.clone())
                .unwrap_or_else(|| region_default(&["440000"])),
            transfer_accounts: entry
                .and_then(|e| e.transfer_accounts.clone())
                .unwrap_or_else(|| region_default(&["580000"])),
            rules: UnmatchedRules {
                ignore_counterparties: config.unmatched_ignore.clone(),
                ignore_descriptions: entry
                    .and_then(|e| e.unmatched_ignore_descriptions.clone())
                    .unwrap_or_else(|| region_default(BE_IGNORE_DESCRIPTIONS)),
                no_document_accounts: entry
                    .and_then(|e| e.no_document_accounts.clone())
                    .unwrap_or_else(|| region_default(BE_NO_DOCUMENT_ACCOUNTS)),
                own_names,
            },
        }
    }
}

/// A bank debit without a matching invoice.
#[derive(Debug, Clone, PartialEq)]
struct UnmatchedDebit {
    bank_account: String,
    id: String,
    date: String,
    amount: String,
    counterparty: String,
    description: String,
}

/// Canonical cent string of an amount, e.g. `"-7.3"` -> `"-7.30"`.
fn cents(amount: &str) -> Option<String> {
    amount.trim().parse::<f64>().ok().map(|a| format!("{a:.2}"))
}

/// Key under which a bank debit and its ledger counter-entry meet.
fn entry_key(date: &str, abs_amount: &str) -> String {
    format!("{date}|{abs_amount}")
}

/// A multiset of keys that each match at most once.
#[derive(Default)]
struct Pool(HashMap<String, usize>);

impl Pool {
    fn add(&mut self, key: String) {
        *self.0.entry(key).or_insert(0) += 1;
    }

    fn take(&mut self, key: &str) -> bool {
        match self.0.get_mut(key) {
            Some(c) if *c > 0 => {
                *c -= 1;
                true
            }
            _ => false,
        }
    }
}

/// `TransactionType` of a purchase invoice or credit note on the creditors
/// account (Yuki Belgium). Bank and card lines carry other types (`0`, `10`).
const PURCHASE_TRANSACTION_TYPE: &str = "9";

/// What a line on the creditors control account is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreditorLine {
    /// A purchase invoice: a credit, adds to what the supplier is owed.
    Invoice,
    /// A purchase credit note: a debit that reduces what is owed; no bank line.
    CreditNote,
    /// A payment to the supplier: a debit mirrored by a bank debit.
    Payment,
    /// Money back from the supplier: a credit with no document behind it.
    Refund,
}

impl CreditorLine {
    /// Classify by journal type when the response carries one, else by sign
    /// alone (credit = invoice, debit = payment).
    ///
    /// A bank-type credit linked to a document (`FileName`) is the payment
    /// difference Yuki books when a payment settles an invoice for slightly
    /// more (rounding, exchange rate), so it counts with the invoice rather
    /// than as a refund. Bank-type debits stay payments even with a document:
    /// Yuki links real payments (type `10`, "Betaling: Factuur van ...") too.
    fn of(tx: &GlTransactionWithContact, amount: f64) -> Self {
        let credit = amount < 0.0;
        let document = !tx.file_name.trim().is_empty();
        match tx.transaction_type.trim() {
            "" if credit => Self::Invoice,
            "" => Self::Payment,
            PURCHASE_TRANSACTION_TYPE if credit => Self::Invoice,
            PURCHASE_TRANSACTION_TYPE => Self::CreditNote,
            _ if credit && document => Self::Invoice,
            _ if credit => Self::Refund,
            _ => Self::Payment,
        }
    }
}

/// A supplier payment on the creditors account.
#[derive(Debug, Clone, PartialEq)]
struct LedgerPayment {
    contact: String,
    /// Whether that supplier's invoices cover it.
    covered: bool,
}

/// What the creditors control account says about bank payments.
#[derive(Default)]
struct CreditorLedger {
    /// Supplier payments by date and amount ([`entry_key`]). Several payments
    /// can share a key, so every one is kept.
    payments: HashMap<String, Vec<LedgerPayment>>,
}

impl CreditorLedger {
    /// Claim the ledger payment mirroring a bank debit with `key`, whose
    /// counterparty according to the bank is `hint` (may be empty).
    ///
    /// The bank line and its ledger line share no identifier, only date and
    /// amount, so among equal candidates this prefers the supplier the bank
    /// names, then a covered payment (leaving the uncovered one for another
    /// bank line), then the first in supplier-name order.
    fn claim(&mut self, key: &str, hint: &str) -> Option<LedgerPayment> {
        let list = self.payments.get_mut(key)?;
        let index = list
            .iter()
            .position(|p| !hint.is_empty() && names_match(hint, &p.contact))
            .or_else(|| list.iter().position(|p| p.covered))
            .or(if list.is_empty() { None } else { Some(0) })?;
        Some(list.remove(index))
    }
}

/// Take up to `amount` from `invoices`, oldest first.
fn consume_fifo(invoices: &mut Vec<(&str, f64)>, amount: f64) {
    let mut due = amount;
    invoices.retain_mut(|(_, open)| {
        if due <= 0.005 {
            return true;
        }
        let used = open.min(due);
        due -= used;
        *open -= used;
        *open > 0.005
    });
}

/// Match supplier payments on the creditors account to that supplier's invoices.
///
/// On the creditors account an invoice is a credit and a payment a debit, each
/// carrying the supplier as contact. Credit notes (debits from the purchase
/// journal) first reduce the supplier's invoices; refunds (credits from the
/// bank) are no invoices and are ignored ([`CreditorLine`]).
///
/// Payments are taken in date order. Each is covered by an open invoice of
/// the same supplier with the same amount, or else by what is left of that
/// supplier's invoices (batched or partial payments), consumed oldest first. Payments before `period_start`
/// consume invoices the same way, so an invoice paid before the period cannot
/// also cover a payment inside it. `entries` should reach back before the
/// period so that invoices paid later are seen.
fn match_creditor_ledger(
    entries: &[GlTransactionWithContact],
    period_start: &str,
) -> CreditorLedger {
    let mut invoices: HashMap<&str, Vec<(&str, f64)>> = HashMap::new();
    let mut credit_notes: Vec<(&str, f64)> = Vec::new();
    let mut payments: Vec<(&GlTransactionWithContact, f64)> = Vec::new();
    for tx in entries {
        let Ok(amount) = tx.amount.trim().parse::<f64>() else {
            continue;
        };
        let contact = tx.contact_name.as_str();
        if contact.is_empty() || is_gl_code(contact) || amount == 0.0 {
            continue;
        }
        match CreditorLine::of(tx, amount) {
            CreditorLine::Invoice => invoices
                .entry(contact)
                .or_default()
                .push((tx.date.as_str(), -amount)),
            CreditorLine::CreditNote => credit_notes.push((contact, amount)),
            CreditorLine::Payment => payments.push((tx, amount)),
            CreditorLine::Refund => {}
        }
    }
    for list in invoices.values_mut() {
        list.sort_by(|a, b| a.0.cmp(b.0));
    }

    let same = |a: f64, b: f64| (a - b).abs() < 0.005;
    for (contact, amount) in credit_notes {
        let list = invoices.entry(contact).or_default();
        match list.iter().position(|(_, inv)| same(*inv, amount)) {
            Some(i) => {
                list.remove(i);
            }
            None => consume_fifo(list, amount),
        }
    }

    // One pass in date order, so every payment sees only the invoices that
    // earlier payments left open.
    payments.sort_by(|a, b| a.0.date.cmp(&b.0.date));
    let mut covered = vec![false; payments.len()];
    for (i, (tx, amount)) in payments.iter().enumerate() {
        let list = invoices.entry(tx.contact_name.as_str()).or_default();
        if let Some(j) = list.iter().position(|(_, inv)| same(*inv, *amount)) {
            list.remove(j);
            covered[i] = true;
            continue;
        }
        let left: f64 = list.iter().map(|(_, inv)| inv).sum();
        if tx.date.as_str() < period_start {
            // Outside the period nothing is reported; it only uses up invoices.
            consume_fifo(list, *amount);
        } else if left + 0.005 >= *amount {
            consume_fifo(list, *amount);
            covered[i] = true;
        }
    }

    let mut ledger = CreditorLedger::default();
    for ((tx, amount), covered) in payments.into_iter().zip(covered) {
        ledger
            .payments
            .entry(entry_key(&tx.date, &format!("{amount:.2}")))
            .or_default()
            .push(LedgerPayment {
                contact: tx.contact_name.clone(),
                covered,
            });
    }
    for list in ledger.payments.values_mut() {
        list.sort_by(|a, b| a.contact.cmp(&b.contact));
    }
    ledger
}

/// First day of the month three months before `date` (`YYYY-MM-DD`), so the
/// creditors account is read far enough back to see invoices paid later.
fn lookback_start(date: &str) -> String {
    let year: i32 = date.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(1970);
    let month: i32 = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
    let total = year * 12 + (month - 1) - 3;
    format!("{:04}-{:02}-01", total / 12, total % 12 + 1)
}

/// Cross-reference bank debits against ledgers, open items and the archive.
///
/// A debit counts as matched when, in order:
/// 1. the creditors account shows it paid a supplier invoice
///    ([`match_creditor_ledger`]);
/// 2. a transfer account or another scanned bank account holds the opposite
///    entry on the same date (an own transfer);
/// 3. its description matches an ignore pattern, it was booked straight to a
///    GL account, or it pays the administration itself;
/// 4. its amount equals an outstanding creditor item or a booked archive document;
/// 5. its counterparty is ignored or, when the creditors account does not know
///    it, appears among archived documents.
fn find_unmatched(
    banks: &[(String, Vec<GlTransactionWithContact>)],
    mut ledger: CreditorLedger,
    transfer_entries: &[GlTransactionWithContact],
    creditor_items: &[OutstandingItem],
    archive_docs: &[ArchiveDocument],
    rules: &UnmatchedRules,
) -> Vec<UnmatchedDebit> {
    let mut transfers = Pool::default();
    for tx in transfer_entries {
        if let Some(a) = cents(&tx.amount).filter(|a| !a.starts_with('-')) {
            transfers.add(entry_key(&tx.date, &a));
        }
    }
    // Credits on the other scanned bank accounts are own transfers too.
    let mut bank_credits = Pool::default();
    for (account, txs) in banks {
        for tx in txs {
            if let Some(a) = cents(&tx.amount).filter(|a| !a.starts_with('-') && a != "0.00") {
                bank_credits.add(format!("{account}|{}", entry_key(&tx.date, &a)));
            }
        }
    }
    let mut creditor_pool = Pool::default();
    for item in creditor_items {
        creditor_pool.add(item.open_amount.trim().to_string());
    }
    let mut archive_pool = Pool::default();
    for doc in archive_docs {
        if let Ok(amt) = doc.amount.trim().parse::<f64>()
            && amt > 0.0
        {
            archive_pool.add(format!("{amt:.2}"));
        }
    }
    // Normalized archive contact names catch batched or split charges where the
    // amount differs but the supplier is known.
    let archive_names: HashSet<String> = archive_docs
        .iter()
        .filter(|d| !d.contact_name.is_empty())
        .map(|d| normalize_name(&d.contact_name))
        .filter(|n| !n.is_empty())
        .collect();
    let ignore_desc: Vec<String> = rules
        .ignore_descriptions
        .iter()
        .map(|p| p.to_lowercase())
        .collect();

    let mut unmatched = Vec::new();
    for (account, txs) in banks {
        for tx in txs {
            let amount: f64 = tx.amount.trim().parse().unwrap_or(0.0);
            if amount >= 0.0 {
                // Only debits (payments out) are relevant.
                continue;
            }
            let abs_amount = format!("{:.2}", amount.abs());
            let key = entry_key(&tx.date, &abs_amount);

            // The supplier the bank names, to pick among equal ledger payments.
            let hint = if !tx.contact_name.is_empty() && !is_gl_code(&tx.contact_name) {
                tx.contact_name.clone()
            } else {
                parse_counterparty(&tx.description)
            };
            let ledger_payment = ledger.claim(&key, &hint);
            if ledger_payment.as_ref().is_some_and(|p| p.covered) || transfers.take(&key) {
                continue;
            }
            let own_transfer = banks
                .iter()
                .any(|(other, _)| other != account && bank_credits.take(&format!("{other}|{key}")));
            if own_transfer {
                continue;
            }

            let desc_lower = tx.description.to_lowercase();
            if ignore_desc.iter().any(|p| desc_lower.contains(p.as_str())) {
                continue;
            }
            if is_gl_code(&tx.contact_name)
                && rules
                    .no_document_accounts
                    .iter()
                    .any(|prefix| tx.contact_name.starts_with(prefix.as_str()))
            {
                continue;
            }

            if creditor_pool.take(&abs_amount) || archive_pool.take(&abs_amount) {
                continue;
            }

            // Prefer the supplier from the creditors account, then the API
            // contact when it is a name, then the description.
            let ledger_contact = ledger_payment.map(|p| p.contact);
            let known_to_ledger = ledger_contact.is_some();
            let counterparty = match ledger_contact {
                Some(name) => name,
                None if !tx.contact_name.is_empty() && !is_gl_code(&tx.contact_name) => {
                    tx.contact_name.clone()
                }
                None => parse_counterparty(&tx.description),
            };

            let cp_lower = counterparty.to_lowercase();
            if rules
                .ignore_counterparties
                .iter()
                .any(|pat| cp_lower.contains(&pat.to_lowercase()))
            {
                continue;
            }
            if rules.own_names.contains(&normalize_name(&counterparty)) {
                continue;
            }
            // The creditors account already compared this supplier's invoices.
            if !known_to_ledger && archive_names.iter().any(|n| names_match(&counterparty, n)) {
                continue;
            }

            unmatched.push(UnmatchedDebit {
                bank_account: account.clone(),
                id: tx.id.clone(),
                date: tx.date.clone(),
                amount: format!("-{abs_amount}"),
                counterparty,
                description: tx.description.clone(),
            });
        }
    }
    unmatched
}

/// Find bank transactions on the bank GL account(s) that have no matching invoice.
///
/// See [`find_unmatched`] for the matching order. Bank, creditor and transfer
/// accounts and the ignore patterns come from the administration's config,
/// falling back to region defaults (see [`UnmatchedSetup::resolve`]).
pub async fn unmatched(
    config: &Config,
    admin: Option<&str>,
    period: Option<&str>,
    bank_accounts: &[String],
    format: Option<&str>,
    quiet: bool,
) -> Result<(), YukiError> {
    let (start, end) = resolve_period(period)?;
    let (accounting_client, target) = setup_domain(config, admin).await?;
    let setup = UnmatchedSetup::resolve(config, target.config_name, bank_accounts);
    let mut calls = 2;

    let mut banks = Vec::new();
    for account in &setup.bank_accounts {
        if !quiet {
            eprintln!("Fetching bank transactions (GL {account})...");
        }
        let txs = accounting_client
            .gl_account_transactions_and_contact(target.admin_id, account, &start, &end)
            .await?;
        calls += 1;
        banks.push((account.clone(), txs));
    }

    let lookback = lookback_start(&start);
    let mut creditor_entries = Vec::new();
    for account in &setup.creditor_accounts {
        if !quiet {
            eprintln!("Fetching supplier ledger (GL {account}, from {lookback})...");
        }
        creditor_entries.extend(
            accounting_client
                .gl_account_transactions_and_contact(target.admin_id, account, &lookback, &end)
                .await?,
        );
        calls += 1;
    }

    let mut transfer_entries = Vec::new();
    for account in &setup.transfer_accounts {
        if !quiet {
            eprintln!("Fetching internal transfers (GL {account})...");
        }
        transfer_entries.extend(
            accounting_client
                .gl_account_transactions_and_contact(target.admin_id, account, &start, &end)
                .await?,
        );
        calls += 1;
    }

    if !quiet {
        eprintln!("Fetching outstanding creditor items...");
    }
    let creditor_items = accounting_client
        .outstanding_creditor_items(target.admin_id)
        .await?;
    calls += 1;

    if !quiet {
        eprintln!("Fetching booked invoices from archive...");
    }
    let mut archive_client = ArchiveClient::new().with_api_root(target.api_root);
    archive_client.authenticate(target.api_key).await?;
    let archive_docs = archive_client.search_documents("", &start, &end).await?;
    calls += 2;

    if !quiet {
        eprintln!("API calls made: {calls}");
    }

    let found = find_unmatched(
        &banks,
        match_creditor_ledger(&creditor_entries, &start),
        &transfer_entries,
        &creditor_items,
        &archive_docs,
        &setup.rules,
    );

    let fmt = OutputFormat::from_flag(format, is_tty());
    let several = setup.bank_accounts.len() > 1;
    let mut headers: Vec<String> = Vec::new();
    if several {
        headers.push("Bank".into());
    }
    headers.extend(
        ["ID", "Date", "Amount", "Counterparty", "Description"]
            .iter()
            .map(|h| (*h).to_string()),
    );
    let rows: Vec<Vec<String>> = found
        .into_iter()
        .map(|u| {
            let description = match fmt {
                OutputFormat::Table => u.description.chars().take(80).collect(),
                OutputFormat::Json => u.description,
            };
            let mut row = Vec::new();
            if several {
                row.push(u.bank_account);
            }
            row.extend([u.id, u.date, u.amount, u.counterparty, description]);
            row
        })
        .collect();

    match fmt {
        OutputFormat::Table => println!("{}", format_table(&headers, &rows)),
        OutputFormat::Json => println!("{}", format_json(&headers, &rows)),
    }
    Ok(())
}

/// Legal-form suffixes dropped by [`normalize_name`], written without dots:
/// Dutch, German, English, and Belgian (Dutch and French forms).
const LEGAL_SUFFIXES: &[&str] = &[
    "bv", "gmbh", "inc", "ltd", "sa", "nv", "srl", "bvba", "sprl", "vzw", "asbl", "cvba", "scrl",
    "commv", "commva", "vof",
];

/// Normalize a company name for fuzzy matching.
///
/// Lowercases the name, removes "via ..." suffixes, treats `, ( ) - /` as word
/// breaks, drops dots, and removes legal-form words ("B.V.", "NV", "SRL",
/// "Comm.V", ...), including spaced spellings such as "B. V." or "Comm. V.".
/// Suffixes are removed as whole words only, so a name that merely contains
/// "sa" or "nv" is left intact.
///
/// A name made only of a legal form (or punctuation) never normalizes to
/// empty: it falls back to its dot-less form, then to the trimmed lowercase
/// original, so it still matches itself.
fn normalize_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    // Remove "via ..." suffix (e.g. "Vimexx via Mollie" -> "vimexx")
    let base = lower.split(" via ").next().unwrap_or(&lower);
    let cleaned = base
        .replace('.', "")
        .replace([',', '(', ')', '-', '/'], " ");
    let words: Vec<&str> = cleaned.split_whitespace().collect();

    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        // "B. V." and "Comm. V." arrive as two words once the dots are gone.
        if let Some(next) = words.get(i + 1)
            && LEGAL_SUFFIXES.contains(&format!("{}{next}", words[i]).as_str())
        {
            i += 2;
            continue;
        }
        if !LEGAL_SUFFIXES.contains(&words[i]) {
            kept.push(words[i]);
        }
        i += 1;
    }

    if !kept.is_empty() {
        return kept.join(" ");
    }
    if !words.is_empty() {
        return words.concat();
    }
    lower
}

/// Check whether two company names refer to the same entity.
///
/// Returns true if one normalized name contains the other, allowing for
/// abbreviations or partial matches.
fn names_match(bank_name: &str, archive_name: &str) -> bool {
    let a = normalize_name(bank_name);
    let b = normalize_name(archive_name);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.contains(b.as_str()) || b.contains(a.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_counterparty_extracts_cntp_name() {
        let desc = "/TRTP/SEPA/CNTP/NL01ABNA0001234567/ABNANL2A/Vimexx B.V./REMI/Hosting";
        assert_eq!(parse_counterparty(desc), "Vimexx B.V.");
    }

    #[test]
    fn parse_counterparty_falls_back_to_description() {
        assert_eq!(
            parse_counterparty("ING bankkosten maart"),
            "ING bankkosten maart"
        );
    }

    #[test]
    fn parse_counterparty_truncates_long() {
        let desc = "A".repeat(100);
        assert_eq!(parse_counterparty(&desc).len(), 50);
    }

    #[test]
    fn normalize_name_strips_legal_suffixes() {
        assert_eq!(normalize_name("Vimexx B.V."), "vimexx");
        assert_eq!(normalize_name("Hetzner GmbH"), "hetzner");
        assert_eq!(normalize_name("Amazon Inc"), "amazon");
    }

    #[test]
    fn normalize_name_removes_via_suffix() {
        assert_eq!(normalize_name("Vimexx via Mollie"), "vimexx");
    }

    #[test]
    fn normalize_name_handles_empty() {
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn names_match_bidirectional_substring() {
        assert!(names_match("Hetzner Online GmbH", "Hetzner"));
        assert!(names_match("Hetzner", "Hetzner Online GmbH"));
    }

    #[test]
    fn names_match_case_insensitive() {
        assert!(names_match("HETZNER", "hetzner online"));
    }

    #[test]
    fn names_match_strips_legal_suffixes() {
        assert!(names_match("Vimexx B.V.", "Vimexx via Mollie"));
    }

    #[test]
    fn names_match_rejects_empty() {
        assert!(!names_match("", "Hetzner"));
        assert!(!names_match("Hetzner", ""));
    }

    #[test]
    fn names_match_rejects_unrelated() {
        assert!(!names_match("Hetzner", "Amazon"));
    }

    // --- Belgian (CODA) fixtures. Names, amounts, IBANs and references are made up.

    const CODA_TRANSFER: &str = "Binnenlandse overschrijvingen - SEPA credit transfers : \
        Enkelvoudige overschrijving | Netto bedrag: 88,110 : Overschrijving of storting met \
        een gestructureerde mededeling: 000000000097 | EXAMPLE RECOVERY BV";
    const CODA_CARD: &str = "Kaarten : Betaling met debetkaart binnen eurozone | Netto \
        bedrag: 12,340 : | Debet ATM/POS - Gemaskeerde PAN of kaartnummer: 0000000000000000 - \
        Kaartschema: TEST - Terminalnummer: 000000 - Volgnummer verrichting: 000001 - Uur";
    const CODA_DIRECT_DEBIT: &str = "Domiciliëringen - Direct debit : Betaling | Netto bedrag: \
        61,500 : | Europese domiciliëring - Datum: 01-07-26 - Type domiciliëring: recurrent";
    const CODA_LOAN: &str = "Kredieten : Lening op korte termijn | Netto bedrag: 900,000 : \
        Kredieten - Lening op korte termijn (Netto bedrag) -- |\n| Afloss. kapitaal lening of \
        krediet: 880,000\n| Betaalde rente: 20,000 --";
    const CODA_SALARY: &str = "Binnenlandse overschrijvingen - SEPA credit transfers : \
        Betaling lonen, e.d. | Netto bedrag: 1000,000 : | Klantreferentie: 0000001/2026-07-28 |";

    fn tx(
        id: &str,
        date: &str,
        amount: &str,
        description: &str,
        contact: &str,
    ) -> GlTransactionWithContact {
        GlTransactionWithContact {
            id: id.into(),
            date: date.into(),
            description: description.into(),
            gl_account: String::new(),
            amount: amount.into(),
            contact_name: contact.into(),
            transaction_type: String::new(),
            file_name: String::new(),
        }
    }

    /// A creditors-account line with its journal type: "9" for a purchase
    /// document, "0" for a bank line.
    fn typed(kind: &str, mut t: GlTransactionWithContact) -> GlTransactionWithContact {
        t.transaction_type = kind.into();
        t
    }

    fn doc(amount: &str, contact: &str) -> ArchiveDocument {
        ArchiveDocument {
            id: String::new(),
            subject: String::new(),
            document_date: String::new(),
            amount: amount.into(),
            folder: String::new(),
            contact_name: contact.into(),
            file_name: String::new(),
            reference: String::new(),
        }
    }

    fn be_rules() -> UnmatchedRules {
        UnmatchedRules {
            ignore_counterparties: Vec::new(),
            ignore_descriptions: BE_IGNORE_DESCRIPTIONS.iter().map(|s| (*s).into()).collect(),
            no_document_accounts: vec!["65".into()],
            own_names: vec![normalize_name("Example Studio BV")],
        }
    }

    /// Whether the ledger shows the payment of `amount` on `date` as covered.
    fn covered(ledger: &mut CreditorLedger, date: &str, amount: &str) -> bool {
        ledger
            .claim(&entry_key(date, amount), "")
            .is_some_and(|p| p.covered)
    }

    fn ids(found: &[UnmatchedDebit]) -> Vec<&str> {
        found.iter().map(|u| u.id.as_str()).collect()
    }

    #[test]
    fn parse_counterparty_takes_the_last_coda_segment_as_name() {
        assert_eq!(parse_counterparty(CODA_TRANSFER), "EXAMPLE RECOVERY BV");
    }

    #[test]
    fn parse_counterparty_squeezes_coda_padding() {
        let desc = "Binnenlandse overschrijvingen - SEPA credit transfers : Enkelvoudige \
            overschrijving | Netto bedrag: 500,000 : RC | EXAMPLE PERSON    0000 TOWN";
        assert_eq!(parse_counterparty(desc), "EXAMPLE PERSON 0000 TOWN");
    }

    #[test]
    fn parse_counterparty_falls_back_to_the_coda_type_without_a_name() {
        assert_eq!(
            parse_counterparty(CODA_CARD),
            "Kaarten : Betaling met debetkaart binnen eurozone"
        );
        assert_eq!(
            parse_counterparty(CODA_DIRECT_DEBIT),
            "Domiciliëringen - Direct debit : Betaling"
        );
        assert_eq!(
            parse_counterparty(CODA_SALARY),
            "Binnenlandse overschrijvingen - SEPA credit transfers : Betaling lonen, e.d."
        );
    }

    #[test]
    fn normalize_name_strips_belgian_legal_forms() {
        assert_eq!(normalize_name("Example NV"), "example");
        assert_eq!(normalize_name("Exemple SA"), "exemple");
        assert_eq!(normalize_name("Example BVBA"), "example");
        assert_eq!(normalize_name("Exemple S.P.R.L."), "exemple");
        assert_eq!(normalize_name("Exemple SRL"), "exemple");
        assert_eq!(normalize_name("Example VZW"), "example");
        assert_eq!(normalize_name("Example CVBA"), "example");
        assert_eq!(normalize_name("Example Comm.V"), "example");
    }

    #[test]
    fn normalize_name_keeps_suffix_letters_inside_words() {
        assert_eq!(normalize_name("Sanvest Bvio"), "sanvest bvio");
    }

    #[test]
    fn gl_codes_are_not_names() {
        assert!(is_gl_code("657100"));
        assert!(!is_gl_code("Example NV"));
        assert!(!is_gl_code(""));
    }

    #[test]
    fn lookback_starts_three_months_earlier() {
        assert_eq!(lookback_start("2026-07-01"), "2026-04-01");
        assert_eq!(lookback_start("2026-02-15"), "2025-11-01");
        assert_eq!(lookback_start("2026-01-01"), "2025-10-01");
    }

    #[test]
    fn creditor_ledger_covers_payments_by_supplier_invoice() {
        let entries = vec![
            // Invoice from before the period, paid inside it.
            tx(
                "i1",
                "2026-06-20",
                "-50.00",
                "Factuur van Supplier A",
                "Supplier A",
            ),
            tx("p1", "2026-07-02", "50.00", CODA_TRANSFER, "Supplier A"),
            // Two invoices paid in one batch.
            tx(
                "i2",
                "2026-07-03",
                "-10.00",
                "Factuur van Supplier B",
                "Supplier B",
            ),
            tx(
                "i3",
                "2026-07-04",
                "-15.00",
                "Factuur van Supplier B",
                "Supplier B",
            ),
            tx("p2", "2026-07-05", "25.00", CODA_CARD, "Supplier B"),
            // A payment to a supplier with no invoice at all.
            tx("p3", "2026-07-06", "30.00", CODA_CARD, "Supplier C"),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(covered(&mut ledger, "2026-07-02", "50.00"));
        assert!(covered(&mut ledger, "2026-07-05", "25.00"));
        let open = ledger.claim(&entry_key("2026-07-06", "30.00"), "").unwrap();
        assert!(!open.covered);
        assert_eq!(open.contact, "Supplier C");
    }

    #[test]
    fn creditor_ledger_does_not_let_one_invoice_cover_two_payments() {
        let entries = vec![
            tx(
                "i1",
                "2026-07-01",
                "-20.00",
                "Factuur van Supplier A",
                "Supplier A",
            ),
            tx("p1", "2026-07-02", "20.00", CODA_CARD, "Supplier A"),
            tx("p2", "2026-07-09", "20.00", CODA_CARD, "Supplier A"),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(covered(&mut ledger, "2026-07-02", "20.00"));
        assert!(!covered(&mut ledger, "2026-07-09", "20.00"));
    }

    #[test]
    fn find_unmatched_belgian_admin() {
        let bank_a = vec![
            tx("loan", "2026-07-10", "-900.00", CODA_LOAN, ""),
            tx("salary", "2026-07-28", "-1000.00", CODA_SALARY, ""),
            tx(
                "own",
                "2026-07-06",
                "-300.00",
                "Binnenlandse overschrijvingen - SEPA credit \
                transfers : Enkelvoudige overschrijving | Netto bedrag: 300,000 | EXAMPLE STUDIO",
                "",
            ),
            tx(
                "to-b",
                "2026-07-07",
                "-400.00",
                "Interne overboeking | Netto bedrag: 400,000",
                "",
            ),
            tx(
                "via-580",
                "2026-07-08",
                "-250.00",
                "Overschrijving | Netto bedrag: 250,000",
                "",
            ),
            tx("gl", "2026-07-06", "-4.56", CODA_CARD, "657100"),
            tx("covered", "2026-07-02", "-50.00", CODA_CARD, ""),
            tx("uncovered", "2026-07-06", "-30.00", CODA_CARD, ""),
            tx("card", "2026-07-03", "-12.34", CODA_CARD, ""),
            tx("recovery", "2026-07-07", "-88.11", CODA_TRANSFER, ""),
            tx("credit", "2026-07-01", "120.00", CODA_TRANSFER, ""),
        ];
        let bank_b = vec![tx(
            "from-a",
            "2026-07-07",
            "400.00",
            "Interne overboeking",
            "",
        )];
        let banks = vec![
            ("550003".to_string(), bank_a),
            ("550002".to_string(), bank_b),
        ];
        let creditor = vec![
            tx(
                "i1",
                "2026-06-20",
                "-50.00",
                "Factuur van Supplier A",
                "Supplier A",
            ),
            tx("p1", "2026-07-02", "50.00", CODA_CARD, "Supplier A"),
            tx("p3", "2026-07-06", "30.00", CODA_CARD, "Supplier C"),
        ];
        let transfers = vec![tx("t1", "2026-07-08", "250.00", "Overschrijving", "")];

        let found = find_unmatched(
            &banks,
            match_creditor_ledger(&creditor, "2026-07-01"),
            &transfers,
            &[],
            &[],
            &be_rules(),
        );

        assert_eq!(ids(&found), ["uncovered", "card", "recovery"]);
        assert_eq!(found[0].counterparty, "Supplier C");
        assert_eq!(
            found[1].counterparty,
            "Kaarten : Betaling met debetkaart binnen eurozone"
        );
        assert_eq!(found[2].counterparty, "EXAMPLE RECOVERY BV");
        assert_eq!(found[2].bank_account, "550003");
        assert_eq!(found[2].amount, "-88.11");
    }

    #[test]
    fn find_unmatched_ledger_supplier_skips_the_archive_name_fallback() {
        let banks = vec![(
            "550003".to_string(),
            vec![tx("p", "2026-07-06", "-30.00", CODA_CARD, "")],
        )];
        let creditor = vec![tx("p3", "2026-07-06", "30.00", CODA_CARD, "Supplier C")];
        // An archived document of another amount from the same supplier must not
        // hide a payment the supplier ledger already found uncovered.
        let found = find_unmatched(
            &banks,
            match_creditor_ledger(&creditor, "2026-07-01"),
            &[],
            &[],
            &[doc("12.00", "Supplier C")],
            &be_rules(),
        );
        assert_eq!(ids(&found), ["p"]);
    }

    #[test]
    fn find_unmatched_dutch_admin_keeps_amount_and_name_matching() {
        let rules = UnmatchedRules {
            ignore_counterparties: vec!["Belastingdienst".into()],
            ..UnmatchedRules::default()
        };
        let sepa =
            |name: &str| format!("/TRTP/SEPA/CNTP/NL00TEST0000000000/TESTNL2A/{name}/REMI/x");
        let banks = vec![(
            "11001".to_string(),
            vec![
                tx(
                    "open",
                    "2025-03-01",
                    "-100.00",
                    &sepa("Supplier A B.V."),
                    "",
                ),
                tx("archived", "2025-03-02", "-7.28", &sepa("Supplier B"), ""),
                tx(
                    "by-name",
                    "2025-03-03",
                    "-9.99",
                    &sepa("Supplier C via Mollie"),
                    "",
                ),
                tx(
                    "ignored",
                    "2025-03-04",
                    "-500.00",
                    &sepa("Belastingdienst"),
                    "",
                ),
                tx(
                    "missing",
                    "2025-03-05",
                    "-42.00",
                    &sepa("Supplier D B.V."),
                    "",
                ),
                // Loans are not special-cased outside Belgium.
                tx("loan", "2025-03-06", "-80.00", CODA_LOAN, ""),
            ],
        )];
        let open = vec![OutstandingItem {
            contact_name: "Supplier A B.V.".into(),
            description: String::new(),
            date: String::new(),
            amount: "100.00".into(),
            open_amount: "100.00".into(),
        }];
        let found = find_unmatched(
            &banks,
            CreditorLedger::default(),
            &[],
            &open,
            &[doc("7.28", "Supplier B"), doc("20.00", "Supplier C")],
            &rules,
        );
        assert_eq!(ids(&found), ["missing", "loan"]);
        assert_eq!(found[0].counterparty, "Supplier D B.V.");
    }

    fn config(region: &str, entry: &str) -> Config {
        toml::from_str(&format!(
            "api_key = \"k\"\ndefault_admin = \"co\"\n{region}\n[administrations.co]\n\
             domain_id = \"d\"\nadmin_id = \"a\"\nname = \"Example Studio BV\"\n{entry}"
        ))
        .unwrap()
    }

    #[test]
    fn setup_defaults_for_the_netherlands_are_unchanged() {
        let setup = UnmatchedSetup::resolve(&config("", ""), "co", &[]);
        assert_eq!(setup.bank_accounts, ["11001"]);
        assert!(setup.creditor_accounts.is_empty());
        assert!(setup.transfer_accounts.is_empty());
        assert!(setup.rules.ignore_descriptions.is_empty());
        assert!(setup.rules.no_document_accounts.is_empty());
        assert!(setup.rules.own_names.is_empty());
    }

    #[test]
    fn setup_defaults_for_belgium() {
        let setup = UnmatchedSetup::resolve(&config("region = \"be\"", ""), "co", &[]);
        assert_eq!(setup.bank_accounts, ["550000"]);
        assert_eq!(setup.creditor_accounts, ["440000"]);
        assert_eq!(setup.transfer_accounts, ["580000"]);
        assert!(!setup.rules.ignore_descriptions.is_empty());
        assert_eq!(setup.rules.no_document_accounts, ["65"]);
        assert_eq!(setup.rules.own_names, ["example studio"]);
    }

    #[test]
    fn setup_prefers_flag_then_administration_config() {
        let entry = "bank_accounts = [\"550002\", \"550003\"]\ncreditor_accounts = []\n\
                     unmatched_ignore_descriptions = [\"Huur\"]";
        let cfg = config("region = \"be\"", entry);
        let setup = UnmatchedSetup::resolve(&cfg, "co", &[]);
        assert_eq!(setup.bank_accounts, ["550002", "550003"]);
        assert!(setup.creditor_accounts.is_empty());
        assert_eq!(setup.transfer_accounts, ["580000"]);
        assert_eq!(setup.rules.ignore_descriptions, ["Huur"]);

        let flagged = UnmatchedSetup::resolve(&cfg, "co", &["550009".to_string()]);
        assert_eq!(flagged.bank_accounts, ["550009"]);
    }

    #[test]
    fn normalize_name_handles_brackets_dashes_slashes_and_spaced_forms() {
        assert_eq!(normalize_name("Example (Belgium) NV"), "example belgium");
        assert_eq!(normalize_name("Ex-ample"), "ex ample");
        assert_eq!(normalize_name("Example / Other"), "example other");
        assert_eq!(normalize_name("Example Comm. V."), "example");
        assert_eq!(normalize_name("Example Comm. VA"), "example");
        assert_eq!(normalize_name("Example B. V."), "example");
        assert!(names_match("Example Comm. V.", "EXAMPLE COMMV"));
    }

    #[test]
    fn normalize_name_never_returns_empty_for_a_name() {
        assert_eq!(normalize_name("NV"), "nv");
        assert_eq!(normalize_name("B.V."), "bv");
        assert_eq!(normalize_name("(-)"), "(-)");
        assert_eq!(normalize_name("  "), "");
    }

    #[test]
    fn creditor_ledger_lets_pre_period_payments_consume_invoices_first() {
        let entries = vec![
            typed(
                "9",
                tx("i1", "2026-05-01", "-100.00", "Factuur", "Supplier A"),
            ),
            typed(
                "9",
                tx("i2", "2026-05-15", "-100.00", "Factuur", "Supplier A"),
            ),
            // Paid before the period: settles i1 and half of i2 (FIFO).
            typed(
                "0",
                tx("p0", "2026-06-10", "150.00", CODA_CARD, "Supplier A"),
            ),
            // Only 50.00 of invoices is left for this one.
            typed(
                "0",
                tx("p1", "2026-07-05", "100.00", CODA_CARD, "Supplier A"),
            ),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(!covered(&mut ledger, "2026-07-05", "100.00"));
    }

    #[test]
    fn creditor_ledger_does_not_count_a_supplier_refund_as_an_invoice() {
        let entries = vec![
            // Money back from the supplier: same sign as an invoice, no document.
            typed(
                "0",
                tx("r1", "2026-07-01", "-80.00", CODA_TRANSFER, "Supplier A"),
            ),
            typed(
                "0",
                tx("p1", "2026-07-10", "80.00", CODA_CARD, "Supplier A"),
            ),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(!covered(&mut ledger, "2026-07-10", "80.00"));
    }

    #[test]
    fn creditor_ledger_offsets_credit_notes_without_treating_them_as_payments() {
        let entries = vec![
            typed(
                "9",
                tx("i1", "2026-06-01", "-100.00", "Factuur", "Supplier A"),
            ),
            // A credit note of 30.00 leaves 70.00 to pay.
            typed(
                "9",
                tx("c1", "2026-06-05", "30.00", "Creditnota", "Supplier A"),
            ),
            typed(
                "0",
                tx("p1", "2026-07-03", "70.00", CODA_CARD, "Supplier A"),
            ),
            // A credit note on the same day and amount as a real bank payment
            // to another supplier must not cover that payment.
            typed(
                "9",
                tx("i2", "2026-07-01", "-40.00", "Factuur", "Supplier B"),
            ),
            typed(
                "9",
                tx("c2", "2026-07-08", "40.00", "Creditnota", "Supplier B"),
            ),
            typed(
                "0",
                tx("p2", "2026-07-08", "40.00", CODA_CARD, "Supplier C"),
            ),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(covered(&mut ledger, "2026-07-03", "70.00"));
        let p2 = ledger.claim(&entry_key("2026-07-08", "40.00"), "").unwrap();
        assert!(!p2.covered);
        assert_eq!(p2.contact, "Supplier C");
        assert!(
            ledger
                .claim(&entry_key("2026-07-08", "40.00"), "")
                .is_none()
        );
    }

    #[test]
    fn find_unmatched_keeps_every_ledger_payment_of_the_same_day_and_amount() {
        let named = "Binnenlandse overschrijvingen - SEPA credit transfers : Enkelvoudige \
            overschrijving | Netto bedrag: 25,000 : | SUPPLIER B NV";
        let banks = vec![(
            "550003".to_string(),
            vec![
                tx("to-b", "2026-07-06", "-25.00", named, ""),
                tx("card", "2026-07-06", "-25.00", CODA_CARD, ""),
            ],
        )];
        let creditor = vec![
            // Supplier B was paid without an invoice; Supplier A's is covered.
            typed("0", tx("pb", "2026-07-06", "25.00", named, "Supplier B")),
            typed(
                "9",
                tx("ia", "2026-06-20", "-25.00", "Factuur", "Supplier A"),
            ),
            typed(
                "0",
                tx("pa", "2026-07-06", "25.00", CODA_CARD, "Supplier A"),
            ),
        ];
        let found = find_unmatched(
            &banks,
            match_creditor_ledger(&creditor, "2026-07-01"),
            &[],
            &[],
            &[],
            &be_rules(),
        );
        assert_eq!(ids(&found), ["to-b"]);
        assert_eq!(found[0].counterparty, "Supplier B");
    }

    #[test]
    fn find_unmatched_skips_only_gl_bookings_to_no_document_accounts() {
        let banks = vec![(
            "550003".to_string(),
            vec![
                tx("bank-cost", "2026-07-06", "-4.56", CODA_CARD, "657100"),
                tx("interest", "2026-07-06", "-9.10", CODA_CARD, "650000"),
                tx("expense", "2026-07-07", "-61.50", CODA_TRANSFER, "612000"),
            ],
        )];
        let found = find_unmatched(
            &banks,
            CreditorLedger::default(),
            &[],
            &[],
            &[],
            &be_rules(),
        );
        assert_eq!(ids(&found), ["expense"]);
        assert_eq!(found[0].counterparty, "EXAMPLE RECOVERY BV");
    }

    #[test]
    fn setup_reads_no_document_accounts() {
        let be = UnmatchedSetup::resolve(&config("region = \"be\"", ""), "co", &[]);
        assert_eq!(be.rules.no_document_accounts, ["65"]);
        let nl = UnmatchedSetup::resolve(&config("", ""), "co", &[]);
        assert!(nl.rules.no_document_accounts.is_empty());
        let custom = config(
            "region = \"be\"",
            "no_document_accounts = [\"657\", \"6400\"]",
        );
        let setup = UnmatchedSetup::resolve(&custom, "co", &[]);
        assert_eq!(setup.rules.no_document_accounts, ["657", "6400"]);
    }

    #[test]
    fn creditor_ledger_counts_document_linked_payment_differences() {
        // Yuki books a rounding/FX difference found while matching a payment to
        // an invoice as a bank-type line that carries the invoice's document.
        let with_file = |mut t: GlTransactionWithContact| {
            t.file_name = "invoice.pdf".into();
            t
        };
        let entries = vec![
            typed(
                "9",
                with_file(tx("i1", "2026-07-12", "-144.23", "Factuur", "Supplier A")),
            ),
            typed(
                "0",
                tx("p1", "2026-07-13", "144.25", CODA_CARD, "Supplier A"),
            ),
            typed(
                "0",
                with_file(tx("d1", "2026-07-13", "-0.02", "Factuur", "Supplier A")),
            ),
            // And the other way round: paid 0.02 less, difference booked as a debit.
            typed(
                "9",
                with_file(tx("i2", "2026-07-16", "-47.27", "Factuur", "Supplier B")),
            ),
            typed(
                "0",
                tx("p2", "2026-07-17", "47.25", CODA_CARD, "Supplier B"),
            ),
            typed(
                "0",
                with_file(tx("d2", "2026-07-17", "0.02", "Factuur", "Supplier B")),
            ),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(covered(&mut ledger, "2026-07-13", "144.25"));
        assert!(covered(&mut ledger, "2026-07-17", "47.25"));
    }
}
