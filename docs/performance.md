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

## Resident memory

The default latency guard does not measure process memory. Run the ignored RSS
benchmark in release mode to start the packaged server over stdio, generate
Compact workspaces, and sample the server's resident set size:

```bash
cargo test --release -p compact-lsp --test lsp_performance \
  resident_memory_benchmark -- --ignored --nocapture
```

The default run indexes 300- and 1,000-file workspaces. The 1,000-file scenario
also repeats completion requests, document edits, and document open/close
cycles. Set `COMPACT_LSP_RSS_FULL=1` to add 1-, 3,000-, and 10,000-file scaling
points plus a denser 1,000-file workspace.

The churn counts can be changed with `COMPACT_LSP_RSS_COMPLETIONS`,
`COMPACT_LSP_RSS_EDITS`, and `COMPACT_LSP_RSS_OPEN_CLOSE`. On macOS, set
`COMPACT_LSP_RSS_LEAKS=1` to also run Apple's malloc leak inspector against the
stressed server process.

The harness reports RSS after workspace readiness, after each kind of churn,
after a one-second settling period, and at the observed peak. It always uses
generated sources and a local compiler stub. A POSIX `ps` implementation is
required; RSS collection currently supports macOS and Linux.

RSS is physical memory currently resident for the process. It can rise when the
allocator warms up and fall when the operating system reclaims pages. Virtual
size includes reserved address space and is not a substitute for RSS. Compare
runs made on the same operating system, architecture, build profile, and
similar machine load.

### Apple Silicon baseline

The following release-mode measurements were collected on 2026-07-24 from
commit `6fa6bfc9f8993ce81a5b393768f308802e121059`:

| Scenario | Observed RSS |
|---|---:|
| 1 generated file, 2 symbols | 7.6 MiB peak |
| 300 generated files, 2 symbols each | 8.5 MiB peak |
| 1,000 generated files, 2 symbols each | 10.7 MiB ready; 16.1 MiB peak |
| 3,000 generated files, 2 symbols each | 16.1 MiB peak |
| 10,000 generated files, 2 symbols each | 35.7 MiB ready; 36.5 MiB peak |
| 1,000 generated files, 20 symbols each | 22–25 MiB peak |

A longer soak sent 50,000 completion requests, 10,000 document edits, and
20,000 open/close cycles. RSS peaked at 18.2 MiB and settled at 15.8 MiB after
macOS reclaimed resident pages. A separate 5,000-cycle run completed Apple's
`leaks` inspection with zero reported leaks.

These observations are regression evidence, not resource guarantees. The
project does not currently enforce an RSS limit in CI because allocator and
runner differences make a threshold derived from one machine misleading. Add a
broad Linux ceiling only after repeated hosted-runner measurements establish a
stable range.
