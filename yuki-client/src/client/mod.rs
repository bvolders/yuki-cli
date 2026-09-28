pub mod accounting;
pub mod accounting_info;
pub mod archive;
pub mod contact;
mod region;
pub mod sales;
pub mod soap_client;
pub mod vat;

use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::Event;

use crate::error::YukiError;

pub use region::Region;
pub(crate) use region::service_url;
pub use soap_client::{SoapClient, SoapEnvelope};

/// Strip any XML namespace prefix, returning only the local name.
pub(crate) fn local_name(name: &str) -> &str {
    name.rfind(':').map(|i| &name[i + 1..]).unwrap_or(name)
}

/// Accumulates the text content of one element across reader events.
///
/// quick-xml (>= 0.38) splits text around entity and character references:
/// `Smith &amp; Jones` arrives as `Text("Smith ")`, `GeneralRef("amp")`,
/// `Text(" Jones")`. Parsers route every event their `Start`/`End` arms do not
/// handle through [`push_if`](Self::push_if) and read the value once with
/// [`take`](Self::take) at the element's end.
/// Readers must not enable `trim_text`, or the spaces around a reference are lost.
#[derive(Debug, Default)]
pub(crate) struct ElementText(String);

impl ElementText {
    /// Append the content of a text, CDATA or reference event; other events are ignored.
    pub(crate) fn push(&mut self, event: &Event<'_>) -> Result<(), YukiError> {
        match event {
            Event::Text(e) => self.0.push_str(&e.xml10_content()),
            Event::CData(e) => self.0.push_str(&e.xml10_content()),
            Event::GeneralRef(e) => {
                if let Some(ch) = e
                    .resolve_char_ref()
                    .map_err(|err| YukiError::Xml(err.to_string()))?
                {
                    self.0.push(ch);
                } else if let Some(value) = resolve_xml_entity(e) {
                    self.0.push_str(value);
                } else {
                    return Err(YukiError::Xml(format!(
                        "unrecognized entity reference '&{};'",
                        &**e
                    )));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// [`push`](Self::push) the event while `inside` the element being read.
    ///
    /// Non-text events are ignored, so a parser's catch-all arm can hand over
    /// every leftover event without filtering it first.
    pub(crate) fn push_if(&mut self, inside: bool, event: &Event<'_>) -> Result<(), YukiError> {
        if inside { self.push(event) } else { Ok(()) }
    }

    /// Like [`push_if`](Self::push_if), but an unresolvable entity reference is
    /// dropped instead of failing: for best-effort text such as a SOAP fault
    /// message, where a parse error must not hide the fault itself.
    pub(crate) fn push_lossy_if(&mut self, inside: bool, event: &Event<'_>) {
        if inside {
            // An unresolvable reference is skipped; the rest of the text is kept.
            let _ = self.push(event);
        }
    }

    /// Return the accumulated text, trimmed, and reset for the next element.
    pub(crate) fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.0);
        text.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    use super::ElementText;

    /// Collect the text of every `<v>` element, using `ElementText` the way parsers do.
    fn values(xml: &str) -> Result<Vec<String>, crate::error::YukiError> {
        let mut reader = Reader::from_str(xml);
        let mut text = ElementText::default();
        let mut in_value = false;
        let mut out = Vec::new();
        loop {
            match reader.read_event().unwrap() {
                Event::Start(e) if e.name().as_ref() == "v" => in_value = true,
                Event::End(e) if e.name().as_ref() == "v" => {
                    in_value = false;
                    out.push(text.take());
                }
                Event::Eof => break,
                event => text.push_if(in_value, &event)?,
            }
        }
        Ok(out)
    }

    #[test]
    fn joins_text_split_by_entity_references() {
        let xml = "<r><v>Smith &amp; Jones fee</v><v>&lt;x&gt; &quot;q&quot; &apos;a&apos;</v></r>";
        assert_eq!(values(xml).unwrap(), ["Smith & Jones fee", "<x> \"q\" 'a'"]);
    }

    #[test]
    fn resolves_decimal_and_hex_character_references() {
        let xml = "<r><v>caf&#233; &#x2713;&#65;</v></r>";
        assert_eq!(values(xml).unwrap(), ["café ✓A"]);
    }

    #[test]
    fn trims_once_and_keeps_inner_whitespace() {
        let xml = "<r>\n  <v>\n   a &amp;  b \n </v>\n  <v>   </v>\n</r>";
        assert_eq!(values(xml).unwrap(), ["a &  b", ""]);
    }

    #[test]
    fn includes_cdata_sections() {
        let xml = "<r><v>a <![CDATA[<b> & c]]> &amp; d</v></r>";
        assert_eq!(values(xml).unwrap(), ["a <b> & c & d"]);
    }

    #[test]
    fn rejects_unknown_entities() {
        assert!(values("<r><v>&nbsp;</v></r>").is_err());
    }

    #[test]
    fn lossy_push_skips_unknown_entities_only() {
        let mut reader = Reader::from_str("<v>a &nbsp;b &amp; c</v>");
        let mut text = ElementText::default();
        loop {
            match reader.read_event().unwrap() {
                Event::Eof => break,
                event => text.push_lossy_if(true, &event),
            }
        }
        assert_eq!(text.take(), "a b & c");
    }
}
