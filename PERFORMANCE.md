# Performance report

## Whole-app follow-up pass (2026-08-30)

This pass targeted the remaining startup, persistence, large-collection, HTML,
networking, and production-codegen costs. The before figures are warmed
baselines captured immediately before the changes; the after figures are
medians of five warmed runs with the repository's `profiling` profile.
Measurements used the same Apple M1 Max and Rust 1.97.1 toolchain described
below.

| Workload | Before | After | Change |
| --- | ---: | ---: | ---: |
| HTML response formatting | 21.704 ms | 7.659 ms | **64.7% faster** |
| Variable resolution | 42.203 µs | 26.268 µs | **37.8% faster** |
| Persisted-state load (warm cache) | 12.450 ms | 1.128 ms | **90.9% faster** |
| Persisted-state save | 21.631 ms | 21.017 ms | **2.8% faster** |
| 100-tab / 10,000-request environment sync | 11.760 ms | 293.694 µs | **97.5% faster** |
| 100-tab assignment lookup in 10,000 requests | 4.044 ms | 345.964 µs | **91.4% faster** |
| Bounded 500-entry history insertion | 14.902 µs | 1.393 µs | **90.7% faster** |
| Production benchmark binary | 1.95 MB | 1.61 MB | **17.8% smaller** |

The native QA build also opened and scrolled an environment containing 10,000
saved requests and 10,000 variables. Both pages construct only the visible
rows; scrolling five pages advanced the request list from request 100000 to
the 100072–100081 range without a layout stall.

### Startup follow-up

History loading and response-language registration now begin after GPUI has
rendered the first frame. A side-by-side warm-cache benchmark of the unchanged
full-state path and the new startup-critical path measured medians of 842.907
µs and 250.223 µs respectively, removing **70.3%** of state-loading time from
the pre-window path. The native QA build also opened with a 500-entry fixture
and returned those deferred entries through its live agent interface after the
first frame.

### Changes in the follow-up pass

- Versioned, size-bounded binary caches sit beside the editable TOML state.
  Cache validity is tied to the TOML file's size and nanosecond modification
  timestamp. Missing, stale, corrupt, oversized, or older-format caches fall
  back to TOML and rebuild without becoming load errors.
- Startup loads only the workspace and environments needed to build the first
  window. History loads after the first frame and merges behind any entries
  recorded in the meantime; persistence waits for that merge so it cannot
  overwrite history with the temporary empty state.
- Tree-sitter's global response-language registry initializes after the first
  frame. A one-time guard still initializes it synchronously if an unusually
  fast response arrives before the background warm-up finishes.
- TOML and cache representations serialize concurrently, unchanged files are
  not rewritten, and unique temporary filenames prevent overlapping saves
  from colliding.
- Open-request synchronization builds one lookup table instead of scanning all
  open tabs for every saved request. Batch assignment checks retain only the
  requested tab IDs instead of materializing the entire environment index.
- Root-level environment request lists now use GPUI's lazy `uniform_list`.
  Collapsed folder sections no longer clone hidden request drafts, and empty
  history searches no longer build and lowercase a searchable string for all
  500 entries on every paint.
- History storage uses a copy-on-write `VecDeque`, making newest-first bounded
  insertion constant-time instead of shifting the full history vector.
- HTML formatting writes directly into one output buffer, avoids per-tag
  lowercase strings, and performs case-insensitive detection without copying
  the remaining document.
- Variable parsing reuses the already-located closing delimiter, validates
  ASCII names bytewise, and avoids falling back to a linear scan after an
  indexed lookup miss.
- HTTP responses retain a 2 KiB first-paint read, then switch to 64 KiB reads.
  Bounded `Content-Length` preallocation reduces reallocations without trusting
  an arbitrarily large server-provided size.
- Production builds use thin LTO and one codegen unit. The `profiling` profile
  explicitly keeps LTO disabled and normal codegen parallelism for fast,
  symbolicated iteration.

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
target/profiling/examples/perf_profile format-html 1
target/profiling/examples/perf_profile environment-sync 1
target/profiling/examples/perf_profile environment-lookup 1
target/profiling/examples/perf_profile history-push 1
```

## Further opportunities

1. Move history to an append-oriented store such as SQLite in WAL mode. Alula
   currently rewrites the bounded history document; incremental inserts would
   reduce write amplification and make loading only the newest entries cheap.
2. Flatten expanded folder trees into a virtual row model. Root-only large
   environments are now lazy, but an expanded folder containing thousands of
   direct requests still builds all of that folder's visible rows.
3. Move response formatting into a cancellable worker generation so a newer
   request can supersede CPU work for an older multi-megabyte response.
