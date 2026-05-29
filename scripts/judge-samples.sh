#!/usr/bin/env bash
set -euo pipefail

samples_dir="${1:-samples/real}"
out_dir="${2:-out/judgement}"
bin="${BIN:-target/release/ccda-to-pdf}"

cargo build --release >/dev/null

mkdir -p "$out_dir/pdfs" "$out_dir/text" "$out_dir/logs"
report="$out_dir/report.tsv"
printf 'sample\tstatus\tpages\tpdf_bytes\ttext_bytes\tclinical_hits\tverdict\n' >"$report"

for xml in "$samples_dir"/*.xml; do
  sample="$(basename "$xml" .xml)"
  pdf="$out_dir/pdfs/$sample.pdf"
  text="$out_dir/text/$sample.txt"
  log="$out_dir/logs/$sample.stderr"
  status="ok"
  if ! "$bin" "$xml" "$pdf" --quiet 2>"$log"; then
    status="failed"
  fi

  pages="0"
  pdf_bytes="0"
  text_bytes="0"
  clinical_hits="0"
  verdict="fail"

  if [[ "$status" == "ok" && -s "$pdf" ]]; then
    pdf_bytes="$(wc -c <"$pdf" | tr -d ' ')"
    if command -v pdfinfo >/dev/null; then
      pages="$(pdfinfo "$pdf" | awk '/^Pages:/ {print $2}')"
    fi
    if command -v pdftotext >/dev/null; then
      pdftotext "$pdf" "$text"
      text_bytes="$(wc -c <"$text" | tr -d ' ')"
      clinical_hits="$(
        awk '
          BEGIN { IGNORECASE=1; count=0 }
          /allerg|medication|problem|result|vital|procedure|encounter|plan|social|immunization|insurance|directive|instruction|diagnos/ { count++ }
          END { print count }
        ' "$text"
      )"
    fi
    if [[ "${pages:-0}" -gt 0 && "${text_bytes:-0}" -gt 120 ]]; then
      verdict="pass"
    fi
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$sample" "$status" "$pages" "$pdf_bytes" "$text_bytes" "$clinical_hits" "$verdict" \
    >>"$report"
done

echo "Wrote $report"
