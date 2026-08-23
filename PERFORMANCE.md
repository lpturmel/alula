# Performance report

Measured on 2026-08-23 on an Apple M1 Max (10 cores, 64 GB), macOS 26.6.2,
Rust 1.97.1. CPU results are medians of five warmed runs using the repository's
`profiling` Cargo profile. The fixture contains a 30,000-record JSON response,
128 environment variables with 256 references, 100 saved requests, and 500
history entries.

| Workload | Before | After | Change |
| --- | ---: | ---: | ---: |
| Large JSON response formatting | 37.851 ms | 15.458 ms | **59.2% faster** |
| Formatting throughput | 60.7 MiB/s | 148.5 MiB/s | **144.6% higher** |
| Formatter peak RSS | 71.8 MiB | 21.6 MiB | **69.9% lower** |
| Variable resolution | 36.711 µs | 26.346 µs | **28.2% faster** |
| Persisted state load | 13.003 ms | 10.945 ms | **15.8% faster** |
| Persistence snapshot clone | 373.284 µs | 82.948 µs | **77.8% faster** |
| Persisted state save | 20.394 ms | 15.856 ms | **22.3% faster** |
| Steady idle main-thread CPU | 26.6% | 0.3% | **98.9% lower** |

The peak-memory comparison uses the retained `format-legacy` workload, which
executes the previous JSON DOM and fenced-block construction path in the same
binary as the optimized workload.

## Changes behind the results

- JSON is transcoded directly from the parser into the pretty serializer. The
  previous implementation allocated a complete `serde_json::Value` tree and
  traversed it again.
- Highlighting blocks now reference ranges in the formatted body while they are
  assembled, eliminating a temporary string allocation and copy for every
  block.
- Large variable environments build a lookup index when a template contains
  enough references to amortize it.
- Workspace, history, and environment TOML files load and save concurrently.
- History entries use copy-on-write shared storage, making the periodic
  persistence snapshot cheap when history has not changed.
- Live formatted previews publish at 32 KiB intervals instead of cloning the
  entire accumulated preview after every network fragment.
- The redundant full persisted-state clone before opening the first window was
  removed, and credential-store reads are hydrated off the UI thread.
- History and WebSocket transcript rows use GPUI's lazy `uniform_list`; a
  500-entry history now builds roughly 11 visible rows instead of all 500 on
  each render.
- History and Environment pages retain only the response viewer state off-page
  instead of rebuilding the complete hidden request composer.
- URL and key/value variable diagnostics are cached on input changes rather
  than reparsed during every paint, and request-tab environment menu data is
  shared across tabs for each render.
- TextView suppresses duplicate text and style messages, avoiding recurring
  channel traffic and allocations for unchanged formatted responses.
- HTTP stream fragments are coalesced and stream bursts are drained in one UI
  update. WebSocket messages remain individually inspectable while the detail
  editor is synchronized only once per burst.
- Idle persistence checks read an atomic dirty flag without entering the app
  entity, theme metadata/configuration reads run off-thread, and the decorative
  MCP-ready dot no longer forces a perpetual full-window animation.

## Flamegraphs

The sampled profiles are saved under `target/flamegraphs/`:

- `format-final.json` — optimized response formatting
- `variables-final.json` — indexed variable resolution
- `startup-final.json` — persisted state parsing
- `app-startup-final.json` — native application startup and first renders
- `ui-interaction-final2.json` — repeated navigation and request editing
- `ui-idle-symbolicated.json` — idle UI baseline before the final UI-thread fixes
- `ui-idle-final.json` — idle UI after polling and perpetual-animation fixes

The formatting profile originally identified the JSON value-tree round trip,
fenced-block staging allocations, `memcpy`, and repeated fence scans as the hot
path. After the changes, time is concentrated in streaming JSON parsing,
serialization, and the unavoidable final output copy. The native application
profile shows GPUI/Taffy layout and bounds-tree work as the main post-startup UI
cost, which motivated virtualizing both large row collections. A later idle
control trace exposed a 150 ms entity poll, synchronous theme file metadata,
and a repeating status-dot animation. Removing those reduced steady idle
main-thread CPU from 26.6% to 0.3%. In the final interaction trace, the main
thread used 65.8 ms of CPU after startup across roughly nine seconds of repeated
page switching, while returning to the near-zero idle baseline between actions.

Rebuild and reproduce the microbenchmarks with:

```sh
cargo build --profile profiling -p alula --example perf_profile
target/profiling/examples/perf_profile all 1
target/profiling/examples/perf_profile format-legacy 1
target/profiling/examples/perf_profile variables-legacy 1
```

## Further disk/startup opportunities

1. Keep TOML as the editable source of truth but write a versioned binary cache
   after successful parsing. Validate it with schema version, source size, and
   modification time; a valid cache would avoid most `winnow`/TOML parsing on
   subsequent launches.
2. Move history to an append-oriented store such as SQLite in WAL mode. Alula
   currently rewrites the bounded history document; incremental inserts would
   reduce write amplification and make loading only the newest entries cheap.
3. Load history after the first window becomes visible. Workspace and
   environments are required to construct the request editor, while history is
   not needed until its navigation section is opened.
4. Build release distributions with thin LTO and one codegen unit after
   measuring build-time impact. This can improve startup instruction locality,
   but should be evaluated separately from development builds.
