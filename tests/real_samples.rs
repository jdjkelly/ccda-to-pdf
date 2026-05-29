use ccda_to_pdf::{parse_ccda, render_pdf, RenderOptions};
use std::fs;
use std::path::PathBuf;

fn sample_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("real")
        .join(name)
}

#[test]
fn converts_representative_real_ccdas() {
    let samples = [
        "hl7_ccd.xml",
        "hl7_unstructured.xml",
        "nist_ccd_ambulatory.xml",
        "cerner_toc.xml",
        "allscripts_toc.xml",
        "vitera_smart.xml",
    ];

    for sample in samples {
        let xml = fs::read_to_string(sample_path(sample)).expect(sample);
        let doc = parse_ccda(&xml).expect(sample);
        assert!(
            !doc.sections.is_empty(),
            "{sample} should produce at least one printable section"
        );
        let pdf = render_pdf(&doc, &RenderOptions::default()).expect(sample);
        assert!(pdf.starts_with(b"%PDF-1.4"), "{sample} should be a PDF");
        assert!(
            pdf.ends_with(b"%%EOF\n"),
            "{sample} should have an EOF marker"
        );
        let minimum_size = if sample == "hl7_unstructured.xml" {
            1_800
        } else {
            2_500
        };
        assert!(
            pdf.len() > minimum_size,
            "{sample} PDF is unexpectedly small: {} bytes",
            pdf.len()
        );
    }
}

#[test]
fn converts_all_available_real_ccdas_without_panics() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join("real");
    let mut converted = 0usize;
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|value| value.to_str()) != Some("xml") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let xml = fs::read_to_string(entry.path()).expect(&name);
        let doc = parse_ccda(&xml).expect(&name);
        let pdf = render_pdf(&doc, &RenderOptions::default()).expect(&name);
        assert!(pdf.starts_with(b"%PDF-1.4"), "{name}");
        converted += 1;
    }
    assert!(
        converted >= 20,
        "expected broad fixture coverage, converted only {converted} files"
    );
}
