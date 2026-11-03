#!/usr/bin/env bash
set -euo pipefail

paper_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$paper_dir"

for paper_tool in latexmk xelatex bibtex rsvg-convert; do
  command -v "$paper_tool" >/dev/null || {
    printf 'Missing whitepaper build dependency: %s\n' "$paper_tool" >&2
    exit 1
  }
done

mkdir -p build
rsvg-convert --format pdf --output figures/private-inference.pdf figures/private-inference.svg
if ! latexmk -xelatex -interaction=nonstopmode -halt-on-error -outdir=build main.tex >build/latexmk.log 2>&1; then
  tail -n 60 build/latexmk.log >&2
  exit 1
fi
cp build/main.pdf slop_ninja.pdf

printf 'Built whitepaper/slop_ninja.pdf\n'
