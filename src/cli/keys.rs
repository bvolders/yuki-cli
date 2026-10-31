//! Loose comparison keys for what people type differently: supplier names
//! and invoice references (invoice numbers compare as
//! [`same_number`](crate::cli::invoice_number::same_number), file names as
//! [`name_key`](crate::sync::name_key)).

use crate::sync::name_key;

/// Legal forms left out of a supplier name when names are compared.
const LEGAL_FORMS: &[&str] = &[
    "bv", "nv", "bvba", "srl", "sa", "vof", "commv", "ltd", "inc", "llc", "gmbh",
];

/// A supplier name as names are compared: Unicode NFC, lower case, dots
/// dropped (`B.V.` is `bv`), other punctuation as spaces, whitespace
/// collapsed, and legal forms left out: `Leverancier B.V.` is `leverancier`.
pub fn contact_key(name: &str) -> String {
    let lower = name_key(name).replace('.', "");
    let spaced: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = spaced
        .split_whitespace()
        .filter(|w| !LEGAL_FORMS.contains(w))
        .collect();
    if words.is_empty() {
        // A name that is only a legal form: keep it whole.
        spaced.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        words.join(" ")
    }
}

/// What to ask Yuki's free-text searches for a supplier name: they match a
/// raw substring of the name, so a [`contact_key`] (`may june` for
/// `MAY & JUNE`) finds nothing. This is the name's longest word as written
/// (letters and digits, not a legal form), or the trimmed name when it has
/// none; the results are then compared by [`contact_key`].
pub fn search_word(name: &str) -> String {
    let word = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !LEGAL_FORMS.contains(&w.to_lowercase().as_str()))
        .fold("", |best, w| {
            if w.chars().count() > best.chars().count() {
                w
            } else {
                best
            }
        });
    if word.is_empty() { name.trim() } else { word }.to_string()
}

/// A reference as references are compared: lower case, letters and digits
/// only, so `F 2026/0042` is `F2026-0042`.
pub fn reference_key(reference: &str) -> String {
    name_key(reference)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_word_is_a_raw_substring_of_the_name() {
        for (name, word) in [
            ("MAY & JUNE Woudenberg", "Woudenberg"),
            ("Leverancier B.V.", "Leverancier"),
            ("D'Ieteren Lease NV", "Ieteren"),
            ("O'Neill & Co. BVBA", "Neill"),
            ("Acme, Inc.", "Acme"),
            ("BV", "BV"),
        ] {
            let got = search_word(name);
            assert_eq!(got, word, "{name}");
            assert!(name.contains(&got), "{name}: {got}");
        }
    }
}
