# Benchmarks

Two kinds of measurement, both reproducible from a clean checkout:

| What | Command | Where |
|---|---|---|
| Micro-benchmarks (criterion) | `cargo bench --workspace` or `make bench` | `crates/*/benches/` |
| Whole application: start-up, large files, memory | `cargo build --release && benches/startup.sh` | `benches/startup.sh` |
| Fuzzing | `cargo install cargo-fuzz` then `cd fuzz && cargo +nightly fuzz run <target>` | `fuzz/` |

Results with the machine they were measured on are kept in
`docs/performance/benchmarks.md`. Never paste numbers you did not measure.

## Micro-benchmarks

* `apex-buffer` `offsets`: char/byte/UTF-16/display-column conversions and
  grapheme and word motions over 1 MB of mixed-width text.
* `apex-edit` `editing`: typing with 1, 100 and 1,000 cursors into 1 MB;
  undo/redo of 50 steps; mapping 10,000 positions through a 1,000-change
  transaction; literal, case-insensitive and regex search over 10 MB.
* `apex-syntax` `highlight`: full parse and highlight of 100 KB of Rust;
  one-character incremental edit and re-highlight; a single 2 MB line.
* `apex-project` `io`: load and decode 1 KB to 100 MB files (set
  `APEX_BENCH_500MB=1` to add 500 MB), one million lines, and project search
  over a generated 2,000-file tree.

Run one group with `cargo bench -p apex-edit -- search` and compare against
the previous run with criterion's built-in baseline support
(`cargo bench -- --save-baseline before`, then `-- --baseline before`).

## Whole-application script

`benches/startup.sh` starts the release binary in a fresh profile for each
case, waits for the window to appear (xdotool), then reads resident memory
from `/proc`. Cases: empty window, 1 KB, 100 KB, 10 MB, 100 MB (large-file
mode), one million lines, a single 4.5 MB line, optionally 500 MB. Without a
display it starts Xvfb with the lavapipe software renderer, which makes the
first frame several times slower than on a GPU; say which it was when
recording results.

## Profiling

* `cargo build --profile profiling` is the release build with symbols, for
  `perf record -g target/profiling/apex`, `samply` or Instruments.
* `APEX_FRAME_TIME=1 apex` shows the frame interval in the status bar; with
  `APEX_LOG=debug` the language-server client and background tasks log their
  timings.
* egui repaints only on input or when a background task asks for a frame, so
  an idle editor draws nothing; `APEX_FRAME_TIME` therefore shows the
  interval between the last two frames, not a continuous rate.

## Fuzzing

Targets in `fuzz/fuzz_targets`: `transactions` (apply/undo round trip and
position mapping), `offsets`, `settings` (settings, key map and theme
parsing), `search` (patterns and replacements), `session` (session and
recovery files). A short pass of every target:

```sh
cd fuzz
for t in transactions offsets settings search session; do
  cargo +nightly fuzz run $t -- -max_total_time=60
done
```

Crashes are written to `fuzz/artifacts/<target>/`; reproduce with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`.
