pub mod accounting;
pub mod accounting_info;
pub mod archive;
pub mod contact;
pub mod sales;
pub mod soap_client;
pub mod vat;

use std::borrow::Cow;

use quick_xml::escape::EscapeError;
use quick_xml::events::BytesText;

pub use soap_client::{SoapClient, SoapEnvelope};

/// Strip any XML namespace prefix, returning only the local name.
pub(crate) fn local_name(name: &str) -> &str {
    name.rfind(':').map(|i| &name[i + 1..]).unwrap_or(name)
}

/// Decode XML entity escapes (`&amp;`, `&lt;`, ...) in an element's text
/// content. `BytesText` carries its content pre-decoded to UTF-8 but still
/// escaped, so this must run before the text is used.
pub(crate) fn unescape_text<'a>(text: &'a BytesText<'_>) -> Result<Cow<'a, str>, EscapeError> {
    quick_xml::escape::unescape(text.as_ref())
}
