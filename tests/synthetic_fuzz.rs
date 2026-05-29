use ccda_to_pdf::{render_ccda_xml_to_pdf, Color, RenderOptions};
use std::env;
use std::panic::{catch_unwind, AssertUnwindSafe};

const DEFAULT_VALID_CASES: usize = 500;
const DEFAULT_INVALID_CASES: usize = 250;

#[test]
fn synthetic_valid_ccdas_render_without_panics() {
    let cases = env_usize("CCDA_SYNTHETIC_VALID_CASES", DEFAULT_VALID_CASES);
    for seed in 0..cases as u64 {
        let xml = SyntheticCcda::new(seed).valid_document();
        let options = render_options(seed);
        let result = catch_unwind(AssertUnwindSafe(|| render_ccda_xml_to_pdf(&xml, &options)));

        let pdf = match result {
            Ok(Ok(pdf)) => pdf,
            Ok(Err(err)) => {
                panic!("seed {seed} returned an error for valid synthetic CCDA: {err}\n{xml}")
            }
            Err(payload) => panic!("seed {seed} panicked: {payload:?}\n{xml}"),
        };

        assert!(
            pdf.starts_with(b"%PDF-1.4"),
            "seed {seed} did not produce a PDF header"
        );
        assert!(
            pdf.ends_with(b"%%EOF\n"),
            "seed {seed} did not produce a PDF EOF marker"
        );
        assert!(
            pdf.len() > 1_000,
            "seed {seed} PDF was too small: {}",
            pdf.len()
        );
    }
}

#[test]
fn synthetic_invalid_inputs_return_errors_without_panics() {
    let cases = env_usize("CCDA_SYNTHETIC_INVALID_CASES", DEFAULT_INVALID_CASES);
    for seed in 0..cases as u64 {
        let xml = SyntheticCcda::new(seed).invalid_input();
        let result = catch_unwind(AssertUnwindSafe(|| {
            render_ccda_xml_to_pdf(&xml, &RenderOptions::default())
        }));

        match result {
            Ok(Ok(pdf)) => {
                assert!(
                    pdf.starts_with(b"%PDF-1.4"),
                    "seed {seed} returned non-PDF bytes"
                );
            }
            Ok(Err(_)) => {}
            Err(payload) => panic!("invalid seed {seed} panicked: {payload:?}\n{xml}"),
        }
    }
}

#[test]
fn hostile_table_spans_are_capped_and_rendered() {
    let xml = r#"
<ClinicalDocument xmlns="urn:hl7-org:v3">
  <title>Hostile span document</title>
  <component><structuredBody><component><section>
    <title>Hostile Table</title>
    <text><table><tbody>
      <tr><td colspan="999999999999999999999999999999999999">wide</td><td>tail</td></tr>
      <tr><td rowspan="999999999999999999999999999999999999">tall</td><td>next</td></tr>
      <tr><td>after</td></tr>
    </tbody></table></text>
  </section></component></structuredBody></component>
</ClinicalDocument>
"#;
    let pdf = render_ccda_xml_to_pdf(xml, &RenderOptions::default()).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4"));
    assert!(pdf.ends_with(b"%%EOF\n"));
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn render_options(seed: u64) -> RenderOptions {
    let mut rng = Rng::new(seed ^ 0xa5a5_1234_9876_fedc);
    RenderOptions {
        primary_color: Color::rgb_u8(rng.byte(), rng.byte(), rng.byte()),
        secondary_color: Color::rgb_u8(rng.byte(), rng.byte(), rng.byte()),
        logo: None,
    }
}

struct SyntheticCcda {
    rng: Rng,
}

impl SyntheticCcda {
    fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed.wrapping_add(0x9e37_79b9_7f4a_7c15)),
        }
    }

    fn valid_document(mut self) -> String {
        let title = self.pick([
            "Summary of Care",
            "Continuity of Care Document",
            "Discharge Summary",
            "Progress Note",
            "Referral Summary",
            "Synthetic Edge Case",
        ]);
        let patient = self.person_name();
        let author = self.person_name();
        let organization = self.pick([
            "Primary Care Partners",
            "Madison Medical Center",
            "Community Health and Hospitals",
            "Get Well Clinic",
            "Very Long Organization Name With Multiple Departments",
        ]);
        let section_count = self.rng.range(1, 10);

        let mut xml = String::new();
        xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        xml.push_str(r#"<ClinicalDocument xmlns="urn:hl7-org:v3" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#);
        xml.push_str(r#"<realmCode code="US"/><typeId root="2.16.840.1.113883.1.3" extension="POCD_HD000040"/>"#);
        xml.push_str(r#"<templateId root="2.16.840.1.113883.10.20.22.1.1"/>"#);
        xml.push_str(&format!(
            r#"<templateId root="{}"/><id root="{}" extension="{}"/>"#,
            self.pick([
                "2.16.840.1.113883.10.20.22.1.1",
                "2.16.840.1.113883.10.20.22.1.2",
                "2.16.840.1.113883.10.20.22.1.8",
                "2.16.840.1.113883.10.20.22.1.10",
            ]),
            self.oid(),
            self.rng.next()
        ));
        xml.push_str(&format!(
            r#"<code code="{}" displayName="{}" codeSystem="2.16.840.1.113883.6.1"/>"#,
            self.pick(["34133-9", "11488-4", "11506-3", "18842-5", "18761-7"]),
            escape_attr(title)
        ));
        xml.push_str(&format!(
            "<title>{}</title><effectiveTime value=\"{}\"/>",
            escape_text(title),
            self.timestamp()
        ));
        xml.push_str(r#"<recordTarget><patientRole>"#);
        xml.push_str(&format!(
            r#"<id root="{}" extension="{}"/>"#,
            self.oid(),
            self.maybe_long_id()
        ));
        if self.rng.bool() {
            xml.push_str(&self.address());
        }
        if self.rng.bool() {
            xml.push_str(&format!(r#"<telecom value="tel:{}"/>"#, self.phone()));
        }
        xml.push_str(&format!(
            r#"<patient><name><given>{}</given><family>{}</family></name><administrativeGenderCode code="{}" displayName="{}"/><birthTime value="{}"/></patient>"#,
            escape_text(patient.0),
            escape_text(patient.1),
            self.pick(["M", "F", "UN", "UNK"]),
            self.pick(["Male", "Female", "Undifferentiated", "Unknown"]),
            self.date(),
        ));
        xml.push_str(&format!(
            r#"<providerOrganization><name>{}</name>{}</providerOrganization>"#,
            escape_text(organization),
            self.address()
        ));
        xml.push_str(r#"</patientRole></recordTarget>"#);
        xml.push_str(&format!(
            r#"<author><time value="{}"/><assignedAuthor><assignedPerson><name><given>{}</given><family>{}</family></name></assignedPerson></assignedAuthor></author>"#,
            self.timestamp(),
            escape_text(author.0),
            escape_text(author.1),
        ));
        xml.push_str(&format!(
            r#"<custodian><assignedCustodian><representedCustodianOrganization><name>{}</name></representedCustodianOrganization></assignedCustodian></custodian>"#,
            escape_text(organization)
        ));

        if self.rng.one_in(8) {
            xml.push_str(&self.non_xml_body());
        } else {
            xml.push_str(r#"<component><structuredBody>"#);
            for idx in 0..section_count {
                xml.push_str(&self.section(idx));
            }
            xml.push_str(r#"</structuredBody></component>"#);
        }
        xml.push_str("</ClinicalDocument>");
        xml
    }

    fn invalid_input(mut self) -> String {
        match self.rng.range(0, 10) {
            0 => String::new(),
            1 => "<ClinicalDocument><broken></ClinicalDocument>".to_string(),
            2 => "<NotClinicalDocument/>".to_string(),
            3 => format!(
                "<ClinicalDocument><title>{}</title><component>",
                self.long_text(300)
            ),
            4 => "<ClinicalDocument><component><structuredBody><component><section><text><table><tr><td>&bad;</td></tr></table></text></section></component></structuredBody></component></ClinicalDocument>".to_string(),
            5 => format!(
                r#"<ClinicalDocument><component><nonXMLBody><text representation="B64">{}</text></nonXMLBody></component></ClinicalDocument>"#,
                self.long_text(80)
            ),
            6 => format!(
                r#"<ClinicalDocument><component><structuredBody>{}</structuredBody></component></ClinicalDocument>"#,
                "<component><section><text><table>".repeat(self.rng.range(1, 12))
            ),
            7 => format!(
                r#"<ClinicalDocument><title>{}</title><component><structuredBody/></component></ClinicalDocument>"#,
                "\u{0}".escape_default()
            ),
            8 => format!(
                r#"<ClinicalDocument><component><structuredBody><component><section><title>{}</title></section></component></structuredBody></component></ClinicalDocument>"#,
                self.long_text(16_000)
            ),
            _ => self.valid_document().replace("</ClinicalDocument>", ""),
        }
    }

    fn section(&mut self, idx: usize) -> String {
        let title = self.pick([
            "Allergies",
            "Medications",
            "Problems",
            "Results",
            "Vital Signs",
            "Plan of Care",
            "Procedures",
            "Encounters",
            "Social History",
            "Instructions",
        ]);
        let mut section = format!(
            r#"<component><section><templateId root="2.16.840.1.113883.10.20.22.2.{}"/><code code="{}" displayName="{}" codeSystem="2.16.840.1.113883.6.1"/><title>{}</title><text>"#,
            idx + 1,
            self.pick(["48765-2", "10160-0", "11450-4", "30954-2", "8716-3", "18776-5"]),
            escape_attr(title),
            escape_text(title),
        );

        let block_count = self.rng.range(1, 5);
        for _ in 0..block_count {
            match self.rng.range(0, 5) {
                0 => section.push_str(&self.paragraph()),
                1 => section.push_str(&self.list()),
                2 => section.push_str(&self.table()),
                3 => section.push_str(&self.nested_content()),
                _ => section.push_str(&format!(
                    r#"<content ID="{}">{}</content>"#,
                    self.id(),
                    escape_text(&self.clinical_text())
                )),
            }
        }
        section.push_str("</text>");
        if self.rng.bool() {
            section.push_str(&self.entry());
        }
        section.push_str("</section></component>");
        section
    }

    fn paragraph(&mut self) -> String {
        format!(
            "<paragraph>{}</paragraph>",
            escape_text(&self.clinical_text())
        )
    }

    fn list(&mut self) -> String {
        let mut list = String::from("<list>");
        if self.rng.bool() {
            list.push_str(&format!(
                "<caption>{}</caption>",
                escape_text(self.pick(["Active items", "Historical items", "Reviewed items"]))
            ));
        }
        for _ in 0..self.rng.range(1, 6) {
            list.push_str(&format!(
                "<item>{}</item>",
                escape_text(&self.clinical_text())
            ));
        }
        list.push_str("</list>");
        list
    }

    fn nested_content(&mut self) -> String {
        format!(
            r#"<content styleCode="{}"><content ID="{}">{}</content><br/><content>{}</content></content>"#,
            self.pick(["Bold", "Italics", "xmain", "xdetails"]),
            self.id(),
            escape_text(&self.clinical_text()),
            escape_text(&self.clinical_text())
        )
    }

    fn table(&mut self) -> String {
        let cols = self.rng.range(1, 9);
        let rows = self.rng.range(1, 9);
        let mut table = String::from("<table border=\"1\"><thead><tr>");
        let mut col = 0;
        while col < cols {
            let span = if self.rng.one_in(8) {
                self.rng.range(1, cols - col + 1)
            } else {
                1
            };
            table.push_str(&format!(
                r#"<th colspan="{}">{}</th>"#,
                span,
                escape_text(self.pick(["Name", "Dates", "Details", "Status", "Value", "Units"]))
            ));
            col += span;
        }
        table.push_str("</tr></thead><tbody>");
        for row_idx in 0..rows {
            table.push_str("<tr>");
            let mut col = 0;
            while col < cols {
                if self.rng.one_in(10) {
                    table.push_str("<td></td>");
                    col += 1;
                    continue;
                }
                let remaining = cols - col;
                let colspan = if remaining > 1 && self.rng.one_in(9) {
                    self.rng.range(1, remaining + 1)
                } else if self.rng.one_in(40) {
                    999_999
                } else {
                    1
                };
                let rowspan = if row_idx + 1 < rows && self.rng.one_in(12) {
                    self.rng.range(1, (rows - row_idx).min(4) + 1)
                } else if self.rng.one_in(40) {
                    999_999
                } else {
                    1
                };
                table.push_str(&format!(
                    r#"<td colspan="{}" rowspan="{}" ID="{}">{}</td>"#,
                    colspan,
                    rowspan,
                    self.id(),
                    escape_text(&self.clinical_text())
                ));
                col += colspan.min(remaining).max(1);
            }
            table.push_str("</tr>");
        }
        table.push_str("</tbody></table>");
        table
    }

    fn entry(&mut self) -> String {
        format!(
            r#"<entry><observation classCode="OBS" moodCode="EVN"><code code="{}" displayName="{}"/><statusCode code="{}"/><effectiveTime value="{}"/><value xsi:type="ST">{}</value></observation></entry>"#,
            self.pick(["ASSERTION", "55607006", "271649006", "75325-1"]),
            escape_attr(self.pick(["Problem", "Finding", "Medication", "Result"])),
            self.pick(["completed", "active", "aborted", "held"]),
            self.timestamp(),
            escape_text(&self.clinical_text())
        )
    }

    fn non_xml_body(&mut self) -> String {
        if self.rng.bool() {
            r#"<component><nonXMLBody><text><reference value="synthetic-note.pdf"/></text></nonXMLBody></component>"#.to_string()
        } else {
            r#"<component><nonXMLBody><text mediaType="text/plain" representation="B64">U3ludGhldGljIG5vbiBYTUwgYm9keQ==</text></nonXMLBody></component>"#.to_string()
        }
    }

    fn clinical_text(&mut self) -> String {
        match self.rng.range(0, 10) {
            0 => {
                let len = self.rng.range(60, 900);
                self.long_text(len)
            }
            1 => format!(
                "{} {}",
                self.pick(["Asthma", "Diabetes", "Hypertension", "Migraine"]),
                self.code()
            ),
            2 => format!(
                "{} {}",
                self.date(),
                self.pick(["Active", "Inactive", "Resolved", "Completed"])
            ),
            3 => "unicode punctuation: \u{201c}quoted\u{201d} \u{2014} degree 98.6\u{00b0}F"
                .to_string(),
            4 => "symbols that must be escaped: (paren) backslash \\ slash / percent %".to_string(),
            5 => format!(
                "{} mg by mouth {} times daily",
                self.rng.range(1, 500),
                self.rng.range(1, 6)
            ),
            6 => String::new(),
            _ => self
                .pick([
                    "No known allergies",
                    "Patient denies shortness of breath",
                    "Follow up in two weeks",
                    "Result reviewed by clinician",
                    "Medication reconciliation completed",
                ])
                .to_string(),
        }
    }

    fn person_name(&mut self) -> (&'static str, &'static str) {
        (
            self.pick(["Adam", "Sharon", "Henry", "Maria", "Li", "Avery", "Jordan"]),
            self.pick([
                "Everyman",
                "Carlson",
                "Seven",
                "Nguyen",
                "O'Connor",
                "Verylonglastname",
            ]),
        )
    }

    fn address(&mut self) -> String {
        format!(
            "<addr><streetAddressLine>{}</streetAddressLine><city>{}</city><state>{}</state><postalCode>{}</postalCode><country>US</country></addr>",
            escape_text(self.pick(["99 Forest Park", "123 Main St Apt 400", "No Fixed Address", "1004 Healthcare Dr. practice"])),
            escape_text(self.pick(["Chicago", "Portland", "Madison", "Blue Bell"])),
            self.pick(["IL", "OR", "CA", "MA", ""]),
            self.rng.range(10000, 99999),
        )
    }

    fn timestamp(&mut self) -> String {
        format!(
            "{}{}{}{}{}{}{}",
            self.rng.range(1990, 2035),
            two(self.rng.range(1, 13)),
            two(self.rng.range(1, 29)),
            two(self.rng.range(0, 24)),
            two(self.rng.range(0, 60)),
            two(self.rng.range(0, 60)),
            self.pick(["-0500", "-0400", "+0000", ""])
        )
    }

    fn date(&mut self) -> String {
        format!(
            "{}{}{}",
            self.rng.range(1920, 2025),
            two(self.rng.range(1, 13)),
            two(self.rng.range(1, 29))
        )
    }

    fn oid(&mut self) -> String {
        format!(
            "2.16.840.1.113883.3.{}.{}.{}",
            self.rng.range(1, 99999),
            self.rng.range(1, 99999),
            self.rng.range(1, 99999)
        )
    }

    fn maybe_long_id(&mut self) -> String {
        if self.rng.one_in(5) {
            let len = self.rng.range(32, 180);
            self.long_text(len)
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect()
        } else {
            self.rng.range(1_000_000, 9_999_999).to_string()
        }
    }

    fn id(&mut self) -> String {
        format!("id{}", self.rng.next())
    }

    fn code(&mut self) -> String {
        format!("({}.{})", self.rng.range(1, 999), self.rng.range(0, 99))
    }

    fn phone(&mut self) -> String {
        format!(
            "+1-555-{}-{}",
            self.rng.range(100, 999),
            self.rng.range(1000, 9999)
        )
    }

    fn long_text(&mut self, len: usize) -> String {
        let words = [
            "clinical",
            "history",
            "medication",
            "allergy",
            "result",
            "follow-up",
            "extraordinarilylongunbrokenclinicaltokenwithoutspaces",
            "normal",
            "abnormal",
            "reviewed",
        ];
        let mut out = String::new();
        while out.len() < len {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(self.pick(words));
        }
        out.truncate(len);
        out
    }

    fn pick<const N: usize>(&mut self, values: [&'static str; N]) -> &'static str {
        values[self.rng.range(0, N)]
    }
}

#[derive(Clone, Copy)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn range(&mut self, start: usize, end: usize) -> usize {
        assert!(start < end);
        start + (self.next() as usize % (end - start))
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    fn bool(&mut self) -> bool {
        self.next() & 1 == 0
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.range(0, n) == 0
    }
}

fn two(value: usize) -> String {
    format!("{value:02}")
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(value: &str) -> String {
    escape_text(value).replace('"', "&quot;")
}
