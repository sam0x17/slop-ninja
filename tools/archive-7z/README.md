# slop_ninja_7z_extract

Streams one member of a 7z archive to stdout, for example `Posts.xml` from a
Stack Exchange data dump, so the Rust pool adapters can read it without a
system 7-Zip install.

```sh
slop_ninja_7z_extract list site.7z
slop_ninja_7z_extract cat site.7z Posts.xml --receipt posts-member.json | slop_ninja_a_pools stackexchange --posts - ...
```

- Decoding uses the pure-Rust `sevenz-rust2` crate, pinned at `=0.23.0`, with
  LZMA, LZMA2, BZip2 and PPMd. Encrypted archives are not supported.
- Members before the requested one are decoded and discarded, so solid blocks
  stay aligned. The archive's CRC-32 is checked while the member is read; a
  mismatch, a missing member or a size difference is an error.
- `--receipt` writes the member's byte count, verified CRC-32 and SHA-256.
- The tool writes only to stdout and the optional receipt path.

Tests use synthetic archives built in a temporary directory:
`cargo test` from this directory.
