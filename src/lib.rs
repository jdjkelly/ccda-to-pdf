use roxmltree::{Document as XmlDocument, Node};
use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::io::{self, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const PAGE_W: f32 = 612.0;
const PAGE_H: f32 = 792.0;
const MARGIN: f32 = 48.0;
const BOTTOM_MARGIN: f32 = 48.0;
const CONTENT_W: f32 = PAGE_W - (MARGIN * 2.0);
const MAX_XML_DEPTH: usize = 512;
const MAX_TABLE_COLUMNS: usize = 64;
const MIN_RENDER_TABLE_CELL_WIDTH: f32 = 48.0;
const MAX_TABLE_SPAN: usize = 32;

static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(String),
    Xml(String),
    InvalidCcda(String),
    InvalidArgument(String),
    UnsupportedLogo(String),
    Pdf(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(message) => write!(f, "I/O error: {message}"),
            Error::Xml(message) => write!(f, "XML parse error: {message}"),
            Error::InvalidCcda(message) => write!(f, "invalid C-CDA: {message}"),
            Error::InvalidArgument(message) => write!(f, "invalid argument: {message}"),
            Error::UnsupportedLogo(message) => write!(f, "unsupported logo: {message}"),
            Error::Pdf(message) => write!(f, "PDF generation error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Color {
    pub const fn rgb_u8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
        }
    }

    pub fn parse(input: &str) -> Result<Self> {
        let raw = input.trim().strip_prefix('#').unwrap_or(input.trim());
        if raw.len() != 6 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::InvalidArgument(format!(
                "color '{input}' must be a 6-digit hex color like #0f766e"
            )));
        }
        let r = parse_hex_byte(&raw[0..2], input)?;
        let g = parse_hex_byte(&raw[2..4], input)?;
        let b = parse_hex_byte(&raw[4..6], input)?;
        Ok(Self::rgb_u8(r, g, b))
    }

    fn pdf_fill(self) -> String {
        format!("{:.3} {:.3} {:.3} rg\n", self.r, self.g, self.b)
    }

    fn pdf_stroke(self) -> String {
        format!("{:.3} {:.3} {:.3} RG\n", self.r, self.g, self.b)
    }

    fn tint(self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            r: self.r + ((1.0 - self.r) * amount),
            g: self.g + ((1.0 - self.g) * amount),
            b: self.b + ((1.0 - self.b) * amount),
        }
    }

    fn shade(self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            r: self.r * (1.0 - amount),
            g: self.g * (1.0 - amount),
            b: self.b * (1.0 - amount),
        }
    }
}

fn parse_hex_byte(raw: &str, original: &str) -> Result<u8> {
    u8::from_str_radix(raw, 16).map_err(|_| {
        Error::InvalidArgument(format!(
            "color '{original}' must be a 6-digit hex color like #0f766e"
        ))
    })
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub primary_color: Color,
    pub secondary_color: Color,
    pub logo: Option<LogoImage>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            primary_color: Color::rgb_u8(22, 87, 98),
            secondary_color: Color::rgb_u8(117, 141, 152),
            logo: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogoImage {
    width: u32,
    height: u32,
    data: Vec<u8>,
    filter: ImageFilter,
    color_space: ImageColorSpace,
    decode_params: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum ImageFilter {
    DctDecode,
    FlateDecode,
}

#[derive(Clone, Copy, Debug)]
enum ImageColorSpace {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
}

#[derive(Debug, Clone)]
pub struct CcdaDocument {
    pub title: String,
    pub effective_time: Option<String>,
    pub patient: Patient,
    pub author: Option<String>,
    pub custodian: Option<String>,
    pub sections: Vec<Section>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Patient {
    pub name: Option<String>,
    pub birth_time: Option<String>,
    pub gender: Option<String>,
    pub id: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub organization: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub code: Option<String>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Paragraph(String),
    List(Vec<String>),
    Table(Table),
    Note(String),
}

#[derive(Debug, Clone)]
pub struct Table {
    pub headers: Vec<TableCell>,
    pub rows: Vec<Vec<TableCell>>,
    pub column_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCell {
    pub text: String,
    pub colspan: usize,
}

impl TableCell {
    fn new(text: impl Into<String>, colspan: usize) -> Self {
        Self {
            text: text.into(),
            colspan: colspan.max(1),
        }
    }

    fn blank() -> Self {
        Self::new("", 1)
    }
}

pub fn run_cli() -> i32 {
    match run_cli_inner() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn run_cli_inner() -> Result<()> {
    let cli = CliOptions::parse(env::args().skip(1))?;
    if cli.help {
        print_usage();
        return Ok(());
    }

    let input = read_input(&cli.input)?;
    let doc = parse_ccda(&input)?;
    if cli.strict && doc.sections.iter().all(|section| section.blocks.is_empty()) {
        return Err(Error::InvalidCcda(
            "no printable structuredBody or nonXMLBody narrative was found".to_string(),
        ));
    }

    let pdf = render_document_to_pdf_panic_safe(&doc, &cli.render)?;
    write_output(&cli.output, &pdf)?;

    if !cli.quiet {
        eprintln!(
            "Converted {} section(s) to {} bytes of PDF{}",
            doc.sections.len(),
            pdf.len(),
            if doc.warnings.is_empty() {
                String::new()
            } else {
                format!(" with {} warning(s)", doc.warnings.len())
            }
        );
        for warning in &doc.warnings {
            eprintln!("warning: {warning}");
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct CliOptions {
    input: IoTarget,
    output: IoTarget,
    render: RenderOptions,
    strict: bool,
    quiet: bool,
    help: bool,
}

#[derive(Debug, Clone)]
enum IoTarget {
    Stdio,
    Path(PathBuf),
    Missing,
}

impl CliOptions {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let mut input = IoTarget::Missing;
        let mut output = IoTarget::Missing;
        let mut render = RenderOptions::default();
        let mut strict = false;
        let mut quiet = false;
        let mut help = false;
        let mut positionals = Vec::new();

        let mut args = args.into_iter().map(Into::into).peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => help = true,
                "--strict" => strict = true,
                "-q" | "--quiet" => quiet = true,
                "--primary-color" => {
                    let value = next_value(&mut args, "--primary-color")?;
                    render.primary_color = Color::parse(&value)?;
                }
                "--secondary-color" => {
                    let value = next_value(&mut args, "--secondary-color")?;
                    render.secondary_color = Color::parse(&value)?;
                }
                "--logo" => {
                    let value = next_value(&mut args, "--logo")?;
                    render.logo = Some(load_logo(value)?);
                }
                "--input" | "-i" => {
                    let value = next_value(&mut args, "--input")?;
                    input = parse_io_target(&value);
                }
                "--output" | "-o" => {
                    let value = next_value(&mut args, "--output")?;
                    output = parse_io_target(&value);
                }
                _ if arg.starts_with("--primary-color=") => {
                    render.primary_color = Color::parse(value_after_equals(&arg)?)?;
                }
                _ if arg.starts_with("--secondary-color=") => {
                    render.secondary_color = Color::parse(value_after_equals(&arg)?)?;
                }
                _ if arg.starts_with("--logo=") => {
                    render.logo = Some(load_logo(value_after_equals(&arg)?)?);
                }
                _ if arg.starts_with("--input=") => {
                    input = parse_io_target(value_after_equals(&arg)?)
                }
                _ if arg.starts_with("--output=") => {
                    output = parse_io_target(value_after_equals(&arg)?)
                }
                _ if arg.starts_with('-') && arg != "-" => {
                    return Err(Error::InvalidArgument(format!("unknown option '{arg}'")));
                }
                _ => positionals.push(arg),
            }
        }

        if help {
            return Ok(Self {
                input,
                output,
                render,
                strict,
                quiet,
                help,
            });
        }

        if matches!(input, IoTarget::Missing) {
            if let Some(value) = positionals.get(0) {
                input = parse_io_target(value);
            }
        }
        if matches!(output, IoTarget::Missing) {
            if let Some(value) = positionals.get(1) {
                output = parse_io_target(value);
            }
        }
        if positionals.len() > 2 {
            return Err(Error::InvalidArgument(format!(
                "expected at most input and output paths, got {} positional arguments",
                positionals.len()
            )));
        }
        if matches!(input, IoTarget::Missing) || matches!(output, IoTarget::Missing) {
            return Err(Error::InvalidArgument(
                "usage: ccda-to-pdf [options] <input.xml|- > <output.pdf|- >".to_string(),
            ));
        }

        Ok(Self {
            input,
            output,
            render,
            strict,
            quiet,
            help,
        })
    }
}

fn next_value<I>(args: &mut std::iter::Peekable<I>, flag: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| Error::InvalidArgument(format!("{flag} requires a value")))
}

fn value_after_equals(arg: &str) -> Result<&str> {
    arg.split_once('=')
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::InvalidArgument(format!("{arg} requires a value after '='")))
}

fn parse_io_target(value: &str) -> IoTarget {
    if value == "-" {
        IoTarget::Stdio
    } else {
        IoTarget::Path(PathBuf::from(value))
    }
}

fn read_input(target: &IoTarget) -> Result<String> {
    let bytes = match target {
        IoTarget::Stdio => {
            let mut input = Vec::new();
            io::stdin()
                .read_to_end(&mut input)
                .map_err(|err| Error::Io(format!("failed to read stdin: {err}")))?;
            input
        }
        IoTarget::Path(path) => fs::read(path)
            .map_err(|err| Error::Io(format!("failed to read {}: {err}", path.display())))?,
        IoTarget::Missing => unreachable!("missing target is rejected during CLI parsing"),
    };
    Ok(decode_xml_bytes(&bytes))
}

fn write_output(target: &IoTarget, bytes: &[u8]) -> Result<()> {
    match target {
        IoTarget::Stdio => io::stdout()
            .write_all(bytes)
            .map_err(|err| Error::Io(format!("failed to write stdout: {err}"))),
        IoTarget::Path(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent).map_err(|err| {
                        Error::Io(format!("failed to create {}: {err}", parent.display()))
                    })?;
                }
            }
            fs::write(path, bytes)
                .map_err(|err| Error::Io(format!("failed to write {}: {err}", path.display())))
        }
        IoTarget::Missing => unreachable!("missing target is rejected during CLI parsing"),
    }
}

fn decode_xml_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(&bytes[3..]).into_owned()
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        decode_utf16_lossy(&bytes[2..], true)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        decode_utf16_lossy(&bytes[2..], false)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn decode_utf16_lossy(bytes: &[u8], little_endian: bool) -> String {
    let mut units = Vec::with_capacity((bytes.len() + 1) / 2);
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        let pair = [chunk[0], chunk[1]];
        let unit = if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        };
        units.push(unit);
    }

    let mut decoded = String::from_utf16_lossy(&units);
    if !chunks.remainder().is_empty() {
        decoded.push('\u{FFFD}');
    }
    decoded
}

fn print_usage() {
    eprintln!(
        "Usage: ccda-to-pdf [options] <input.xml|- > <output.pdf|- >\n\
\n\
Options:\n\
  -i, --input <path|->             Read C-CDA XML from a file or stdin\n\
  -o, --output <path|->            Write PDF to a file or stdout\n\
      --logo <path>                Add a JPEG or 8-bit RGB/grayscale PNG logo\n\
      --primary-color <#RRGGBB>    Header and section color\n\
      --secondary-color <#RRGGBB>  Accent and table color\n\
      --strict                     Fail if no printable body narrative exists\n\
  -q, --quiet                      Suppress conversion summary on stderr\n\
  -h, --help                       Show this help"
    );
}

fn preflight_xml(xml: &str) -> Result<()> {
    if contains_ascii_case_insensitive(xml, "<!DOCTYPE")
        || contains_ascii_case_insensitive(xml, "<!ENTITY")
    {
        return Err(Error::InvalidCcda(
            "DTD and entity declarations are not supported".to_string(),
        ));
    }

    let bytes = xml.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }

        if starts_with_at(bytes, i, b"<!--") {
            i = find_delimiter_end(bytes, i + 4, b"-->").unwrap_or(bytes.len());
            continue;
        }
        if starts_with_at(bytes, i, b"<![CDATA[") {
            i = find_delimiter_end(bytes, i + 9, b"]]>").unwrap_or(bytes.len());
            continue;
        }
        if starts_with_at(bytes, i, b"<?") {
            i = find_delimiter_end(bytes, i + 2, b"?>").unwrap_or(bytes.len());
            continue;
        }

        let Some(end) = find_tag_end(bytes, i + 1) else {
            break;
        };

        if starts_with_at(bytes, i, b"</") {
            depth = depth.saturating_sub(1);
            i = end + 1;
            continue;
        }
        if starts_with_at(bytes, i, b"<!") {
            i = end + 1;
            continue;
        }

        if !is_self_closing_tag(&bytes[i + 1..end]) {
            depth += 1;
            if depth > MAX_XML_DEPTH {
                return Err(Error::InvalidCcda(format!(
                    "XML nesting exceeds safety limit of {MAX_XML_DEPTH}"
                )));
            }
        }
        i = end + 1;
    }

    Ok(())
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn starts_with_at(haystack: &[u8], offset: usize, needle: &[u8]) -> bool {
    haystack
        .get(offset..offset.saturating_add(needle.len()))
        .is_some_and(|window| window == needle)
}

fn find_delimiter_end(haystack: &[u8], start: usize, delimiter: &[u8]) -> Option<usize> {
    haystack
        .get(start..)?
        .windows(delimiter.len())
        .position(|window| window == delimiter)
        .map(|idx| start + idx + delimiter.len())
}

fn find_tag_end(bytes: &[u8], mut offset: usize) -> Option<usize> {
    let mut quote = None;
    while offset < bytes.len() {
        match (quote, bytes[offset]) {
            (Some(current), byte) if byte == current => quote = None,
            (None, b'"') | (None, b'\'') => quote = Some(bytes[offset]),
            (None, b'>') => return Some(offset),
            _ => {}
        }
        offset += 1;
    }
    None
}

fn is_self_closing_tag(tag_body: &[u8]) -> bool {
    tag_body
        .iter()
        .rev()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'/')
}

pub fn parse_ccda(xml: &str) -> Result<CcdaDocument> {
    preflight_xml(xml)?;
    let document = XmlDocument::parse(xml).map_err(|err| Error::Xml(err.to_string()))?;
    let root = document.root_element();
    if !has_tag(root, "ClinicalDocument") {
        return Err(Error::InvalidCcda(format!(
            "root element must be ClinicalDocument, found {}",
            root.tag_name().name()
        )));
    }

    let title = direct_child(root, "title")
        .map(text_content)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            direct_child(root, "code")
                .and_then(|node| attr(&node, "displayName"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Clinical Document".to_string());

    let effective_time = direct_child(root, "effectiveTime")
        .and_then(|node| attr(&node, "value"))
        .map(format_hl7_timestamp);

    let patient = parse_patient(root);
    let author = parse_author(root);
    let custodian = parse_custodian(root);
    let mut warnings = Vec::new();
    let mut sections = parse_structured_sections(root);

    if sections.is_empty() {
        let non_xml_sections = parse_non_xml_body(root, &mut warnings);
        sections.extend(non_xml_sections);
    }
    if sections.is_empty() {
        warnings.push("no structuredBody sections or nonXMLBody content were found".to_string());
        sections.push(Section {
            title: "Document Body".to_string(),
            code: None,
            blocks: vec![Block::Note(
                "This C-CDA did not contain printable narrative content.".to_string(),
            )],
        });
    }

    Ok(CcdaDocument {
        title,
        effective_time,
        patient,
        author,
        custodian,
        sections,
        warnings,
    })
}

fn parse_patient(root: Node<'_, '_>) -> Patient {
    let patient_role = root
        .descendants()
        .find(|node| has_tag(*node, "patientRole"));
    let patient_node = patient_role.and_then(|role| direct_child(role, "patient"));

    let name = patient_node
        .and_then(|patient| direct_child(patient, "name"))
        .map(human_name)
        .filter(|value| !value.is_empty());
    let birth_time = patient_node
        .and_then(|patient| direct_child(patient, "birthTime"))
        .and_then(|node| attr(&node, "value"))
        .map(format_hl7_timestamp);
    let gender = patient_node
        .and_then(|patient| direct_child(patient, "administrativeGenderCode"))
        .and_then(|node| {
            attr(&node, "displayName")
                .map(str::to_string)
                .or_else(|| attr(&node, "code").map(gender_from_code))
        });
    let id = patient_role
        .and_then(|role| direct_child(role, "id"))
        .and_then(|node| {
            let extension = attr(&node, "extension");
            let root = attr(&node, "root");
            match (extension, root) {
                (Some(extension), Some(root)) => Some(format!("{extension} ({root})")),
                (Some(extension), None) => Some(extension.to_string()),
                (None, Some(root)) => Some(root.to_string()),
                _ => None,
            }
        });
    let phone = patient_role
        .and_then(|role| direct_child(role, "telecom"))
        .and_then(|node| attr(&node, "value"))
        .map(clean_telecom);
    let address = patient_role
        .and_then(|role| direct_child(role, "addr"))
        .map(format_address)
        .filter(|value| !value.is_empty());
    let organization = patient_role
        .and_then(|role| direct_child(role, "providerOrganization"))
        .and_then(|org| direct_child(org, "name"))
        .map(text_content)
        .filter(|value| !value.is_empty());

    Patient {
        name,
        birth_time,
        gender,
        id,
        phone,
        address,
        organization,
    }
}

fn parse_author(root: Node<'_, '_>) -> Option<String> {
    let author = root.descendants().find(|node| has_tag(*node, "author"))?;
    author
        .descendants()
        .find(|node| has_tag(*node, "assignedPerson"))
        .and_then(|person| direct_child(person, "name"))
        .map(human_name)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            author
                .descendants()
                .find(|node| has_tag(*node, "assignedAuthoringDevice"))
                .and_then(|device| direct_child(device, "softwareName"))
                .map(text_content)
                .filter(|value| !value.is_empty())
        })
}

fn parse_custodian(root: Node<'_, '_>) -> Option<String> {
    root.descendants()
        .find(|node| has_tag(*node, "representedCustodianOrganization"))
        .and_then(|org| direct_child(org, "name"))
        .map(text_content)
        .filter(|value| !value.is_empty())
}

fn parse_structured_sections(root: Node<'_, '_>) -> Vec<Section> {
    root.descendants()
        .filter(|node| has_tag(*node, "section") && is_top_level_section(*node))
        .map(parse_section)
        .collect()
}

fn is_top_level_section(section: Node<'_, '_>) -> bool {
    let mut current = section.parent();
    while let Some(node) = current {
        if has_tag(node, "section") {
            return false;
        }
        if has_tag(node, "structuredBody") {
            return true;
        }
        current = node.parent();
    }
    false
}

fn parse_section(section: Node<'_, '_>) -> Section {
    let title = direct_child(section, "title")
        .map(text_content)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            direct_child(section, "code")
                .and_then(|code| attr(&code, "displayName"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Untitled Section".to_string());
    let code = direct_child(section, "code").and_then(|node| {
        let code = attr(&node, "code");
        let display_name = attr(&node, "displayName");
        match (code, display_name) {
            (Some(code), Some(display_name)) => Some(format!("{display_name} ({code})")),
            (Some(code), None) => Some(code.to_string()),
            (None, Some(display_name)) => Some(display_name.to_string()),
            _ => None,
        }
    });

    let mut blocks = direct_child(section, "text")
        .map(parse_narrative_blocks)
        .unwrap_or_default();
    if blocks.is_empty() {
        blocks = fallback_blocks_from_entries(section);
    }
    if blocks.is_empty() {
        blocks.push(Block::Note(
            "No narrative text was provided for this section.".to_string(),
        ));
    }

    Section {
        title,
        code,
        blocks,
    }
}

fn parse_non_xml_body(root: Node<'_, '_>, warnings: &mut Vec<String>) -> Vec<Section> {
    root.descendants()
        .filter(|node| has_tag(*node, "nonXMLBody"))
        .map(|body| {
            let text = body.descendants().find(|node| has_tag(*node, "text"));
            let mut blocks = Vec::new();
            if let Some(text) = text {
                if let Some(reference) = text
                    .descendants()
                    .find(|node| has_tag(*node, "reference"))
                    .and_then(|node| attr(&node, "value"))
                {
                    blocks.push(Block::Note(format!(
                        "This C-CDA references an external body document: {reference}"
                    )));
                }

                let media_type = attr(&text, "mediaType").unwrap_or("text/plain");
                let representation = attr(&text, "representation");
                let content = text_content(text);
                if representation == Some("B64") {
                    if media_type.starts_with("text/") {
                        match decode_base64(content.as_bytes())
                            .and_then(|bytes| String::from_utf8(bytes).map_err(|err| err.to_string()))
                        {
                            Ok(decoded) if !normalize_ws(&decoded).is_empty() => {
                                blocks.push(Block::Paragraph(normalize_ws(&decoded)));
                            }
                            Ok(_) => {}
                            Err(err) => warnings.push(format!(
                                "nonXMLBody text/plain base64 content could not be decoded: {err}"
                            )),
                        }
                    } else {
                        blocks.push(Block::Note(format!(
                            "Embedded nonXMLBody media ({media_type}) is present but is not rendered inline."
                        )));
                    }
                } else if !content.is_empty() && blocks.is_empty() {
                    blocks.push(Block::Paragraph(content));
                }
            }

            if blocks.is_empty() {
                blocks.push(Block::Note(
                    "A nonXMLBody element was present but did not contain printable text.".to_string(),
                ));
            }
            Section {
                title: "Unstructured Body".to_string(),
                code: None,
                blocks,
            }
        })
        .collect()
}

fn parse_narrative_blocks(text_node: Node<'_, '_>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut text_buffer = String::new();

    for child in text_node.children() {
        if child.is_text() {
            text_buffer.push_str(child.text().unwrap_or_default());
            text_buffer.push(' ');
            continue;
        }
        if !child.is_element() {
            continue;
        }

        flush_paragraph(&mut text_buffer, &mut blocks);
        append_element_block(child, &mut blocks);
    }
    flush_paragraph(&mut text_buffer, &mut blocks);

    if blocks.is_empty() {
        let fallback = text_content(text_node);
        if !fallback.is_empty() {
            blocks.push(Block::Paragraph(fallback));
        }
    }

    blocks
}

fn append_element_block(node: Node<'_, '_>, blocks: &mut Vec<Block>) {
    match node.tag_name().name() {
        "table" => {
            let table = parse_table(node);
            if !table.headers.is_empty() || !table.rows.is_empty() {
                blocks.push(Block::Table(table));
            }
        }
        "list" => {
            if let Some(caption) = direct_child(node, "caption").map(text_content) {
                if !caption.is_empty() {
                    blocks.push(Block::Paragraph(caption));
                }
            }
            let items: Vec<String> = node
                .children()
                .filter(|child| has_tag(*child, "item"))
                .map(text_content)
                .filter(|value| !value.is_empty())
                .collect();
            if !items.is_empty() {
                blocks.push(Block::List(items));
            }
        }
        "paragraph" | "content" | "caption" => {
            let paragraph = text_content(node);
            if !paragraph.is_empty() {
                blocks.push(Block::Paragraph(paragraph));
            }
        }
        "br" => {}
        "renderMultiMedia" | "reference" => {
            if let Some(value) = attr(&node, "referencedObject")
                .or_else(|| attr(&node, "value"))
                .filter(|value| !value.is_empty())
            {
                blocks.push(Block::Note(format!("Media reference: {value}")));
            }
        }
        _ => {
            let mut added_structured_child = false;
            for child in node.children().filter(|child| child.is_element()) {
                if matches!(child.tag_name().name(), "table" | "list" | "paragraph") {
                    append_element_block(child, blocks);
                    added_structured_child = true;
                }
            }
            if !added_structured_child {
                let value = text_content(node);
                if !value.is_empty() {
                    blocks.push(Block::Paragraph(value));
                }
            }
        }
    }
}

fn flush_paragraph(buffer: &mut String, blocks: &mut Vec<Block>) {
    let paragraph = normalize_ws(buffer);
    if !paragraph.is_empty() {
        blocks.push(Block::Paragraph(paragraph));
    }
    buffer.clear();
}

fn parse_table(table: Node<'_, '_>) -> Table {
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut column_count = 0usize;
    let mut rowspans: Vec<usize> = Vec::new();

    for tr in table
        .descendants()
        .filter(|node| has_tag(*node, "tr") && nearest_ancestor(*node, "table") == Some(table))
    {
        let mut row = Vec::new();
        let mut has_header_cell = false;
        let mut col = 0usize;

        for cell in tr
            .children()
            .filter(|node| has_tag(*node, "th") || has_tag(*node, "td"))
        {
            if col >= MAX_TABLE_COLUMNS {
                break;
            }
            while rowspans.get(col).copied().unwrap_or(0) > 0 {
                row.push(TableCell::blank());
                if let Some(remaining) = rowspans.get_mut(col) {
                    *remaining -= 1;
                }
                col += 1;
                if col >= MAX_TABLE_COLUMNS {
                    break;
                }
            }
            if col >= MAX_TABLE_COLUMNS {
                break;
            }

            let colspan = parse_span_attr(cell, "colspan").min(MAX_TABLE_COLUMNS - col);
            let rowspan = parse_span_attr(cell, "rowspan");
            has_header_cell |= has_tag(cell, "th");
            row.push(TableCell::new(text_content(cell), colspan));
            if rowspan > 1 {
                let span_end = col.saturating_add(colspan).min(MAX_TABLE_COLUMNS);
                if rowspans.len() < span_end {
                    rowspans.resize(span_end, 0);
                }
                for pending in &mut rowspans[col..span_end] {
                    *pending = (*pending).max(rowspan - 1);
                }
            }
            col = col.saturating_add(colspan).min(MAX_TABLE_COLUMNS);
        }

        while rowspans.get(col).copied().unwrap_or(0) > 0 {
            row.push(TableCell::blank());
            if let Some(remaining) = rowspans.get_mut(col) {
                *remaining -= 1;
            }
            col += 1;
            if col >= MAX_TABLE_COLUMNS {
                break;
            }
        }

        if row.is_empty() || row.iter().all(|cell| cell.text.is_empty()) {
            continue;
        }

        column_count = column_count.max(row_column_count(&row));
        let in_header = tr.ancestors().any(|ancestor| {
            has_tag(ancestor, "thead") && nearest_ancestor(ancestor, "table") == Some(table)
        });
        if headers.is_empty() && (in_header || has_header_cell) {
            headers = row;
        } else {
            rows.push(row);
        }
    }

    Table {
        headers,
        rows,
        column_count,
    }
}

fn parse_span_attr(node: Node<'_, '_>, name: &str) -> usize {
    attr(&node, name)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(MAX_TABLE_SPAN))
        .unwrap_or(1)
}

fn row_column_count(row: &[TableCell]) -> usize {
    row.iter()
        .map(|cell| cell.colspan.max(1))
        .fold(0usize, |total, span| total.saturating_add(span))
        .min(MAX_TABLE_COLUMNS)
}

fn nearest_ancestor<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.ancestors()
        .skip(1)
        .find(|ancestor| has_tag(*ancestor, tag))
}

fn fallback_blocks_from_entries(section: Node<'_, '_>) -> Vec<Block> {
    let mut rows = Vec::new();
    for entry in section
        .descendants()
        .filter(|node| {
            matches!(
                node.tag_name().name(),
                "act"
                    | "encounter"
                    | "observation"
                    | "organizer"
                    | "procedure"
                    | "substanceAdministration"
            )
        })
        .take(80)
    {
        let kind = entry.tag_name().name().to_string();
        let description = entry
            .children()
            .find(|node| has_tag(*node, "code"))
            .and_then(|node| {
                attr(&node, "displayName")
                    .map(str::to_string)
                    .or_else(|| attr(&node, "code").map(str::to_string))
            })
            .or_else(|| {
                entry
                    .children()
                    .find(|node| has_tag(*node, "value"))
                    .and_then(|node| attr(&node, "displayName").map(str::to_string))
            });
        let date = entry
            .children()
            .find(|node| has_tag(*node, "effectiveTime"))
            .and_then(extract_time_value)
            .map(format_hl7_timestamp);
        let status = entry
            .children()
            .find(|node| has_tag(*node, "statusCode"))
            .and_then(|node| attr(&node, "code"))
            .map(str::to_string);

        if description.is_some() || date.is_some() || status.is_some() {
            rows.push(vec![
                TableCell::new(split_camel_case(&kind), 1),
                TableCell::new(description.unwrap_or_default(), 1),
                TableCell::new(date.unwrap_or_default(), 1),
                TableCell::new(status.unwrap_or_default(), 1),
            ]);
        }
    }

    if rows.is_empty() {
        Vec::new()
    } else {
        vec![Block::Table(Table {
            headers: strings_to_cells(["Type", "Description", "Date", "Status"]),
            rows,
            column_count: 4,
        })]
    }
}

fn strings_to_cells<const N: usize>(values: [&str; N]) -> Vec<TableCell> {
    values
        .into_iter()
        .map(|value| TableCell::new(value, 1))
        .collect()
}

fn extract_time_value<'a, 'input>(node: Node<'a, 'input>) -> Option<&'a str> {
    attr(&node, "value").or_else(|| {
        node.children()
            .find(|child| has_tag(*child, "low"))
            .and_then(|low| attr(&low, "value"))
    })
}

pub fn render_pdf(document: &CcdaDocument, options: &RenderOptions) -> Result<Vec<u8>> {
    let mut layout = PdfLayout::new(document, options);
    layout.render_document(document);
    let pages = layout.finish();
    PdfWriter::write(pages, options.logo.as_ref())
}

pub fn render_ccda_xml_to_pdf(xml: &str, options: &RenderOptions) -> Result<Vec<u8>> {
    let document = parse_ccda(xml)?;
    render_document_to_pdf_panic_safe(&document, options)
}

pub fn render_document_to_pdf_panic_safe(
    document: &CcdaDocument,
    options: &RenderOptions,
) -> Result<Vec<u8>> {
    render_document_to_pdf_panic_safe_with(document, options, render_pdf)
}

fn render_document_to_pdf_panic_safe_with<F>(
    document: &CcdaDocument,
    options: &RenderOptions,
    render_normal: F,
) -> Result<Vec<u8>>
where
    F: FnOnce(&CcdaDocument, &RenderOptions) -> Result<Vec<u8>>,
{
    match catch_unwind_silently(|| render_normal(document, options)) {
        Ok(result) => result,
        Err(_) => {
            let safe_document = safe_mode_document(document);
            match catch_unwind_silently(|| render_pdf(&safe_document, options)) {
                Ok(result) => result,
                Err(_) => Err(Error::Pdf(
                    "PDF rendering panicked in normal and safe mode".to_string(),
                )),
            }
        }
    }
}

fn catch_unwind_silently<F, T>(f: F) -> std::thread::Result<T>
where
    F: FnOnce() -> T,
{
    let _guard = PANIC_HOOK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    panic::set_hook(hook);
    result
}

fn safe_mode_document(document: &CcdaDocument) -> CcdaDocument {
    let mut safe = document.clone();
    safe.warnings.push(
        "Normal table layout failed; rendered tables as stacked text in safe mode.".to_string(),
    );

    for section in &mut safe.sections {
        let mut blocks = Vec::new();
        for block in &section.blocks {
            match block {
                Block::Table(table) => {
                    let lines = table_to_label_value_lines(table);
                    if lines.is_empty() {
                        blocks.push(Block::Note(
                            "A table was present but contained no printable cells.".to_string(),
                        ));
                    } else {
                        blocks.push(Block::List(lines));
                    }
                }
                _ => blocks.push(block.clone()),
            }
        }
        section.blocks = blocks;
    }

    safe
}

struct PdfLayout<'a> {
    options: &'a RenderOptions,
    pages: Vec<String>,
    current: String,
    y: f32,
    page_number: usize,
    document_title: String,
    patient_name: String,
}

impl<'a> PdfLayout<'a> {
    fn new(document: &CcdaDocument, options: &'a RenderOptions) -> Self {
        let patient_name = document
            .patient
            .name
            .clone()
            .unwrap_or_else(|| "Unknown patient".to_string());
        let mut layout = Self {
            options,
            pages: Vec::new(),
            current: String::new(),
            y: 0.0,
            page_number: 0,
            document_title: document.title.clone(),
            patient_name,
        };
        layout.start_page(true);
        layout
    }

    fn finish(mut self) -> Vec<String> {
        if !self.current.is_empty() {
            self.pages.push(self.current);
        }
        self.pages
    }

    fn render_document(&mut self, document: &CcdaDocument) {
        self.render_summary(document);
        for section in &document.sections {
            self.render_section(section);
        }
        if !document.warnings.is_empty() {
            self.render_section(&Section {
                title: "Conversion Warnings".to_string(),
                code: None,
                blocks: document
                    .warnings
                    .iter()
                    .map(|warning| Block::Note(warning.clone()))
                    .collect(),
            });
        }
    }

    fn start_page(&mut self, first_page: bool) {
        if !self.current.is_empty() {
            self.pages.push(std::mem::take(&mut self.current));
        }
        self.page_number += 1;
        self.current = String::new();
        self.rect(0.0, PAGE_H - 20.0, PAGE_W, 20.0, self.options.primary_color);

        if let Some(logo) = &self.options.logo {
            let max_w = 108.0;
            let max_h = if first_page { 48.0 } else { 32.0 };
            let scale = (max_w / logo.width as f32).min(max_h / logo.height as f32);
            let w = logo.width as f32 * scale;
            let h = logo.height as f32 * scale;
            self.image(MARGIN, PAGE_H - MARGIN - h, w, h);
        }

        let title_x = if self.options.logo.is_some() {
            MARGIN + 124.0
        } else {
            MARGIN
        };
        let title_w = PAGE_W - title_x - MARGIN;
        let title_size = if first_page { 19.0 } else { 12.0 };
        let title_lines = wrap_text(&self.document_title, title_w, title_size, true);
        let mut title_y = PAGE_H - MARGIN - if first_page { 3.0 } else { 0.0 };
        for line in title_lines.into_iter().take(if first_page { 2 } else { 1 }) {
            self.text(
                title_x,
                title_y,
                &line,
                "F2",
                title_size,
                self.options.primary_color.shade(0.15),
            );
            title_y -= title_size * 1.2;
        }

        let subtitle = format!("{}    Page {}", self.patient_name, self.page_number);
        self.text(
            title_x,
            title_y - 4.0,
            &subtitle,
            "F1",
            9.0,
            Color::rgb_u8(75, 85, 99),
        );

        let line_y = if first_page {
            PAGE_H - 116.0
        } else {
            PAGE_H - 82.0
        };
        self.line(
            MARGIN,
            line_y,
            PAGE_W - MARGIN,
            line_y,
            self.options.secondary_color,
            1.0,
        );
        self.y = line_y - 20.0;
    }

    fn ensure_space(&mut self, height: f32) {
        if self.y - height < BOTTOM_MARGIN {
            self.start_page(false);
        }
    }

    fn render_summary(&mut self, document: &CcdaDocument) {
        self.ensure_space(112.0);
        self.text(
            MARGIN,
            self.y,
            "Document Summary",
            "F2",
            12.0,
            self.options.primary_color,
        );
        self.y -= 18.0;

        let left = vec![
            labeled("Patient", document.patient.name.as_deref()),
            labeled("Date of birth", document.patient.birth_time.as_deref()),
            labeled("Gender", document.patient.gender.as_deref()),
            labeled("Patient ID", document.patient.id.as_deref()),
        ];
        let right = vec![
            labeled("Document date", document.effective_time.as_deref()),
            labeled("Author", document.author.as_deref()),
            labeled("Custodian", document.custodian.as_deref()),
            labeled("Organization", document.patient.organization.as_deref()),
        ];
        let mut rows = left.len().max(right.len());
        if rows == 0 {
            rows = 1;
        }
        let column_gap = 30.0;
        let column_w = (CONTENT_W - column_gap) / 2.0;
        let right_x = MARGIN + column_w + column_gap;
        let line_height = 11.5;
        for idx in 0..rows {
            let left_lines = left
                .get(idx)
                .map(|value| wrap_text(value, column_w, 9.5, false))
                .unwrap_or_default();
            let right_lines = right
                .get(idx)
                .map(|value| wrap_text(value, column_w, 9.5, false))
                .unwrap_or_default();
            let line_count = left_lines.len().max(right_lines.len()).max(1);
            self.ensure_space((line_count as f32 * line_height) + 2.0);

            let row_y = self.y;
            for (line_idx, line) in left_lines.iter().enumerate() {
                self.text(
                    MARGIN,
                    row_y - (line_idx as f32 * line_height),
                    line,
                    "F1",
                    9.5,
                    Color::rgb_u8(31, 41, 55),
                );
            }
            for (line_idx, line) in right_lines.iter().enumerate() {
                self.text(
                    right_x,
                    row_y - (line_idx as f32 * line_height),
                    line,
                    "F1",
                    9.5,
                    Color::rgb_u8(31, 41, 55),
                );
            }
            self.y -= (line_count as f32 * line_height) + 3.0;
        }

        if let Some(address) = &document.patient.address {
            self.draw_wrapped_text(
                &format!("Address: {address}"),
                MARGIN,
                CONTENT_W,
                9.5,
                "F1",
                Color::rgb_u8(31, 41, 55),
                11.5,
            );
        }
        if let Some(phone) = &document.patient.phone {
            self.draw_wrapped_text(
                &format!("Phone: {phone}"),
                MARGIN,
                CONTENT_W,
                9.5,
                "F1",
                Color::rgb_u8(31, 41, 55),
                11.5,
            );
        }
        self.y -= 10.0;
    }

    fn render_section(&mut self, section: &Section) {
        self.ensure_space(46.0);
        let heading_h = 24.0;
        self.rect(
            MARGIN,
            self.y - heading_h + 6.0,
            CONTENT_W,
            heading_h,
            self.options.primary_color.tint(0.89),
        );
        self.rect(
            MARGIN,
            self.y - heading_h + 6.0,
            4.0,
            heading_h,
            self.options.primary_color,
        );
        let heading = if let Some(code) = &section.code {
            format!("{}  |  {}", section.title, code)
        } else {
            section.title.clone()
        };
        let clipped = wrap_text(&heading, CONTENT_W - 18.0, 12.0, true)
            .into_iter()
            .next()
            .unwrap_or(heading);
        self.text(
            MARGIN + 10.0,
            self.y - 10.5,
            &clipped,
            "F2",
            11.5,
            self.options.primary_color.shade(0.2),
        );
        self.y -= 34.0;

        for block in &section.blocks {
            match block {
                Block::Paragraph(text) => {
                    self.draw_wrapped_text(
                        text,
                        MARGIN,
                        CONTENT_W,
                        9.5,
                        "F1",
                        Color::rgb_u8(17, 24, 39),
                        12.5,
                    );
                    self.y -= 6.0;
                }
                Block::Note(text) => {
                    self.draw_wrapped_text(
                        text,
                        MARGIN + 8.0,
                        CONTENT_W - 8.0,
                        9.0,
                        "F1",
                        Color::rgb_u8(75, 85, 99),
                        12.0,
                    );
                    self.y -= 5.0;
                }
                Block::List(items) => {
                    for item in items {
                        self.ensure_space(14.0);
                        self.text(
                            MARGIN + 6.0,
                            self.y,
                            "-",
                            "F2",
                            9.5,
                            self.options.secondary_color.shade(0.25),
                        );
                        self.draw_wrapped_text(
                            item,
                            MARGIN + 20.0,
                            CONTENT_W - 20.0,
                            9.5,
                            "F1",
                            Color::rgb_u8(17, 24, 39),
                            12.5,
                        );
                    }
                    self.y -= 5.0;
                }
                Block::Table(table) => {
                    self.render_table(table);
                    self.y -= 10.0;
                }
            }
        }
        self.y -= 8.0;
    }

    fn draw_wrapped_text(
        &mut self,
        text: &str,
        x: f32,
        width: f32,
        size: f32,
        font: &str,
        color: Color,
        line_height: f32,
    ) {
        let lines = wrap_text(text, width, size, font == "F2");
        for line in lines {
            self.ensure_space(line_height);
            self.text(x, self.y, &line, font, size, color);
            self.y -= line_height;
        }
    }

    fn render_table(&mut self, table: &Table) {
        if table_needs_stacked_layout(table) {
            self.render_stacked_table(table);
            return;
        }

        let (columns, headers, rows) = normalize_table_columns(table);
        if headers.is_empty() && rows.is_empty() {
            return;
        }
        let col_w = CONTENT_W / columns as f32;

        if !headers.is_empty() {
            self.render_table_header(&headers, columns, col_w);
        }

        for row in rows {
            self.render_table_row(&row, columns, col_w);
        }
    }

    fn render_stacked_table(&mut self, table: &Table) {
        for line in table_to_label_value_lines(table) {
            self.draw_wrapped_text(
                &line,
                MARGIN,
                CONTENT_W,
                8.9,
                "F1",
                Color::rgb_u8(31, 41, 55),
                11.5,
            );
        }
    }

    fn render_table_header(&mut self, headers: &[TableCell], columns: usize, col_w: f32) {
        let prepared = prepare_render_cells(headers, columns, col_w, 8.2, true);
        let mut line_count = 1;
        for cell in &prepared {
            line_count = line_count.max(cell.lines.len());
        }
        let row_h = (line_count as f32 * 10.0) + 8.0;
        self.ensure_space(row_h + 4.0);
        let y_bottom = self.y - row_h;
        self.rect(
            MARGIN,
            y_bottom,
            CONTENT_W,
            row_h,
            self.options.secondary_color.tint(0.78),
        );
        for cell in &prepared {
            let x = MARGIN + (cell.start_col as f32 * col_w);
            let right_x = MARGIN + ((cell.start_col + cell.colspan) as f32 * col_w);
            self.line(
                x,
                y_bottom,
                x,
                self.y,
                self.options.secondary_color.tint(0.35),
                0.35,
            );
            self.line(
                right_x,
                y_bottom,
                right_x,
                self.y,
                self.options.secondary_color.tint(0.35),
                0.35,
            );
            for (idx, line) in cell.lines.iter().enumerate() {
                self.text(
                    x + 4.0,
                    self.y - 12.0 - (idx as f32 * 10.0),
                    line,
                    "F2",
                    8.0,
                    Color::rgb_u8(31, 41, 55),
                );
            }
        }
        self.line(
            PAGE_W - MARGIN,
            y_bottom,
            PAGE_W - MARGIN,
            self.y,
            self.options.secondary_color.tint(0.35),
            0.35,
        );
        self.line(
            MARGIN,
            y_bottom,
            PAGE_W - MARGIN,
            y_bottom,
            self.options.secondary_color,
            0.5,
        );
        self.y = y_bottom;
    }

    fn render_table_row(&mut self, row: &[TableCell], columns: usize, col_w: f32) {
        let prepared = prepare_render_cells(row, columns, col_w, 8.1, false);
        let mut total_lines = 1;
        for cell in &prepared {
            total_lines = total_lines.max(cell.lines.len());
        }

        let mut offset = 0;
        while offset < total_lines {
            let available_lines = ((self.y - BOTTOM_MARGIN - 8.0) / 10.0).floor().max(0.0) as usize;
            if available_lines < 2 {
                self.start_page(false);
                continue;
            }
            let take = (total_lines - offset).min(available_lines);
            let row_h = (take as f32 * 10.0) + 8.0;
            let y_bottom = self.y - row_h;
            self.rect(
                MARGIN,
                y_bottom,
                CONTENT_W,
                row_h,
                Color::rgb_u8(255, 255, 255),
            );

            for cell in &prepared {
                let x = MARGIN + (cell.start_col as f32 * col_w);
                let right_x = MARGIN + ((cell.start_col + cell.colspan) as f32 * col_w);
                self.line(
                    x,
                    y_bottom,
                    x,
                    self.y,
                    self.options.secondary_color.tint(0.65),
                    0.25,
                );
                self.line(
                    right_x,
                    y_bottom,
                    right_x,
                    self.y,
                    self.options.secondary_color.tint(0.65),
                    0.25,
                );
                for idx in 0..take {
                    if let Some(line) = cell.lines.get(offset + idx) {
                        self.text(
                            x + 4.0,
                            self.y - 12.0 - (idx as f32 * 10.0),
                            line,
                            "F1",
                            8.1,
                            Color::rgb_u8(31, 41, 55),
                        );
                    }
                }
            }
            self.line(
                MARGIN,
                y_bottom,
                PAGE_W - MARGIN,
                y_bottom,
                self.options.secondary_color.tint(0.55),
                0.25,
            );
            self.y = y_bottom;
            offset += take;
            if offset < total_lines {
                self.start_page(false);
            }
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        self.current.push_str(&color.pdf_fill());
        self.current
            .push_str(&format!("{x:.2} {y:.2} {w:.2} {h:.2} re f\n"));
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, color: Color, width: f32) {
        self.current.push_str(&color.pdf_stroke());
        self.current.push_str(&format!(
            "{width:.2} w {x1:.2} {y1:.2} m {x2:.2} {y2:.2} l S\n"
        ));
    }

    fn text(&mut self, x: f32, y: f32, text: &str, font: &str, size: f32, color: Color) {
        let escaped = pdf_escape_text(text);
        self.current.push_str(&color.pdf_fill());
        self.current.push_str(&format!(
            "BT /{font} {size:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm ({escaped}) Tj ET\n"
        ));
    }

    fn image(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.current.push_str(&format!(
            "q {w:.2} 0 0 {h:.2} {x:.2} {y:.2} cm /ImLogo Do Q\n"
        ));
    }
}

#[derive(Debug)]
struct RenderCell {
    start_col: usize,
    colspan: usize,
    lines: Vec<String>,
}

fn prepare_render_cells(
    row: &[TableCell],
    columns: usize,
    col_w: f32,
    font_size: f32,
    bold: bool,
) -> Vec<RenderCell> {
    let mut prepared = Vec::new();
    let mut col = 0usize;
    for cell in row {
        if col >= columns {
            break;
        }
        let colspan = cell.colspan.min(columns - col).max(1);
        let width = (colspan as f32 * col_w) - 8.0;
        prepared.push(RenderCell {
            start_col: col,
            colspan,
            lines: wrap_text(&cell.text, width, font_size, bold),
        });
        col += colspan;
    }
    while col < columns {
        prepared.push(RenderCell {
            start_col: col,
            colspan: 1,
            lines: Vec::new(),
        });
        col += 1;
    }
    prepared
}

fn table_column_count(table: &Table) -> usize {
    table
        .column_count
        .max(row_column_count(&table.headers))
        .max(
            table
                .rows
                .iter()
                .map(|row| row_column_count(row))
                .max()
                .unwrap_or(0),
        )
        .max(1)
}

fn table_needs_stacked_layout(table: &Table) -> bool {
    let columns = table_column_count(table);
    CONTENT_W / (columns as f32) < MIN_RENDER_TABLE_CELL_WIDTH
}

fn normalize_table_columns(table: &Table) -> (usize, Vec<TableCell>, Vec<Vec<TableCell>>) {
    let columns = table_column_count(table).max(1);
    (
        columns,
        normalize_row_width(&table.headers, columns),
        table
            .rows
            .iter()
            .map(|row| normalize_row_width(row, columns))
            .collect(),
    )
}

fn normalize_row_width(row: &[TableCell], columns: usize) -> Vec<TableCell> {
    let mut result = Vec::new();
    let mut used = 0usize;
    for cell in row {
        if used >= columns {
            break;
        }
        let colspan = cell.colspan.min(columns - used).max(1);
        result.push(TableCell::new(cell.text.clone(), colspan));
        used += colspan;
    }
    while used < columns {
        result.push(TableCell::blank());
        used += 1;
    }
    result
}

fn expand_row(row: &[TableCell], columns: usize) -> Vec<String> {
    let mut expanded = Vec::new();
    for cell in row {
        if expanded.len() >= columns {
            break;
        }
        expanded.push(cell.text.clone());
        for _ in 1..cell.colspan {
            if expanded.len() >= columns {
                break;
            }
            expanded.push(String::new());
        }
    }
    expanded.resize(columns, String::new());
    expanded
}

fn table_to_label_value_lines(table: &Table) -> Vec<String> {
    let columns = table_column_count(table);
    let headers = expand_row(&table.headers, columns);
    let mut lines = Vec::new();

    if table.rows.is_empty() {
        for (idx, header) in headers.iter().enumerate() {
            if !header.is_empty() {
                lines.push(format!("Column {}: {header}", idx + 1));
            }
        }
        return lines;
    }

    let include_row_number = table.rows.len() > 1;
    for (row_idx, row) in table.rows.iter().enumerate() {
        let values = expand_row(row, columns);
        for (idx, value) in values.iter().enumerate() {
            if value.is_empty() {
                continue;
            }
            let label = headers
                .get(idx)
                .filter(|header| !header.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("Column {}", idx + 1));
            if include_row_number {
                lines.push(format!("Row {} - {label}: {value}", row_idx + 1));
            } else {
                lines.push(format!("{label}: {value}"));
            }
        }
    }

    lines
}

fn labeled(label: &str, value: Option<&str>) -> String {
    format!(
        "{label}: {}",
        value.filter(|value| !value.is_empty()).unwrap_or("Unknown")
    )
}

struct PdfWriter;

impl PdfWriter {
    fn write(pages: Vec<String>, logo: Option<&LogoImage>) -> Result<Vec<u8>> {
        if pages.is_empty() {
            return Err(Error::Pdf("cannot write a PDF with no pages".to_string()));
        }

        let mut objects = PdfObjects::new();
        let catalog_id = objects.reserve();
        let pages_id = objects.reserve();
        let font_regular_id = objects.reserve();
        let font_bold_id = objects.reserve();
        let logo_id = if logo.is_some() {
            Some(objects.reserve())
        } else {
            None
        };

        objects.set(
            font_regular_id,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        );
        objects.set(
            font_bold_id,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        );
        if let (Some(id), Some(image)) = (logo_id, logo) {
            objects.set(id, image_object(image));
        }

        let mut page_ids = Vec::new();
        for content in pages {
            let content_id = objects.reserve();
            let page_id = objects.reserve();
            objects.set(content_id, stream_object("", content.as_bytes()));
            let mut resources =
                format!("<< /Font << /F1 {font_regular_id} 0 R /F2 {font_bold_id} 0 R >>");
            if let Some(logo_id) = logo_id {
                resources.push_str(&format!(" /XObject << /ImLogo {logo_id} 0 R >>"));
            }
            resources.push_str(" >>");
            objects.set(
                page_id,
                format!(
                    "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {PAGE_W:.0} {PAGE_H:.0}] /Resources {resources} /Contents {content_id} 0 R >>"
                )
                .into_bytes(),
            );
            page_ids.push(page_id);
        }

        let kids = page_ids
            .iter()
            .map(|id| format!("{id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        objects.set(
            pages_id,
            format!(
                "<< /Type /Pages /Kids [{kids}] /Count {} >>",
                page_ids.len()
            )
            .into_bytes(),
        );
        objects.set(
            catalog_id,
            format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
        );

        Ok(objects.finish(catalog_id))
    }
}

struct PdfObjects {
    objects: Vec<Option<Vec<u8>>>,
}

impl PdfObjects {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    fn reserve(&mut self) -> usize {
        self.objects.push(None);
        self.objects.len()
    }

    fn set(&mut self, id: usize, bytes: Vec<u8>) {
        self.objects[id - 1] = Some(bytes);
    }

    fn finish(self, root_id: usize) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"%PDF-1.4\n%\xD3\xEB\xE9\xE1\n");
        let mut offsets = Vec::with_capacity(self.objects.len() + 1);
        offsets.push(0);

        for (idx, object) in self.objects.into_iter().enumerate() {
            offsets.push(output.len());
            output.extend_from_slice(format!("{} 0 obj\n", idx + 1).as_bytes());
            output.extend_from_slice(&object.expect("all PDF objects must be set before finish"));
            output.extend_from_slice(b"\nendobj\n");
        }

        let xref_start = output.len();
        output.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        output.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        output.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root_id} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        output
    }
}

fn image_object(image: &LogoImage) -> Vec<u8> {
    let filter = match image.filter {
        ImageFilter::DctDecode => "/DCTDecode",
        ImageFilter::FlateDecode => "/FlateDecode",
    };
    let color_space = match image.color_space {
        ImageColorSpace::DeviceGray => "/DeviceGray",
        ImageColorSpace::DeviceRgb => "/DeviceRGB",
        ImageColorSpace::DeviceCmyk => "/DeviceCMYK",
    };
    let decode_params = image
        .decode_params
        .as_ref()
        .map(|params| format!(" /DecodeParms {params}"))
        .unwrap_or_default();
    let dict = format!(
        "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} /BitsPerComponent 8 /Filter {}{}",
        image.width, image.height, color_space, filter, decode_params
    );
    stream_object(&dict, &image.data)
}

fn stream_object(extra_dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if extra_dict.is_empty() {
        out.extend_from_slice(format!("<< /Length {} >>\nstream\n", data.len()).as_bytes());
    } else {
        out.extend_from_slice(
            format!("<< {extra_dict} /Length {} >>\nstream\n", data.len()).as_bytes(),
        );
    }
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

pub fn load_logo<P: AsRef<Path>>(path: P) -> Result<LogoImage> {
    let path = path.as_ref();
    let data = fs::read(path)
        .map_err(|err| Error::Io(format!("failed to read logo {}: {err}", path.display())))?;
    if data.starts_with(&[0xFF, 0xD8]) {
        parse_jpeg_logo(data)
    } else if data.starts_with(b"\x89PNG\r\n\x1A\n") {
        parse_png_logo(data)
    } else {
        Err(Error::UnsupportedLogo(
            "logo must be a JPEG or an 8-bit RGB/grayscale PNG".to_string(),
        ))
    }
}

fn parse_jpeg_logo(data: Vec<u8>) -> Result<LogoImage> {
    let mut i = 2;
    while i + 9 < data.len() {
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            break;
        }
        let marker = data[i];
        i += 1;
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if i + 2 > data.len() {
            break;
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return Err(Error::UnsupportedLogo(
                "JPEG has an invalid segment length".to_string(),
            ));
        }
        let sof_marker = matches!(
            marker,
            0xC0 | 0xC1
                | 0xC2
                | 0xC3
                | 0xC5
                | 0xC6
                | 0xC7
                | 0xC9
                | 0xCA
                | 0xCB
                | 0xCD
                | 0xCE
                | 0xCF
        );
        if sof_marker {
            if len < 8 {
                return Err(Error::UnsupportedLogo(
                    "JPEG SOF segment is too short".to_string(),
                ));
            }
            let height = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            let width = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let components = data[i + 7];
            let color_space = match components {
                1 => ImageColorSpace::DeviceGray,
                3 => ImageColorSpace::DeviceRgb,
                4 => ImageColorSpace::DeviceCmyk,
                _ => {
                    return Err(Error::UnsupportedLogo(format!(
                        "JPEG has unsupported component count {components}"
                    )))
                }
            };
            return Ok(LogoImage {
                width,
                height,
                data,
                filter: ImageFilter::DctDecode,
                color_space,
                decode_params: None,
            });
        }
        i += len;
    }
    Err(Error::UnsupportedLogo(
        "JPEG dimensions could not be found".to_string(),
    ))
}

fn parse_png_logo(data: Vec<u8>) -> Result<LogoImage> {
    let mut i = 8;
    let mut width = None;
    let mut height = None;
    let mut color_type = None;
    let mut idat = Vec::new();

    while i + 12 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let chunk_type = &data[i + 4..i + 8];
        let chunk_start = i + 8;
        let chunk_end = chunk_start + len;
        if chunk_end + 4 > data.len() {
            return Err(Error::UnsupportedLogo(
                "PNG chunk extends past end of file".to_string(),
            ));
        }
        match chunk_type {
            b"IHDR" => {
                if len != 13 {
                    return Err(Error::UnsupportedLogo(
                        "PNG IHDR chunk has invalid length".to_string(),
                    ));
                }
                width = Some(u32::from_be_bytes([
                    data[chunk_start],
                    data[chunk_start + 1],
                    data[chunk_start + 2],
                    data[chunk_start + 3],
                ]));
                height = Some(u32::from_be_bytes([
                    data[chunk_start + 4],
                    data[chunk_start + 5],
                    data[chunk_start + 6],
                    data[chunk_start + 7],
                ]));
                let bit_depth = data[chunk_start + 8];
                let png_color_type = data[chunk_start + 9];
                let compression = data[chunk_start + 10];
                let filter = data[chunk_start + 11];
                let interlace = data[chunk_start + 12];
                if bit_depth != 8 || compression != 0 || filter != 0 || interlace != 0 {
                    return Err(Error::UnsupportedLogo(
                        "PNG logo must be non-interlaced 8-bit standard-compression data"
                            .to_string(),
                    ));
                }
                if !matches!(png_color_type, 0 | 2) {
                    return Err(Error::UnsupportedLogo(
                        "PNG logo must be grayscale or RGB; alpha/palette PNGs are not supported"
                            .to_string(),
                    ));
                }
                color_type = Some(png_color_type);
            }
            b"IDAT" => idat.extend_from_slice(&data[chunk_start..chunk_end]),
            b"IEND" => break,
            _ => {}
        }
        i = chunk_end + 4;
    }

    let width = width.ok_or_else(|| Error::UnsupportedLogo("PNG is missing IHDR".to_string()))?;
    let height = height.ok_or_else(|| Error::UnsupportedLogo("PNG is missing IHDR".to_string()))?;
    if idat.is_empty() {
        return Err(Error::UnsupportedLogo(
            "PNG is missing image data".to_string(),
        ));
    }
    let color_type = color_type.unwrap_or(2);
    let (color_space, colors) = match color_type {
        0 => (ImageColorSpace::DeviceGray, 1),
        2 => (ImageColorSpace::DeviceRgb, 3),
        _ => unreachable!("unsupported color types are rejected above"),
    };
    Ok(LogoImage {
        width,
        height,
        data: idat,
        filter: ImageFilter::FlateDecode,
        color_space,
        decode_params: Some(format!(
            "<< /Predictor 15 /Colors {colors} /BitsPerComponent 8 /Columns {width} >>"
        )),
    })
}

fn has_tag(node: Node<'_, '_>, tag: &str) -> bool {
    node.is_element() && node.tag_name().name() == tag
}

fn direct_child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| has_tag(*child, tag))
}

fn attr<'a, 'input>(node: &Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.attribute(name)
}

fn human_name(name: Node<'_, '_>) -> String {
    let ordered_parts = ["prefix", "given", "family", "suffix"];
    let mut parts = Vec::new();
    for part in ordered_parts {
        for child in name.children().filter(|child| has_tag(*child, part)) {
            let text = text_content(child);
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        text_content(name)
    } else {
        parts.join(" ")
    }
}

fn text_content(node: Node<'_, '_>) -> String {
    let mut raw = String::new();
    collect_text(node, &mut raw);
    normalize_ws(&raw)
}

fn collect_text(node: Node<'_, '_>, out: &mut String) {
    for child in node.children() {
        if child.is_text() {
            out.push_str(child.text().unwrap_or_default());
            out.push(' ');
        } else if child.is_element() {
            match child.tag_name().name() {
                "br" => out.push('\n'),
                "reference" => {
                    if let Some(value) = attr(&child, "value") {
                        out.push_str(value);
                        out.push(' ');
                    }
                }
                _ => collect_text(child, out),
            }
        }
    }
}

fn normalize_ws(input: &str) -> String {
    let mut out = String::new();
    let mut last_space = true;
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

fn format_address(addr: Node<'_, '_>) -> String {
    let mut parts = Vec::new();
    for tag in [
        "streetAddressLine",
        "city",
        "state",
        "postalCode",
        "country",
    ] {
        for child in addr.children().filter(|child| has_tag(*child, tag)) {
            let text = text_content(child);
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    parts.join(", ")
}

fn clean_telecom(value: &str) -> String {
    value.strip_prefix("tel:").unwrap_or(value).to_string()
}

fn gender_from_code(code: &str) -> String {
    match code {
        "M" => "Male".to_string(),
        "F" => "Female".to_string(),
        "UN" => "Undifferentiated".to_string(),
        "UNK" => "Unknown".to_string(),
        _ => code.to_string(),
    }
}

fn format_hl7_timestamp(value: &str) -> String {
    let trimmed = value.trim();
    let date_time = trimmed
        .split_once('.')
        .map(|(before, after)| {
            let tz_start = after
                .find(['+', '-'])
                .map(|idx| format!("{}", &after[idx..]));
            match tz_start {
                Some(tz) => format!("{before}{tz}"),
                None => before.to_string(),
            }
        })
        .unwrap_or_else(|| trimmed.to_string());

    let mut digits = String::new();
    let mut suffix = String::new();
    for ch in date_time.chars() {
        if ch.is_ascii_digit() && suffix.is_empty() {
            digits.push(ch);
        } else {
            suffix.push(ch);
        }
    }

    let mut formatted = String::new();
    if digits.len() >= 4 {
        formatted.push_str(&digits[0..4]);
    }
    if digits.len() >= 6 {
        formatted.push('-');
        formatted.push_str(&digits[4..6]);
    }
    if digits.len() >= 8 {
        formatted.push('-');
        formatted.push_str(&digits[6..8]);
    }
    if digits.len() >= 10 {
        formatted.push(' ');
        formatted.push_str(&digits[8..10]);
    }
    if digits.len() >= 12 {
        formatted.push(':');
        formatted.push_str(&digits[10..12]);
    }
    if digits.len() >= 14 {
        formatted.push(':');
        formatted.push_str(&digits[12..14]);
    }
    if !suffix.is_empty() {
        formatted.push(' ');
        formatted.push_str(&suffix);
    }
    if formatted.is_empty() {
        trimmed.to_string()
    } else {
        formatted
    }
}

fn split_camel_case(value: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx > 0 && ch.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

fn decode_base64(input: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in input.iter().copied() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b'\t' | b' ' => continue,
            _ => return Err(format!("invalid base64 byte 0x{byte:02x}")),
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

fn wrap_text(text: &str, max_width: f32, font_size: f32, bold: bool) -> Vec<String> {
    let normalized = normalize_ws(text);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let words = normalized.split_whitespace();

    for word in words {
        let pieces = split_long_word(word, max_width, font_size, bold);
        for piece in pieces {
            if current.is_empty() {
                current = piece;
                continue;
            }
            let candidate = format!("{current} {piece}");
            if estimate_text_width(&candidate, font_size, bold) <= max_width {
                current = candidate;
            } else {
                lines.push(current);
                current = piece;
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn split_long_word(word: &str, max_width: f32, font_size: f32, bold: bool) -> Vec<String> {
    if estimate_text_width(word, font_size, bold) <= max_width {
        return vec![word.to_string()];
    }
    let mut pieces = Vec::new();
    let mut current = String::new();
    for ch in word.chars() {
        let candidate = format!("{current}{ch}");
        if !current.is_empty() && estimate_text_width(&candidate, font_size, bold) > max_width {
            pieces.push(current);
            current = ch.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn estimate_text_width(text: &str, font_size: f32, bold: bool) -> f32 {
    text.chars()
        .map(|ch| helvetica_width_units(ch, bold) as f32 * font_size / 1000.0)
        .sum()
}

fn helvetica_width_units(ch: char, bold: bool) -> u16 {
    if !ch.is_ascii() {
        return ascii_fallback(ch)
            .chars()
            .map(|fallback| helvetica_width_units(fallback, bold))
            .sum();
    }

    if bold {
        match ch {
            ' ' => 278,
            '!' => 333,
            '"' => 474,
            '#' | '$' | '0'..='9' | '_' | 'a' | 'c' | 'e' | 'k' | 's' | 'v' | 'x' => 556,
            '%' => 889,
            '&' | 'A' | 'B' | 'C' | 'D' | 'H' | 'K' | 'N' | 'R' => 722,
            '\'' | ',' | '.' | '/' | 'I' | '\\' | '`' | 'i' | 'j' | 'l' => 278,
            '(' | ')' | 'f' | ':' | ';' | '[' | ']' | 't' => 333,
            '*' | 'r' | '{' | '}' => 389,
            '+' | '<' | '=' | '>' | '^' | '~' => 584,
            '-' => 333,
            '?' | 'F' | 'L' | 'T' | 'Z' | 'b' | 'd' | 'g' | 'h' | 'n' | 'o' | 'p' | 'q' | 'u' => {
                611
            }
            '@' => 975,
            'G' | 'O' | 'Q' | 'U' => 778,
            'J' => 556,
            'M' => 833,
            'E' | 'P' | 'S' | 'V' | 'X' | 'Y' => 667,
            'W' => 944,
            'm' => 889,
            'w' => 778,
            'y' | 'z' => 500,
            '|' => 280,
            _ => 556,
        }
    } else {
        match ch {
            ' ' | '!' | ',' | '-' | '.' | '/' | 'I' | '[' | '\\' | ']' => 278,
            '"' => 355,
            '#'
            | '$'
            | '0'..='9'
            | '?'
            | 'L'
            | '_'
            | 'a'
            | 'b'
            | 'd'
            | 'e'
            | 'g'
            | 'h'
            | 'n'
            | 'o'
            | 'p'
            | 'q'
            | 'u' => 556,
            '%' => 889,
            '&' | 'A' | 'B' | 'R' => 667,
            '\'' | '`' | 'i' | 'j' | 'l' => 222,
            '(' | ')' | 'r' | '{' | '}' => 333,
            '*' => 389,
            '+' | '<' | '=' | '>' | '~' => 584,
            ':' | ';' => 278,
            '@' => 1015,
            'C' | 'H' | 'N' => 722,
            'D' | 'G' | 'O' | 'Q' | 'U' => 778,
            'E' | 'K' | 'P' | 'V' | 'X' | 'Y' => 667,
            'F' | 'T' | 'Z' => 611,
            'J' => 500,
            'M' | 'm' => 833,
            'S' => 667,
            'W' => 944,
            '^' => 469,
            'c' | 'k' | 's' | 'v' | 'x' | 'y' | 'z' => 500,
            'f' | 't' => 278,
            'w' => 722,
            '|' => 260,
            _ => 556,
        }
    }
}

fn pdf_escape_text(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' | '\r' | '\t' => out.push(' '),
            ch if ch.is_control() => out.push(' '),
            ch if ch.is_ascii() => out.push(ch),
            _ => out.push_str(ascii_fallback(ch)),
        }
    }
    out
}

fn ascii_fallback(ch: char) -> &'static str {
    match ch as u32 {
        0x00e1 | 0x00e0 | 0x00e2 | 0x00e4 | 0x00e3 | 0x00e5 | 0x0101 | 0x0103 | 0x0105 => "a",
        0x00c1 | 0x00c0 | 0x00c2 | 0x00c4 | 0x00c3 | 0x00c5 | 0x0100 | 0x0102 | 0x0104 => "A",
        0x00e7 | 0x0107 | 0x010d | 0x0109 | 0x010b => "c",
        0x00c7 | 0x0106 | 0x010c | 0x0108 | 0x010a => "C",
        0x010f | 0x0111 => "d",
        0x010e | 0x0110 => "D",
        0x00e9 | 0x00e8 | 0x00ea | 0x00eb | 0x0113 | 0x0115 | 0x0117 | 0x0119 | 0x011b => "e",
        0x00c9 | 0x00c8 | 0x00ca | 0x00cb | 0x0112 | 0x0114 | 0x0116 | 0x0118 | 0x011a => "E",
        0x00ed | 0x00ec | 0x00ee | 0x00ef | 0x012b | 0x012d | 0x012f | 0x0131 => "i",
        0x00cd | 0x00cc | 0x00ce | 0x00cf | 0x012a | 0x012c | 0x012e | 0x0130 => "I",
        0x00f1 | 0x0144 | 0x0148 => "n",
        0x00d1 | 0x0143 | 0x0147 => "N",
        0x00f3 | 0x00f2 | 0x00f4 | 0x00f6 | 0x00f5 | 0x014d | 0x014f | 0x0151 => "o",
        0x00d3 | 0x00d2 | 0x00d4 | 0x00d6 | 0x00d5 | 0x014c | 0x014e | 0x0150 => "O",
        0x0161 | 0x015b | 0x015d | 0x015f => "s",
        0x0160 | 0x015a | 0x015c | 0x015e => "S",
        0x00fa | 0x00f9 | 0x00fb | 0x00fc | 0x016b | 0x016d | 0x016f | 0x0171 | 0x0173 => "u",
        0x00da | 0x00d9 | 0x00db | 0x00dc | 0x016a | 0x016c | 0x016e | 0x0170 | 0x0172 => "U",
        0x00fd | 0x00ff | 0x0177 => "y",
        0x00dd | 0x0176 => "Y",
        0x017e | 0x017a | 0x017c => "z",
        0x017d | 0x0179 | 0x017b => "Z",
        0x00c6 => "AE",
        0x00e6 => "ae",
        0x0152 => "OE",
        0x0153 => "oe",
        0x00df => "ss",
        0x2019 | 0x2018 | 0x201a | 0x201b => "'",
        0x201c | 0x201d | 0x201e | 0x201f => "\"",
        0x2013 | 0x2014 | 0x2212 => "-",
        0x2026 => "...",
        0x00b0 => " degrees",
        _ => "?",
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_exports {
    use super::{decode_xml_bytes, render_ccda_xml_to_pdf, Color, RenderOptions};
    use std::cell::RefCell;
    use std::slice;
    use std::str;

    thread_local! {
        static LAST_RESULT: RefCell<Vec<u8>> = RefCell::new(Vec::new());
        static LAST_ERROR: RefCell<Vec<u8>> = RefCell::new(Vec::new());
    }

    #[no_mangle]
    pub extern "C" fn ccda_alloc(len: usize) -> *mut u8 {
        let mut buffer = Vec::<u8>::with_capacity(len);
        let ptr = buffer.as_mut_ptr();
        std::mem::forget(buffer);
        ptr
    }

    #[no_mangle]
    pub unsafe extern "C" fn ccda_dealloc(ptr: *mut u8, len: usize) {
        if !ptr.is_null() {
            drop(Vec::from_raw_parts(ptr, 0, len));
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn ccda_render(
        xml_ptr: *const u8,
        xml_len: usize,
        primary_ptr: *const u8,
        primary_len: usize,
        secondary_ptr: *const u8,
        secondary_len: usize,
    ) -> i32 {
        LAST_RESULT.with(|result| result.borrow_mut().clear());
        LAST_ERROR.with(|error| error.borrow_mut().clear());

        let result = render_from_parts(
            xml_ptr,
            xml_len,
            primary_ptr,
            primary_len,
            secondary_ptr,
            secondary_len,
        );

        match result {
            Ok(pdf) => {
                LAST_RESULT.with(|result| *result.borrow_mut() = pdf);
                0
            }
            Err(message) => {
                LAST_ERROR.with(|error| *error.borrow_mut() = message.into_bytes());
                1
            }
        }
    }

    unsafe fn render_from_parts(
        xml_ptr: *const u8,
        xml_len: usize,
        primary_ptr: *const u8,
        primary_len: usize,
        secondary_ptr: *const u8,
        secondary_len: usize,
    ) -> std::result::Result<Vec<u8>, String> {
        let xml = decode_xml_bytes(read_bytes(xml_ptr, xml_len, "xml")?);
        let mut options = RenderOptions::default();
        options.logo = None;
        if primary_len > 0 {
            options.primary_color =
                Color::parse(read_utf8(primary_ptr, primary_len, "primaryColor")?)
                    .map_err(|err| err.to_string())?;
        }
        if secondary_len > 0 {
            options.secondary_color =
                Color::parse(read_utf8(secondary_ptr, secondary_len, "secondaryColor")?)
                    .map_err(|err| err.to_string())?;
        }
        render_ccda_xml_to_pdf(&xml, &options).map_err(|err| err.to_string())
    }

    unsafe fn read_bytes<'a>(
        ptr: *const u8,
        len: usize,
        label: &str,
    ) -> std::result::Result<&'a [u8], String> {
        if len == 0 {
            return Ok(&[]);
        }
        if ptr.is_null() {
            return Err(format!("{label} pointer was null with non-zero length"));
        }
        Ok(slice::from_raw_parts(ptr, len))
    }

    unsafe fn read_utf8<'a>(
        ptr: *const u8,
        len: usize,
        label: &str,
    ) -> std::result::Result<&'a str, String> {
        str::from_utf8(read_bytes(ptr, len, label)?)
            .map_err(|err| format!("{label} was not valid UTF-8: {err}"))
    }

    #[no_mangle]
    pub extern "C" fn ccda_result_ptr() -> *const u8 {
        LAST_RESULT.with(|result| result.borrow().as_ptr())
    }

    #[no_mangle]
    pub extern "C" fn ccda_result_len() -> usize {
        LAST_RESULT.with(|result| result.borrow().len())
    }

    #[no_mangle]
    pub extern "C" fn ccda_error_ptr() -> *const u8 {
        LAST_ERROR.with(|error| error.borrow().as_ptr())
    }

    #[no_mangle]
    pub extern "C" fn ccda_error_len() -> usize {
        LAST_ERROR.with(|error| error.borrow().len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_CCDA: &str = r##"
<ClinicalDocument xmlns="urn:hl7-org:v3">
  <title>Test Summary</title>
  <effectiveTime value="20240102123456-0500"/>
  <recordTarget>
    <patientRole>
      <id extension="123" root="1.2.3"/>
      <patient>
        <name><given>Ada</given><family>Lovelace</family></name>
        <administrativeGenderCode code="F"/>
        <birthTime value="18151210"/>
      </patient>
      <providerOrganization><name>Example Clinic</name></providerOrganization>
    </patientRole>
  </recordTarget>
  <component>
    <structuredBody>
      <component>
        <section>
          <code code="10160-0" displayName="History of Medication Use"/>
          <title>Medications</title>
          <text>
            <table>
              <thead><tr><th>Name</th><th>Status</th></tr></thead>
              <tbody><tr><td>Aspirin</td><td>Active</td></tr></tbody>
            </table>
          </text>
        </section>
      </component>
      <component>
        <section>
          <title>Plan</title>
          <text><list><item>Follow up in 2 weeks</item><item>Repeat labs</item></list></text>
        </section>
      </component>
    </structuredBody>
  </component>
</ClinicalDocument>
"##;

    #[test]
    fn parses_structured_ccda() {
        let doc = parse_ccda(SIMPLE_CCDA).unwrap();
        assert_eq!(doc.title, "Test Summary");
        assert_eq!(doc.patient.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(doc.patient.gender.as_deref(), Some("Female"));
        assert_eq!(doc.sections.len(), 2);
        assert!(matches!(doc.sections[0].blocks[0], Block::Table(_)));
    }

    #[test]
    fn preserves_narrative_table_spans() {
        let xml = r#"
<ClinicalDocument xmlns="urn:hl7-org:v3">
  <title>Span Test</title>
  <component><structuredBody><component><section>
    <title>Medications</title>
    <text>
      <table>
        <thead><tr><th colspan="2">Medication</th><th>Status</th></tr></thead>
        <tbody>
          <tr><td rowspan="2">Aspirin</td><td>81 mg</td><td>Active</td></tr>
          <tr><td>daily</td><td>Active</td></tr>
          <tr><td colspan="3">Reviewed by clinician</td></tr>
        </tbody>
      </table>
    </text>
  </section></component></structuredBody></component>
</ClinicalDocument>
"#;
        let doc = parse_ccda(xml).unwrap();
        let Block::Table(table) = &doc.sections[0].blocks[0] else {
            panic!("expected table block");
        };
        assert_eq!(table.column_count, 3);
        assert_eq!(table.headers[0].text, "Medication");
        assert_eq!(table.headers[0].colspan, 2);
        assert_eq!(table.rows[1][0].text, "");
        assert_eq!(table.rows[1][1].text, "daily");
        assert_eq!(table.rows[2][0].text, "Reviewed by clinician");
        assert_eq!(table.rows[2][0].colspan, 3);
    }

    #[test]
    fn rejects_non_ccda_root() {
        let err = parse_ccda("<notClinicalDocument/>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("root element must be ClinicalDocument"));
    }

    #[test]
    fn parses_colors() {
        assert_eq!(
            Color::parse("#0f766e").unwrap(),
            Color::rgb_u8(15, 118, 110)
        );
        assert!(Color::parse("nope").is_err());
    }

    #[test]
    fn renders_pdf_bytes() {
        let doc = parse_ccda(SIMPLE_CCDA).unwrap();
        let pdf = render_pdf(&doc, &RenderOptions::default()).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        assert!(pdf.len() > 3000);
    }

    #[test]
    fn renders_with_rgb_png_logo() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0xfc, 0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x05, 0x83, 0x02, 0x7f, 0x94, 0xe7,
            0xa2, 0xcf, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let path =
            std::env::temp_dir().join(format!("ccda-to-pdf-test-logo-{}.png", std::process::id()));
        std::fs::write(&path, png).unwrap();
        let logo = load_logo(&path).unwrap();
        let _ = std::fs::remove_file(path);

        let doc = parse_ccda(SIMPLE_CCDA).unwrap();
        let pdf = render_pdf(
            &doc,
            &RenderOptions {
                logo: Some(logo),
                ..RenderOptions::default()
            },
        )
        .unwrap();
        assert!(pdf
            .windows(b"/ImLogo".len())
            .any(|window| window == b"/ImLogo"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn decodes_non_xml_base64_text() {
        let xml = r#"
<ClinicalDocument xmlns="urn:hl7-org:v3">
  <title>Unstructured</title>
  <component><nonXMLBody><text mediaType="text/plain" representation="B64">SGVsbG8gQ0NEQQ==</text></nonXMLBody></component>
</ClinicalDocument>
"#;
        let doc = parse_ccda(xml).unwrap();
        assert_eq!(doc.sections.len(), 1);
        assert!(
            matches!(&doc.sections[0].blocks[0], Block::Paragraph(value) if value == "Hello CCDA")
        );
    }

    #[test]
    fn splits_long_unbreakable_tokens_to_available_width() {
        let token = "1.2.840.113883.10.20.22.1.2.9999341.300000000000000000000000000000000000000000000000000";
        let lines = wrap_text(token, 80.0, 9.5, false);
        assert!(lines.len() > 1);
        assert!(lines
            .iter()
            .all(|line| estimate_text_width(line, 9.5, false) <= 80.01));
    }

    #[test]
    fn renders_wide_tables_as_stacked_label_value_lines() {
        let xml = r#"
<ClinicalDocument xmlns="urn:hl7-org:v3">
  <title>Wide Table</title>
  <component><structuredBody><component><section>
    <title>Results</title>
    <text>
      <table>
        <thead><tr><th>H1</th><th>H2</th><th>H3</th><th>H4</th><th>H5</th><th>H6</th><th>H7</th><th>H8</th><th>H9</th><th>H10</th><th>H11</th><th>H12</th></tr></thead>
        <tbody><tr><td>V1</td><td>V2</td><td>V3</td><td>V4</td><td>V5</td><td>V6</td><td>V7</td><td>V8</td><td>V9</td><td>V10</td><td>V11</td><td>sentinel-wide-value</td></tr></tbody>
      </table>
    </text>
  </section></component></structuredBody></component>
</ClinicalDocument>
"#;
        let pdf = render_ccda_xml_to_pdf(xml, &RenderOptions::default()).unwrap();
        assert!(pdf
            .windows(b"H12: sentinel-wide-value".len())
            .any(|window| window == b"H12: sentinel-wide-value"));
    }

    #[test]
    fn keeps_wide_tables_tabular_when_columns_still_fit() {
        let table = Table {
            headers: (1..=8)
                .map(|idx| TableCell::new(format!("H{idx}"), 1))
                .collect(),
            rows: vec![(1..=8)
                .map(|idx| TableCell::new(format!("V{idx}"), 1))
                .collect()],
            column_count: 8,
        };

        assert!(!table_needs_stacked_layout(&table));
    }

    #[test]
    fn rejects_pathologically_deep_xml_before_parsing() {
        let mut xml = String::from("<ClinicalDocument>");
        for _ in 0..MAX_XML_DEPTH {
            xml.push_str("<component>");
        }
        xml.push_str("<title>Too deep</title>");
        for _ in 0..MAX_XML_DEPTH {
            xml.push_str("</component>");
        }
        xml.push_str("</ClinicalDocument>");

        let err = parse_ccda(&xml).unwrap_err().to_string();
        assert!(err.contains("XML nesting exceeds safety limit"));
    }

    #[test]
    fn accepts_realistic_xml_nesting() {
        let mut xml = String::from("<ClinicalDocument><title>Nested</title>");
        for _ in 0..20 {
            xml.push_str("<component>");
        }
        xml.push_str("content");
        for _ in 0..20 {
            xml.push_str("</component>");
        }
        xml.push_str("</ClinicalDocument>");

        let doc = parse_ccda(&xml).unwrap();
        assert_eq!(doc.title, "Nested");
    }

    #[test]
    fn rejects_dtd_and_entity_declarations() {
        let xml = r#"
<!DOCTYPE ClinicalDocument [
  <!ENTITY lol "lol">
  <!ENTITY lol1 "&lol;&lol;&lol;&lol;">
]>
<ClinicalDocument xmlns="urn:hl7-org:v3"><title>Bomb</title></ClinicalDocument>
"#;
        let err = parse_ccda(xml).unwrap_err().to_string();
        assert!(err.contains("DTD and entity declarations are not supported"));
    }

    #[test]
    fn decodes_utf_boms_and_invalid_bytes_lossily() {
        let mut utf16le = vec![0xFF, 0xFE];
        for unit in SIMPLE_CCDA.encode_utf16() {
            utf16le.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_xml_bytes(&utf16le), SIMPLE_CCDA);

        let invalid = decode_xml_bytes(b"\xff<ClinicalDocument/>");
        assert!(invalid.starts_with('\u{FFFD}'));
    }

    #[test]
    fn public_xml_render_is_byte_deterministic() {
        let first = render_ccda_xml_to_pdf(SIMPLE_CCDA, &RenderOptions::default()).unwrap();
        let second = render_ccda_xml_to_pdf(SIMPLE_CCDA, &RenderOptions::default()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn panic_safe_render_retries_without_table_engine() {
        let doc = parse_ccda(SIMPLE_CCDA).unwrap();
        let pdf =
            render_document_to_pdf_panic_safe_with(&doc, &RenderOptions::default(), |_, _| {
                panic!("forced layout panic")
            })
            .unwrap();

        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf
            .windows(b"Normal table layout failed".len())
            .any(|window| window == b"Normal table layout failed"));
        assert!(pdf
            .windows(b"Name: Aspirin".len())
            .any(|window| window == b"Name: Aspirin"));
    }
}
