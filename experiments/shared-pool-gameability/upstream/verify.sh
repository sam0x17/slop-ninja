#!/bin/sh
set -eu
bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$bundle"
jq -r '.files[] | [.sha256,.local_path] | join("  ")' manifest.json | shasum -a 256 -c -
adapted=$(mktemp)
trap 'rm -f "$adapted"' EXIT
sed \
 -e 's/use crate::alloc::borrow::ToOwned;/use std::borrow::ToOwned;/' \
 -e 's/use safe_math::\*;/use super::compatibility::*;/' \
 -e 's/use sp_runtime::traits::CheckedAdd;/use num_traits::CheckedAdd;/' \
 -e 's/use sp_std::vec;/use std::vec;/' \
 -e 's/use sp_std::vec::Vec;/use std::vec::Vec;/' \
 source/pallets/subtensor/src/epoch/math.rs > "$adapted"
cmp "$adapted" math_standalone.rs
