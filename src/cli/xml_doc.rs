//! The indented XML writer the `xmlDoc` of `ProcessSalesInvoices` and
//! `ProcessPurchaseInvoices` is built with: elements in the order written,
//! every value escaped.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::client::escape_text;

/// The XML Schema instance namespace; Yuki rejects an invoice document
/// whose root does not declare it.
pub const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// An indented XML writer for the few element shapes the documents need.
#[derive(Default)]
pub struct XmlWriter {
    out: String,
    depth: usize,
}

impl XmlWriter {
    /// A document whose root `name` declares namespace `ns` and `xsi`.
    pub fn root(name: &str, ns: &str) -> Self {
        let mut x = Self::default();
        let _ = writeln!(x.out, r#"<{name} xmlns="{ns}" xmlns:xsi="{XSI}">"#);
        x.depth = 1;
        x
    }

    fn indent(&mut self) {
        self.out.push_str(&"  ".repeat(self.depth));
    }

    pub fn open(&mut self, name: &str) {
        self.indent();
        let _ = writeln!(self.out, "<{name}>");
        self.depth += 1;
    }

    pub fn close(&mut self, name: &str) {
        self.depth -= 1;
        self.indent();
        let _ = writeln!(self.out, "</{name}>");
    }

    pub fn leaf(&mut self, name: &str, value: &str) {
        self.indent();
        let _ = writeln!(self.out, "<{name}>{}</{name}>", escape_text(value));
    }

    pub fn opt(&mut self, name: &str, value: &Option<String>) {
        if let Some(value) = value {
            self.leaf(name, value);
        }
    }

    /// `DocumentBase64` with the file `bytes` encoded, or, for a human
    /// (`embed` false), a comment giving the sizes instead.
    pub fn base64(&mut self, bytes: &[u8], embed: bool) {
        if embed {
            self.leaf("DocumentBase64", &BASE64.encode(bytes));
        } else {
            self.indent();
            let _ = writeln!(
                self.out,
                "<DocumentBase64><!-- {} bytes base64 ({}-byte PDF) --></DocumentBase64>",
                bytes.len().div_ceil(3) * 4,
                bytes.len()
            );
        }
    }

    /// Close the root opened by [`root`](Self::root): the document.
    pub fn finish(mut self, root: &str) -> String {
        let _ = write!(self.out, "</{root}>");
        self.out
    }
}

pub fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
