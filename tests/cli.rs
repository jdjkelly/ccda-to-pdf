use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccda-to-pdf")
}

#[test]
fn cli_writes_pdf_to_stdout_from_stdin() {
    let sample = include_str!("../samples/real/hl7_unstructured.xml");
    let mut child = Command::new(bin())
        .args(["-", "-", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(sample.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-1.4"));
    assert!(output.stdout.ends_with(b"%%EOF\n"));
    assert!(output.stderr.is_empty());
}

#[test]
fn cli_reads_utf16le_bom_from_stdin() {
    let sample = include_str!("../samples/real/hl7_unstructured.xml");
    let mut bytes = vec![0xFF, 0xFE];
    for unit in sample.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let mut child = Command::new(bin())
        .args(["-", "-", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child.stdin.as_mut().unwrap().write_all(&bytes).unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-1.4"));
    assert!(output.stdout.ends_with(b"%%EOF\n"));
}

#[test]
fn cli_reports_invalid_color() {
    let output = Command::new(bin())
        .args([
            "--primary-color",
            "not-a-color",
            "samples/real/hl7_ccd.xml",
            "-",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("color 'not-a-color' must be a 6-digit hex color"));
}

#[test]
fn cli_reports_malformed_xml() {
    let mut child = Command::new(bin())
        .args(["-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"<ClinicalDocument><broken></ClinicalDocument>")
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("XML parse error"));
}
