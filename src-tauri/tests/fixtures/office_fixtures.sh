#!/usr/bin/env bash
# Build the features.* office fixtures from the flat OpenDocument sources with LibreOffice.
#
# Run: bash office_fixtures.sh   (needs LibreOffice; set SOFFICE when soffice is not on PATH)
#
# features.fodt, .fodp and .fods hold headers, footers, notes, comments, text boxes, tables,
# speaker notes and typed cells, each marked by a distinct word, so the reference texts from
# expected_text.py show what Tika includes and in which order. Each source is converted to its
# Office Open XML and its OpenDocument form. Regenerate expected_text.json afterwards.
set -euo pipefail
cd "$(dirname "$0")"
SOFFICE="${SOFFICE:-soffice}"

convert() {
  "$SOFFICE" --headless --convert-to "$2" --outdir . "$1"
}

convert features.fodt 'docx:MS Word 2007 XML'
convert features.fodt odt
convert features.fodp 'pptx:Impress MS PowerPoint 2007 XML'
convert features.fodp odp
convert features.fods 'xlsx:Calc MS Excel 2007 XML'
convert features.fods ods
ls -l features.docx features.odt features.pptx features.odp features.xlsx features.ods
