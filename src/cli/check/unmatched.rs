//! The matching engine behind `yuki check unmatched`: which bank debits have
//! no purchase invoice, from ledgers, open items and the archive. `check
//! matches` reuses its fetches, amounts, ledger and filters (`pub(super)`).

use std::collections::{HashMap, HashSet};

use futures_util::future::try_join_all;

use super::resolve_period;
use crate::cli::setup_archive;
use crate::client::Region;
use crate::client::accounting::{
    AccountingClient, GlTransactionWithContact, OutstandingItem, TransactionType,
};
use crate::client::archive::ArchiveDocument;
use crate::config::Config;
use crate::error::YukiError;
pub(super) use crate::money::Cents;
use crate::output::{OutputFormat, format_json, format_table, is_tty};
use crate::period::month_start_before;

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
    if let Some(name) = parse_counterparty_name(description) {
        return name;
    }
    if let Some(kind) = coda_kind(description) {
        return kind;
    }
    description
        .chars()
        .take(50)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The counterparty *name* a bank description carries, or `None` when it has
/// none (a CODA card payment or direct debit without a name, or free text).
fn parse_counterparty_name(description: &str) -> Option<String> {
    if let Some(cntp_start) = description.find("/CNTP/") {
        let after_cntp = &description[cntp_start + 6..];
        let parts: Vec<&str> = after_cntp.splitn(4, '/').collect();
        if parts.len() >= 3 {
            return Some(parts[2].trim().to_string()).filter(|n| !n.is_empty());
        }
    }
    parse_coda_counterparty(description)
}

/// The CODA transaction type (first segment) of a Belgian description.
fn coda_kind(description: &str) -> Option<String> {
    if !description.contains("Netto bedrag:") {
        return None;
    }
    let kind = description
        .split('|')
        .map(str::trim)
        .find(|s| !s.is_empty())?;
    Some(kind.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Counterparty name from a Belgian CODA description, or `None` when it is
/// not one or names nobody.
fn parse_coda_counterparty(description: &str) -> Option<String> {
    if !description.contains("Netto bedrag:") {
        return None;
    }
    let segments: Vec<&str> = description
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let (_kind, rest) = segments.split_first()?;
    let name = rest.iter().skip(1).rev().find(|seg| {
        let lower = seg.to_lowercase();
        !CODA_DETAIL_PREFIXES.iter().any(|p| lower.starts_with(p))
    })?;
    // Squeeze the fixed-width padding CODA uses inside name/address fields.
    Some(name.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The supplier the bank names for a line: the API contact when it is a name,
/// else the counterparty in the description ([`parse_counterparty`]).
pub(super) fn bank_counterparty(tx: &GlTransactionWithContact) -> String {
    if !tx.contact_name.is_empty() && !is_gl_code(&tx.contact_name) {
        tx.contact_name.clone()
    } else {
        parse_counterparty(&tx.description)
    }
}

/// Like [`bank_counterparty`], but `None` when the line names nobody, so the
/// CODA type or a description fragment is never mistaken for a name.
pub(super) fn bank_counterparty_name(tx: &GlTransactionWithContact) -> Option<String> {
    if !tx.contact_name.is_empty() && !is_gl_code(&tx.contact_name) {
        Some(tx.contact_name.clone())
    } else {
        parse_counterparty_name(&tx.description)
    }
}

/// Whether an API contact value is a GL account code rather than a name.
///
/// Yuki fills `Contact` with the GL code when a bank line was booked straight to
/// a ledger account (e.g. by a bank rule), so the value is no counterparty.
pub(super) fn is_gl_code(contact: &str) -> bool {
    !contact.is_empty() && contact.chars().all(|c| c.is_ascii_digit())
}

/// Region defaults for `check unmatched`, each used when the administration's
/// config does not set its own.
struct CheckDefaults {
    /// Bank GL accounts to scan.
    bank_accounts: &'static [&'static str],
    /// Creditors control accounts that show who a payment went to.
    creditor_accounts: &'static [&'static str],
    /// Internal-transfer accounts.
    transfer_accounts: &'static [&'static str],
    /// Description patterns of bank movements that never come with a
    /// purchase invoice.
    ignore_descriptions: &'static [&'static str],
    /// GL prefixes that never carry a document when a bank line is booked
    /// straight to them.
    no_document_accounts: &'static [&'static str],
    /// Whether a payment to the administration's own name is an own transfer.
    own_name_is_transfer: bool,
}

impl CheckDefaults {
    fn for_region(region: Region) -> Self {
        match region {
            // The Dutch defaults keep the historic behaviour: one bank account
            // and nothing else.
            Region::Nl => Self {
                bank_accounts: &["11001"],
                creditor_accounts: &[],
                transfer_accounts: &[],
                ignore_descriptions: &[],
                no_document_accounts: &[],
                own_name_is_transfer: false,
            },
            // Belgium has a fixed chart (PCMN/MAR).
            Region::Be => Self {
                bank_accounts: &["550000"],
                creditor_accounts: &["440000"],
                transfer_accounts: &["580000"],
                ignore_descriptions: &[
                    "Lening op korte termijn",
                    "Afloss. kapitaal",
                    "Vast voorschot kredieten",
                    "Kosten i.v.m. kredieten",
                    "Afrekening kredietkaarten",
                    "Betaling lonen",
                    "BTW ONTVANGSTEN",
                    "FOD FINANCI",
                ],
                // Class 65, financial charges (interest, bank costs 657xxx,
                // exchange differences), which never come with an invoice.
                no_document_accounts: &["65"],
                own_name_is_transfer: true,
            },
        }
    }
}

/// Owned copy of a default list.
fn owned(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// Matching rules for `check unmatched`, from region defaults and config.
#[derive(Debug, Clone, Default)]
pub(super) struct UnmatchedRules {
    /// Counterparty substrings to skip (`unmatched_ignore`).
    pub(super) ignore_counterparties: Vec<String>,
    /// Full-description substrings to skip.
    pub(super) ignore_descriptions: Vec<String>,
    /// GL account prefixes that never carry a document. A bank line booked
    /// straight to one (its API contact is that GL code) is skipped; a line
    /// booked straight to any other account is still reported.
    pub(super) no_document_accounts: Vec<String>,
    /// Names of the administration itself; a payment to one is an own transfer.
    pub(super) own_names: Vec<String>,
}

impl UnmatchedRules {
    /// Whether the full description matches an ignore pattern.
    pub(super) fn skips_description(&self, description: &str) -> bool {
        let lower = description.to_lowercase();
        self.ignore_descriptions
            .iter()
            .any(|p| lower.contains(p.to_lowercase().as_str()))
    }

    /// Whether the line was booked straight to a GL account that never
    /// carries a document (its API contact is that GL code).
    pub(super) fn skips_gl_booking(&self, contact: &str) -> bool {
        is_gl_code(contact)
            && self
                .no_document_accounts
                .iter()
                .any(|prefix| contact.starts_with(prefix.as_str()))
    }

    /// Whether a bank debit that no closed invoice explains still needs no
    /// document: an own transfer (consumed from `own_transfers`), an ignored
    /// description, or a booking straight to a no-document account.
    pub(super) fn skips_debit(
        &self,
        tx: &GlTransactionWithContact,
        account: &str,
        key: &EntryKey,
        own_transfers: &mut OwnTransfers<'_>,
    ) -> bool {
        own_transfers.take(account, key)
            || self.skips_description(&tx.description)
            || self.skips_gl_booking(&tx.contact_name)
    }

    /// Whether the counterparty is ignored or is the administration itself.
    pub(super) fn skips_counterparty(&self, counterparty: &str) -> bool {
        let lower = counterparty.to_lowercase();
        self.ignore_counterparties
            .iter()
            .any(|pat| lower.contains(&pat.to_lowercase()))
            || self.own_names.contains(&normalize_name(counterparty))
    }
}

/// Region defaults and per-administration overrides for `check unmatched`.
pub(super) struct UnmatchedSetup {
    pub(super) bank_accounts: Vec<String>,
    pub(super) creditor_accounts: Vec<String>,
    pub(super) transfer_accounts: Vec<String>,
    pub(super) rules: UnmatchedRules,
}

impl UnmatchedSetup {
    pub(super) fn resolve(config: &Config, admin_name: &str, bank_flag: &[String]) -> Self {
        let entry = config.administrations.get(admin_name);
        let defaults = CheckDefaults::for_region(config.region(entry));
        // A list the administration configures wins, even when empty.
        let configured = |list: Option<&Vec<String>>, default: &[&str]| {
            list.cloned().unwrap_or_else(|| owned(default))
        };

        let bank_accounts = if !bank_flag.is_empty() {
            bank_flag.to_vec()
        } else if let Some(e) = entry.filter(|e| !e.bank_accounts.is_empty()) {
            e.bank_accounts.clone()
        } else {
            owned(defaults.bank_accounts)
        };

        let own_names = entry
            .and_then(|e| e.name.as_deref())
            .map(normalize_name)
            .filter(|n| defaults.own_name_is_transfer && !n.is_empty())
            .into_iter()
            .collect();

        Self {
            bank_accounts,
            creditor_accounts: configured(
                entry.and_then(|e| e.creditor_accounts.as_ref()),
                defaults.creditor_accounts,
            ),
            transfer_accounts: configured(
                entry.and_then(|e| e.transfer_accounts.as_ref()),
                defaults.transfer_accounts,
            ),
            rules: UnmatchedRules {
                ignore_counterparties: config.unmatched_ignore.clone(),
                ignore_descriptions: configured(
                    entry.and_then(|e| e.unmatched_ignore_descriptions.as_ref()),
                    defaults.ignore_descriptions,
                ),
                no_document_accounts: configured(
                    entry.and_then(|e| e.no_document_accounts.as_ref()),
                    defaults.no_document_accounts,
                ),
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

/// Date and amount, under which a bank debit and its ledger counter-entry meet.
pub(super) type EntryKey = (String, Cents);

pub(super) fn entry_key(date: &str, amount: Cents) -> EntryKey {
    (date.to_string(), amount)
}

/// A multiset of keys that each match at most once.
pub(super) struct Pool<K>(HashMap<K, usize>);

impl<K> Default for Pool<K> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: std::hash::Hash + Eq> Pool<K> {
    fn add(&mut self, key: K) {
        *self.0.entry(key).or_insert(0) += 1;
    }

    fn take(&mut self, key: &K) -> bool {
        match self.0.get_mut(key) {
            Some(c) if *c > 0 => {
                *c -= 1;
                true
            }
            _ => false,
        }
    }
}

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
    fn of(tx: &GlTransactionWithContact, amount: Cents) -> Self {
        let credit = amount < Cents::ZERO;
        let document = !tx.file_name.trim().is_empty();
        match &tx.transaction_type {
            None if credit => Self::Invoice,
            None => Self::Payment,
            Some(TransactionType::Purchase) if credit => Self::Invoice,
            Some(TransactionType::Purchase) => Self::CreditNote,
            // Bank lines, and any journal we do not know, by sign and document.
            Some(TransactionType::Bank | TransactionType::Unknown(_)) => match (credit, document) {
                (true, true) => Self::Invoice,
                (true, false) => Self::Refund,
                (false, _) => Self::Payment,
            },
        }
    }
}

/// A supplier payment on the creditors account.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LedgerPayment {
    /// GL transaction id of the payment line on the creditors account.
    pub(super) id: String,
    pub(super) contact: String,
    /// Whether that supplier's invoices cover it.
    pub(super) covered: bool,
}

/// What the creditors control account says about bank payments.
#[derive(Default)]
pub(super) struct CreditorLedger {
    /// Supplier payments by date and amount ([`entry_key`]). Several payments
    /// can share a key, so every one is kept.
    pub(super) payments: HashMap<EntryKey, Vec<LedgerPayment>>,
}

impl CreditorLedger {
    /// Claim the ledger payment mirroring a bank debit with `key`, whose
    /// counterparty according to the bank is `hint` (may be empty).
    ///
    /// The bank line and its ledger line share no identifier, only date and
    /// amount, so among equal candidates this prefers the supplier the bank
    /// names, then a covered payment (leaving the uncovered one for another
    /// bank line), then the first in supplier-name order.
    pub(super) fn claim(&mut self, key: &EntryKey, hint: &str) -> Option<LedgerPayment> {
        let list = self.payments.get_mut(key)?;
        let hint = normalize_name(hint);
        let index = list
            .iter()
            .position(|p| normalized_names_match(&hint, &normalize_name(&p.contact)))
            .or_else(|| list.iter().position(|p| p.covered))
            .or(if list.is_empty() { None } else { Some(0) })?;
        Some(list.remove(index))
    }
}

/// Take up to `amount` from `invoices`, oldest first.
fn consume_fifo(invoices: &mut Vec<(&str, Cents)>, amount: Cents) {
    let mut due = amount.0;
    invoices.retain_mut(|(_, open)| {
        if due <= 0 {
            return true;
        }
        let used = open.0.min(due);
        due -= used;
        open.0 -= used;
        open.0 > 0
    });
}

/// Match supplier payments on the creditors account to that supplier's invoices.
///
/// On the creditors account an invoice is a credit and a payment a debit, each
/// carrying the supplier as contact. Credit notes (debits from the purchase
/// journal) first reduce the supplier's invoices, unless a refund of the same
/// supplier and amount paid them back; refunds (credits from the bank) are no
/// invoices ([`CreditorLine`]).
///
/// Payments are taken in date order. Each is covered by an open invoice of
/// the same supplier with the same amount, or else by what is left of that
/// supplier's invoices (batched or partial payments), consumed oldest first. Payments before `period_start`
/// consume invoices the same way, so an invoice paid before the period cannot
/// also cover a payment inside it. `entries` should reach back before the
/// period so that invoices paid later are seen.
pub(super) fn match_creditor_ledger(
    entries: &[GlTransactionWithContact],
    period_start: &str,
) -> CreditorLedger {
    let mut invoices: HashMap<&str, Vec<(&str, Cents)>> = HashMap::new();
    let mut credit_notes: Vec<(&str, Cents)> = Vec::new();
    let mut refunds: Vec<(&str, Cents)> = Vec::new();
    let mut payments: Vec<(&GlTransactionWithContact, Cents)> = Vec::new();
    for tx in entries {
        let Some(amount) = Cents::parse(&tx.amount) else {
            continue;
        };
        let contact = tx.contact_name.as_str();
        if contact.is_empty() || is_gl_code(contact) || amount == Cents::ZERO {
            continue;
        }
        match CreditorLine::of(tx, amount) {
            CreditorLine::Invoice => invoices
                .entry(contact)
                .or_default()
                .push((tx.date.as_str(), -amount)),
            CreditorLine::CreditNote => credit_notes.push((contact, amount)),
            CreditorLine::Payment => payments.push((tx, amount)),
            CreditorLine::Refund => refunds.push((contact, -amount)),
        }
    }
    // A credit note the supplier paid back is settled by that refund, so it
    // no longer reduces what is owed on the invoices.
    for refund in refunds {
        if let Some(i) = credit_notes.iter().position(|c| *c == refund) {
            credit_notes.remove(i);
        }
    }
    for list in invoices.values_mut() {
        list.sort_by(|a, b| a.0.cmp(b.0));
    }

    for (contact, amount) in credit_notes {
        let list = invoices.entry(contact).or_default();
        match list.iter().position(|(_, inv)| *inv == amount) {
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
        if let Some(j) = list.iter().position(|(_, inv)| inv == amount) {
            list.remove(j);
            covered[i] = true;
            continue;
        }
        let left: Cents = list.iter().map(|(_, inv)| *inv).sum();
        if tx.date.as_str() < period_start {
            // Outside the period nothing is reported; it only uses up invoices.
            consume_fifo(list, *amount);
        } else if left >= *amount {
            consume_fifo(list, *amount);
            covered[i] = true;
        }
    }

    let mut ledger = CreditorLedger::default();
    for ((tx, amount), covered) in payments.into_iter().zip(covered) {
        ledger
            .payments
            .entry(entry_key(&tx.date, amount))
            .or_default()
            .push(LedgerPayment {
                id: tx.id.clone(),
                contact: tx.contact_name.clone(),
                covered,
            });
    }
    for list in ledger.payments.values_mut() {
        list.sort_by(|a, b| a.contact.cmp(&b.contact));
    }
    ledger
}

/// Months before the period the creditors account is read from.
pub(super) const LOOKBACK_MONTHS: i32 = 3;

/// Debits that are own transfers: the opposite entry on the same date and
/// amount sits on a transfer account or on another scanned bank account.
/// Each opposite entry explains one debit only.
pub(super) struct OwnTransfers<'a> {
    transfers: Pool<EntryKey>,
    bank_credits: Pool<(&'a str, EntryKey)>,
    accounts: Vec<&'a str>,
}

impl<'a> OwnTransfers<'a> {
    pub(super) fn new(
        banks: &'a [(String, Vec<GlTransactionWithContact>)],
        transfer_entries: &[GlTransactionWithContact],
    ) -> Self {
        let mut transfers = Pool::default();
        for tx in transfer_entries {
            if let Some(a) = Cents::parse(&tx.amount).filter(|a| *a >= Cents::ZERO) {
                transfers.add(entry_key(&tx.date, a));
            }
        }
        // Credits on the other scanned bank accounts are own transfers too.
        let mut bank_credits = Pool::default();
        for (account, txs) in banks {
            for tx in txs {
                if let Some(a) = Cents::parse(&tx.amount).filter(|a| *a > Cents::ZERO) {
                    bank_credits.add((account.as_str(), entry_key(&tx.date, a)));
                }
            }
        }
        Self {
            transfers,
            bank_credits,
            accounts: banks.iter().map(|(a, _)| a.as_str()).collect(),
        }
    }

    /// Whether the debit on `account` with `key` (positive amount) is an own
    /// transfer; consumes the opposite entry when it is.
    pub(super) fn take(&mut self, account: &str, key: &EntryKey) -> bool {
        if self.transfers.take(key) {
            return true;
        }
        let others: Vec<&'a str> = self
            .accounts
            .iter()
            .copied()
            .filter(|other| *other != account)
            .collect();
        others
            .into_iter()
            .any(|other| self.bank_credits.take(&(other, key.clone())))
    }
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
    let mut own_transfers = OwnTransfers::new(banks, transfer_entries);
    let mut creditor_pool = Pool::default();
    for item in creditor_items {
        if let Some(open) = Cents::parse(&item.open_amount) {
            creditor_pool.add(open);
        }
    }
    let mut archive_pool = Pool::default();
    for doc in archive_docs {
        if let Some(amount) = Cents::parse(&doc.amount).filter(|a| *a > Cents::ZERO) {
            archive_pool.add(amount);
        }
    }
    // Normalized archive contact names catch batched or split charges where the
    // amount differs but the supplier is known.
    //
    // Each name is normalized twice on purpose: the matcher has always compared
    // against the normalized form of the already-normalized name, and
    // normalize_name is not idempotent ("Foo (via) Bar" -> "foo via bar" ->
    // "foo"). Doing both passes here keeps those matches while normalizing
    // each name once per run rather than once per bank line.
    let archive_names: HashSet<String> = archive_docs
        .iter()
        .filter(|d| !d.contact_name.is_empty())
        .map(|d| normalize_name(&normalize_name(&d.contact_name)))
        .filter(|n| !n.is_empty())
        .collect();
    let mut unmatched = Vec::new();
    for (account, txs) in banks {
        for tx in txs {
            // Only debits (payments out) are relevant.
            let Some(amount) = Cents::parse(&tx.amount).filter(|a| *a < Cents::ZERO) else {
                continue;
            };
            let abs_amount = amount.abs();
            let key = entry_key(&tx.date, abs_amount);

            // The supplier the bank names: the API contact when it is a name,
            // else the description. Picks among equal ledger payments.
            let bank_counterparty = bank_counterparty(tx);
            let ledger_payment = ledger.claim(&key, &bank_counterparty);
            if ledger_payment.as_ref().is_some_and(|p| p.covered)
                || rules.skips_debit(tx, account, &key, &mut own_transfers)
            {
                continue;
            }

            if creditor_pool.take(&abs_amount) || archive_pool.take(&abs_amount) {
                continue;
            }

            // Prefer the supplier from the creditors account over the bank's.
            let ledger_contact = ledger_payment.map(|p| p.contact);
            let known_to_ledger = ledger_contact.is_some();
            let counterparty = ledger_contact.unwrap_or(bank_counterparty);
            if rules.skips_counterparty(&counterparty) {
                continue;
            }
            let normalized = normalize_name(&counterparty);
            // The creditors account already compared this supplier's invoices.
            if !known_to_ledger
                && archive_names
                    .iter()
                    .any(|n| normalized_names_match(&normalized, n))
            {
                continue;
            }

            unmatched.push(UnmatchedDebit {
                bank_account: account.clone(),
                id: tx.id.clone(),
                date: tx.date.clone(),
                amount: amount.to_string(),
                counterparty,
                description: tx.description.clone(),
            });
        }
    }
    unmatched
}

/// Transactions with contact on each of `accounts` from `from` to `to`, one
/// list per account in the same order; one API call per account, concurrently.
pub(super) async fn gl_entries(
    client: &AccountingClient,
    admin_id: &str,
    accounts: &[String],
    from: &str,
    to: &str,
) -> Result<Vec<Vec<GlTransactionWithContact>>, YukiError> {
    try_join_all(
        accounts
            .iter()
            .map(|account| client.gl_account_transactions_and_contact(admin_id, account, from, to)),
    )
    .await
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
    // The Accounting session works on the archive too, saving an
    // Authenticate. It also carries SetCurrentDomain(target.domain_id), so
    // the archive is searched in the target administration's domain,
    // deliberately: the documents must belong to the administration whose
    // bank lines are being checked. A fresh Archive session (as `documents`
    // and `upload` use) would search the key's default domain instead, which
    // differs for a key that reaches several domains.
    let (accounting, archive, target) = setup_archive(config, admin).await?;
    let setup = UnmatchedSetup::resolve(config, target.config_name, bank_accounts);
    // Read the creditors account far enough back to see invoices paid later.
    let lookback = month_start_before(&start, LOOKBACK_MONTHS);

    if !quiet {
        for account in &setup.bank_accounts {
            eprintln!("Fetching bank transactions (GL {account})...");
        }
        for account in &setup.creditor_accounts {
            eprintln!("Fetching supplier ledger (GL {account}, from {lookback})...");
        }
        for account in &setup.transfer_accounts {
            eprintln!("Fetching internal transfers (GL {account})...");
        }
        eprintln!("Fetching outstanding creditor items...");
        eprintln!("Fetching booked invoices from archive...");
    }
    let admin_id = target.admin_id;
    // The requests are independent, so they run concurrently; results keep
    // their order, so the output does not depend on timing.
    let (bank_entries, creditor_entries, transfer_entries, creditor_items, archive_docs) = tokio::try_join!(
        gl_entries(&accounting, admin_id, &setup.bank_accounts, &start, &end),
        gl_entries(
            &accounting,
            admin_id,
            &setup.creditor_accounts,
            &lookback,
            &end
        ),
        gl_entries(
            &accounting,
            admin_id,
            &setup.transfer_accounts,
            &start,
            &end
        ),
        accounting.outstanding_creditor_items(admin_id),
        archive.search_documents("", &start, &end),
    )?;

    if !quiet {
        // Authenticate + SetCurrentDomain, one call per GL account, the open
        // items, and SearchDocuments on the archive.
        let gl_calls = bank_entries.len() + creditor_entries.len() + transfer_entries.len();
        eprintln!("API calls made: {}", 2 + gl_calls + 1 + 1);
    }

    let banks: Vec<(String, Vec<GlTransactionWithContact>)> = setup
        .bank_accounts
        .iter()
        .cloned()
        .zip(bank_entries)
        .collect();
    let creditor_entries = creditor_entries.concat();
    let transfer_entries = transfer_entries.concat();
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

/// Web domain endings dropped from a word by [`normalize_name`].
const WEB_DOMAINS: &[&str] = &[
    ".com", ".be", ".nl", ".eu", ".net", ".org", ".io", ".de", ".fr", ".co.uk",
];

/// Normalize a company name for fuzzy matching.
///
/// Lowercases the name, removes "via ..." suffixes and web domain endings
/// ("Shop.com"), treats `, ( ) - /` as word breaks, drops dots, and removes legal-form words ("B.V.", "NV", "SRL",
/// "Comm.V", ...), including spaced spellings such as "B. V." or "Comm. V.".
/// Suffixes are removed as whole words only, so a name that merely contains
/// "sa" or "nv" is left intact.
///
/// A name made only of a legal form (or punctuation) never normalizes to
/// empty: it falls back to its dot-less form, then to the trimmed lowercase
/// original, so it still matches itself.
pub(super) fn normalize_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    // Remove "via ..." suffix (e.g. "Vimexx via Mollie" -> "vimexx")
    let base = lower.split(" via ").next().unwrap_or(&lower);
    // "Shop.com" is the shop: drop a web domain ending before the dots go,
    // or it would glue on as "shopcom".
    let base = base
        .split_whitespace()
        .map(|word| {
            WEB_DOMAINS
                .iter()
                .find_map(|d| word.strip_suffix(d).filter(|w| !w.is_empty()))
                .unwrap_or(word)
        })
        .collect::<Vec<_>>()
        .join(" ");
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
/// See [`normalized_names_match`].
#[cfg(test)]
fn names_match(bank_name: &str, archive_name: &str) -> bool {
    normalized_names_match(&normalize_name(bank_name), &normalize_name(archive_name))
}

/// Shortest word that can tie two different names together on its own.
const MIN_NAME_WORD: usize = 3;

/// Whether two names already passed through [`normalize_name`] are the same
/// supplier: equal, or every word of the one with fewer words is a whole word
/// of the other and at least one of those words has [`MIN_NAME_WORD`] letters.
///
/// Whole words, not substrings: "ing" must not match "bookings online".
pub(super) fn normalized_names_match(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let words = |s: &'_ str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (a, b) = (words(a), words(b));
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.iter().any(|w| w.chars().count() >= MIN_NAME_WORD)
        && short.iter().all(|w| long.contains(w))
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
    fn names_match_when_every_word_of_the_shorter_name_is_in_the_other() {
        assert!(names_match("Hetzner Online GmbH", "Hetzner"));
        assert!(names_match("Hetzner", "Hetzner Online GmbH"));
        assert!(names_match("Online Hetzner", "Hetzner Online GmbH"));
    }

    #[test]
    fn names_match_on_whole_words_not_substrings() {
        // "ing" is inside "bookings": no match.
        assert!(!names_match("ING", "Bookings Online"));
        assert!(!names_match("Bookings Online", "ING"));
        assert!(names_match("ING", "ING Belgie"));
        assert!(!names_match("Supplier A", "Supplier B"));
        assert!(!names_match("Pay", "Paypal Europe"));
    }

    #[test]
    fn names_match_needs_a_real_word_unless_equal() {
        // Two-letter words alone are too weak to tie names together...
        assert!(!names_match("AB", "AB Example Trading"));
        assert!(!names_match("A B", "A B C"));
        // ...but a name always matches itself.
        assert!(names_match("AB", "ab"));
    }

    #[test]
    fn names_match_ignores_a_web_domain_suffix() {
        assert!(names_match("Marketplace.com", "Marketplace NV"));
        assert_eq!(normalize_name("Marketplace.com"), "marketplace");
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
        58,400 : | Europese domiciliëring - Datum: 01-07-26 - Type domiciliëring: recurrent";
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
            transaction_type: None,
            file_name: String::new(),
        }
    }

    /// A creditors-account line with its journal type: "9" for a purchase
    /// document, "0" for a bank line.
    fn typed(kind: &str, mut t: GlTransactionWithContact) -> GlTransactionWithContact {
        t.transaction_type = TransactionType::from_code(kind);
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
            ignore_descriptions: owned(CheckDefaults::for_region(Region::Be).ignore_descriptions),
            no_document_accounts: vec!["65".into()],
            own_names: vec![normalize_name("Example Studio BV")],
        }
    }

    fn key(date: &str, amount: &str) -> EntryKey {
        entry_key(date, Cents::parse(amount).unwrap())
    }

    /// Whether the ledger shows the payment of `amount` on `date` as covered.
    fn covered(ledger: &mut CreditorLedger, date: &str, amount: &str) -> bool {
        ledger
            .claim(&key(date, amount), "")
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
        let open = ledger.claim(&key("2026-07-06", "30.00"), "").unwrap();
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
    fn find_unmatched_archive_name_fallback_compares_as_before_the_refactor() {
        // The archive names were normalized twice before comparing, and
        // normalize_name is not idempotent: "Foo (via) Bar" becomes
        // "foo via bar", then "foo", which matches the bank's "Foo Bar".
        let sepa = "/TRTP/SEPA/CNTP/NL00TEST0000000000/TESTNL2A/Foo Bar/REMI/x";
        let banks = vec![(
            "11001".to_string(),
            vec![tx("by-name", "2025-03-03", "-8.95", sepa, "")],
        )];
        let found = find_unmatched(
            &banks,
            CreditorLedger::default(),
            &[],
            &[],
            &[doc("12.00", "Foo (via) Bar")],
            &UnmatchedRules::default(),
        );
        assert!(found.is_empty(), "{:?}", ids(&found));
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
                    "-8.95",
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
            ..Default::default()
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
    fn creditor_ledger_lets_a_refunded_credit_note_leave_invoices_alone() {
        // A small credit note the supplier paid back in cash: once refunded it
        // must not also eat into the next invoices, or every later payment
        // drifts off its invoice by that amount.
        let entries = vec![
            typed(
                "9",
                tx("i1", "2025-10-21", "-600.00", "Factuur", "Supplier X"),
            ),
            typed(
                "9",
                tx("c1", "2025-12-05", "1.37", "Creditnota", "Supplier X"),
            ),
            typed(
                "0",
                tx("r1", "2025-12-22", "-1.37", CODA_TRANSFER, "Supplier X"),
            ),
            typed(
                "0",
                tx("p1", "2025-12-25", "600.00", CODA_CARD, "Supplier X"),
            ),
            typed(
                "9",
                tx("i2", "2026-02-02", "-500.00", "Factuur", "Supplier X"),
            ),
            typed(
                "0",
                tx("p2", "2026-04-02", "500.00", CODA_CARD, "Supplier X"),
            ),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-01-01");
        assert!(covered(&mut ledger, "2026-04-02", "500.00"));
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
        let p2 = ledger.claim(&key("2026-07-08", "40.00"), "").unwrap();
        assert!(!p2.covered);
        assert_eq!(p2.contact, "Supplier C");
        assert!(ledger.claim(&key("2026-07-08", "40.00"), "").is_none());
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
                tx("expense", "2026-07-07", "-58.40", CODA_TRANSFER, "612000"),
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
                with_file(tx("i1", "2026-07-12", "-133.18", "Factuur", "Supplier A")),
            ),
            typed(
                "0",
                tx("p1", "2026-07-13", "133.20", CODA_CARD, "Supplier A"),
            ),
            typed(
                "0",
                with_file(tx("d1", "2026-07-13", "-0.02", "Factuur", "Supplier A")),
            ),
            // And the other way round: paid 0.02 less, difference booked as a debit.
            typed(
                "9",
                with_file(tx("i2", "2026-07-16", "-52.07", "Factuur", "Supplier B")),
            ),
            typed(
                "0",
                tx("p2", "2026-07-17", "52.05", CODA_CARD, "Supplier B"),
            ),
            typed(
                "0",
                with_file(tx("d2", "2026-07-17", "0.02", "Factuur", "Supplier B")),
            ),
        ];
        let mut ledger = match_creditor_ledger(&entries, "2026-07-01");
        assert!(covered(&mut ledger, "2026-07-13", "133.20"));
        assert!(covered(&mut ledger, "2026-07-17", "52.05"));
    }
}
