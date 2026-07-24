# Performance validation

`lsp_performance` exercises the server through its JSON-RPC transport against a
generated 300-file Compact workspace. Each file contains two circuits and one
prefixed import, so the fixture covers file discovery, parsing, symbol caching,
reverse dependencies, completion, navigation, workspace-wide rename, and
diagnostic scheduling.

The fixture is generated in a temporary directory. It does not depend on a
third-party contract corpus or network access, which keeps local and CI runs
reproducible. A fast local compiler stub exercises the diagnostic process
boundary without requiring a Compact toolchain installation.

## Run the guard

Run the same unoptimized test used by the default CI suite:

```bash
cargo test -p compact-lsp --test lsp_performance -- --nocapture
```

Use a release build when comparing implementation changes:

```bash
cargo test --release -p compact-lsp --test lsp_performance -- --nocapture
```

The test prints:

- Workspace startup, from `initialized` until the server reports ready.
- A workspace-symbol request completed before workspace readiness while indexing
  is still active.
- Local completion and go-to-definition latency.
- Workspace-wide rename latency.
- Diagnostics latency from `didOpen` until `publishDiagnostics`.

The request issued during indexing verifies an important responsiveness
invariant: scanning is allowed to use CPU and disk, but it must not block the
async JSON-RPC runtime.

## CI thresholds

The checked-in limits are intentionally broad:

| Measurement | Limit |
|---|---:|
| Complete test | 45 s |
| Startup indexing | 15 s |
| Request during indexing | 5 s |
| Completion or definition | 2 s |
| Workspace rename | 4 s |
| Diagnostics | 5 s |

These are regression guards, not performance promises. They are designed to
catch blocking work, accidental quadratic behavior, deadlocks, and
order-of-magnitude slowdowns while tolerating shared CI runner load.

## Source-index optimization

Workspace indexing and ordinary completion need both declarations and imports.
Previously they called two analyzer methods, and each method parsed the same
source independently. `ParserEngine::index_source` now collects both results
from one tree. The server also updates its source, symbol, and dependency caches
from that single snapshot so the cached data cannot disagree about which parse
it represents.

The following release-mode measurements were collected on 2026-07-23 on Apple
Silicon. Each cell is the median of three runs of the generated 300-file
fixture; absolute values will vary by machine.

| Measurement | Two parses | One parse |
|---|---:|---:|
| Startup | 211.5 ms | 203.1 ms |
| Request during indexing | 0.455 ms | 0.426 ms |
| Completion | 2.157 ms | 1.947 ms |
| Definition | 0.159 ms | 0.182 ms |
| Rename | 28.306 ms | 28.386 ms |
| Diagnostics | 30.647 ms | 30.456 ms |

The change removes one full parse wherever both collections are required. The
end-to-end improvement is intentionally modest because file traversal,
serialization, and cross-file reference work remain unchanged. The benchmark
keeps those surrounding costs visible instead of reporting an isolated parser
microbenchmark.

When changing indexing or request handling, update this page with the fixture
shape, command, build profile, machine class, and before/after results. Do not
tighten CI thresholds to match one development machine.
