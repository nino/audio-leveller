#!/usr/bin/env bash
#
# Build the explainer as an EPUB.
#
#   docs/explainer/build-epub.sh
#
# Requires pandoc (built and checked against 3.1.3). The output is committed
# alongside the source, so anyone can read it without a toolchain — which only
# works if a rebuild that changes nothing produces a byte-identical file.
# Two things make that true: the identifier is pinned in metadata.yaml rather
# than minted per run, and SOURCE_DATE_EPOCH pins the timestamps pandoc stamps
# into content.opf and the zip entries. Without those, every rebuild would show
# up as a diff in a 74 KB binary.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

command -v pandoc >/dev/null || {
  echo "pandoc not found — see https://pandoc.org/installing.html" >&2
  exit 1
}

# Derived from the date in metadata.yaml, so the stamp moves only when the
# prose does. `date -d` is GNU; on macOS this needs coreutils' gdate.
doc_date="$(sed -n 's/^date: *//p' "$here/metadata.yaml" | head -1)"
SOURCE_DATE_EPOCH="$(date -u -d "${doc_date}T00:00:00Z" +%s)"
export SOURCE_DATE_EPOCH

# --split-level=2 puts each `##` section in its own file. One 145 KB XHTML
# document makes e-readers page slowly and lose their place; nine chapter-sized
# ones do not.
pandoc \
  --from=markdown \
  --to=epub3 \
  --standalone \
  --split-level=2 \
  --toc \
  --toc-depth=3 \
  --css="$here/epub.css" \
  --metadata-file="$here/metadata.yaml" \
  --output="$here/audio-leveller-pipeline.epub" \
  "$here/pipeline-explained.md"

echo "wrote $here/audio-leveller-pipeline.epub"

# epubcheck is not required, but if it happens to be installed, use it.
if command -v epubcheck >/dev/null; then
  epubcheck "$here/audio-leveller-pipeline.epub"
fi
