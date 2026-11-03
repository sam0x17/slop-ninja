"""NLP-only subprocess boundary. All corpus/experiment orchestration is Rust."""

import argparse
import json
import sys
import unicodedata

from unslop_nlp import _grammar


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="en_core_web_sm")
    args = parser.parse_args()
    text = sys.stdin.buffer.read().decode("utf-8")
    identity, families, totals, metrics = _grammar(text, args.model)
    json.dump({"extractor": identity + ";python-unicode=" + unicodedata.unidata_version,
               "families": families, "totals": totals, "metrics": metrics},
              sys.stdout, ensure_ascii=False, allow_nan=False)


if __name__ == "__main__":
    main()
