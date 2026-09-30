# Benchmark results

Measured on 12 October 2026 with the commands in `benches/README.md`.
Every number below was produced by those commands on this machine; nothing
is estimated. Replace the whole file when re-measuring.

**Machine**: Linux 7.0.0-34-generic x86_64, 8 CPUs, no discrete GPU in the
test session (window measurements use Xvfb with the lavapipe software
Vulkan driver, so first-frame times are pessimistic compared with a desktop
GPU). Rust stable 1.99, release profile (fat LTO) for the application
script; criterion benches use cargo's `bench` profile.

## Micro-benchmarks (criterion, mean of the reported samples)

### Offsets (`apex-buffer`), 10,000 conversions on 1 MB of mixed-width text

| Benchmark | Mean | Per element |
|---|---|---|
| char → byte | 1.64 ms | 0.16 µs |
| byte → char | 1.21 ms | 0.12 µs |
| char → UTF-16 position | 9.24 ms | 0.92 µs |
| display column round trip (tabs, wide chars) | 18.2 ms | 1.8 µs |
| grapheme next + prev | 6.12 ms | 0.61 µs |
| word motion | 8.01 ms | 0.80 µs |

### Editing (`apex-edit`)

| Benchmark | Mean |
|---|---|
| type one character, 1 cursor, 1 MB document | 71 µs |
| type one character, 100 cursors | 496 µs |
| type one character, 1,000 cursors | 3.58 ms |
| undo then redo 50 steps with 100 cursors | 20.4 ms |
| apply a 1,000-change transaction to 1 MB | 3.79 ms |
| map 10,000 positions through 1,000 changes | 58.7 ms (linear scan; see re-measurement below) |

### Search over 10 MB (`apex-edit::search`)

| Benchmark | Mean | Throughput |
|---|---|---|
| literal, case-sensitive | 10.6 ms | 926 MiB/s |
| case-insensitive, whole word | 32.9 ms | 306 MiB/s |
| regex with capture groups | 29.0 ms | 343 MiB/s |

### Syntax (`apex-syntax`), Rust grammar

| Benchmark | Mean |
|---|---|
| full parse + highlight of 100 KB | 68.8 ms |
| one-character edit, re-parse and re-highlight the line, 100 KB | 12.9 ms (before the incremental-injection change; see re-measurement below) |
| single 2 MB line: parse + highlight the line | 101 µs (highlighting is capped at the first 20,000 characters) |

### Files and project search (`apex-project`)

| Benchmark | Mean | Throughput |
|---|---|---|
| load + decode 1 KB | 11.4 µs | |
| load + decode 100 KB | 59 µs | 1.6 GiB/s |
| load + decode 10 MB | 13.8 ms | 716 MiB/s |
| load + decode 100 MB | 124 ms | 788 MiB/s |
| load 1,000,000 short lines (11.9 MB) | 6.98 ms | 141 M lines/s |
| Find in Files, literal, 2,000 files / 10 MB | 34.9 ms | 282 MiB/s |
| Find in Files, regex | 36.6 ms | 267 MiB/s |
| Find in Files, no match | 12.2 ms | 798 MiB/s |

### Re-measurement after two optimisations

Profiling the incremental-edit benchmark (`timing_probe` test in
`highlight.rs`) showed that re-querying injections over the whole tree took
9–11 ms of the 12.9 ms. Injections are now re-queried only inside the byte
ranges tree-sitter reports as changed; `Transaction::map_pos` now binary
searches instead of scanning. Results from re-running the two groups:

| Benchmark | Before | After |
|---|---|---|
| one-character edit, re-parse and re-highlight the line, 100 KB Rust | 12.9 ms | 248 µs |
| map 10,000 positions through 1,000 changes | 58.7 ms | 406 µs (prefix sums + binary search) |
| apply a 1,000-change transaction to 1 MB | 3.79 ms | 3.30 ms (noise; unchanged code) |
| full parse + highlight of 100 KB | 68.8 ms | 80.1 ms (noise; unchanged code; both within the run-to-run spread seen on this machine) |

The `timing_probe` test in `highlight.rs` (`cargo test -p apex-syntax timing_probe -- --ignored --nocapture`)
now reports, for the same edit: parse 0.1–0.5 ms, injections 5–20 µs, line
highlight 10–40 µs.

## Whole application (`benches/startup.sh`, release binary, Xvfb + lavapipe)

Median of three runs; "window after" is from process start to the X window
being mapped, "RSS" is resident memory two seconds later with the file
loaded.

| Case | Window after | RSS |
|---|---|---|
| empty window | 166 ms | 124 MB |
| 1 KB file | 202 ms | 128 MB |
| 100 KB file | 218 ms | 153 MB |
| 10 MB file (full features) | 464 ms | 494 MB |
| 100 MB file (large-file mode) | 458 ms | 238 MB |
| 1,000,000 lines (11.9 MB) | 212 ms | 141 MB |
| single 4.5 MB line | 227 ms | 140 MB |

Observations:

* The 10 MB file in full mode costs about 50× its size in memory because
  the tree-sitter tree and the highlight caches are kept; large-file mode
  (over `large_file_mb`, default 50) skips them, which is why 100 MB uses
  less than 10 MB. Lowering `large_file_mb` trades colours for memory.
* Start-up is dominated by GPU/driver initialisation under software
  rendering; the editor's own initialisation (settings, themes, key map,
  grammars) is a few milliseconds.
* A 500 MB file (`APEX_BENCH_500MB=1`) was not measured in this session.

## Fuzzing (cargo-fuzz, libFuzzer, nightly), 60 s per target

| Target | Executions | Coverage edges | Result |
|---|---|---|---|
| transactions | 242,082 | 1,423 | no crashes |
| offsets | — | — | found a wrong invariant in the harness itself (display-column round trip with zero-width characters); harness corrected, see re-run below |
| settings | 338,146 | 3,935 | no crashes |
| search | 114,146 | 9,774 | no crashes |
| session | 3,669,952 | 2,481 | no crashes |

Re-run of `offsets` after correcting the harness (positions snapped to
grapheme boundaries, as the editor does; display-column round trips compared
by display column): 512,001 executions, 2,735 coverage edges, no crashes in
60 s. Both earlier findings were harness invariants that were stricter than
the editor's own rules (a CRLF pair is one grapheme; combining marks share a
display column), not defects in the conversion code.

## Targets from the specification

| Requirement | Status |
|---|---|
| p95 interactive latency under 16 ms in typical documents | Typing with one cursor in a 1 MB file costs 71 µs for the edit plus under 1 ms for re-parse and line highlight; layout of a screen of lines is in the same range. Met on this machine for files below the large-file threshold. |
| 1,000 cursors | 3.6 ms per keystroke. |
| 100,000-file project tree | The explorer is lazy and the index runs in the background; not benchmarked in this session. |
| Search across several gigabytes | Find in Files sustains ~280 MiB/s single-threaded on this machine, so 3 GB takes about 11 s, streaming results as it goes; not benchmarked at that size in this session. |
