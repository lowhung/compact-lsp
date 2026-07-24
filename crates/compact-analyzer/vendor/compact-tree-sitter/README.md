# Vendored Compact tree-sitter parser

The generated parser is copied unchanged from
[`midnames/compact-tree-sitter`](https://github.com/midnames/compact-tree-sitter)
at revision `9ea58fc47c88af914599c368cce031ad6343965b`.

Included files:

- `parser.c`: SHA-256
  `fa4e01563617f1e1b5872fb81b7a835c03c553c3e5f7d4392be35b7f4d62c55b`
- `tree_sitter/parser.h`: SHA-256
  `180b893c8734778fd32f372dfbc27bd6ad1cd2221f26150b31256ff6716320d2`

The source project and these vendored files are licensed under Apache-2.0; see
`LICENSE-APACHE` and `NOTICE` in the crate package.

Vendoring the generated C parser keeps release and Cargo source packages
reproducible without requiring a Git dependency. When updating the grammar,
copy these two files from a reviewed revision, update the hashes and revision
above, and run the 0.33 parser fixtures plus the full workspace test suite.
