# Real-file reader audit

Goal: “try the libraries with real files and eyeball them to see if the load
worked”; for defects, “make synthetic test files and then fix the unit test”.

Archive: `/mnt/nas-home/Racing/collection/`. Original files were read only.
Conversions and plots were written to `target/real-file-audit/`, never into
that archive. Regression tests construct synthetic data and need no NAS.

## Coverage and current evidence

The earlier investigation scanned 2,197 PDS/LD/VBO paths and part of the MP4
collection. Those preliminary `/tmp` reports did not survive the machine
restart. Some flags were wrong because the original probe compared raw SI
pressure with a bar threshold, angular minutes with degrees, and missing
speed with zero. **Do not treat those preliminary counts as a clean bill of
health or as confirmed defects.**

The 2026-08-29 continuation re-opened five specific real files, inspected
speed/pedal/counter plots, converted them without processing passes, and
verified the resulting native archives. The table below is the fresh check,
not another complete archive scan. “Complete” here means the library has both
boundaries; it is not a certification that every physical crossing is known.

| Cached file | Result | Visual / numerical cross-check |
|---|---|---|
| `daytona-reset-counter.vbo` | 38 intervals, 36 complete | Speed reaches 305.3 km/h, throttle reaches 99.4%, brake 76.9 bar. Raw counter repeatedly resets; GPS-derived boundaries continue. No spurious 29-minute intervals. |
| `daytona-radio.pds` | 5 intervals, 3 complete | Flying laps 107.434, 102.592, 102.975 s (rounded). Gaps are visible in the trace rather than filled with later values. These agree to millisecond resolution with the logger times checked in the original investigation. |
| `indy-bad-gps.vbo` | 8 intervals, 6 complete | GPS jump detection rejects gate inference; counter produces 80.2–85.4 s complete laps. Speed/brake/throttle cycles agree with the counter sequence. |
| `sebring-frozen-counter.pds` | 1 incomplete interval, no complete laps | Repeated racing-speed/pedal cycles up to 263.4 km/h while the raw counter stays at 1. `laps.long_lap_while_moving` correctly asks for review. Brake pressure converts to 65.985 bar, not millions of bar. |
| `tds-export.ld` | No laps | Speed reaches 300.5 km/h and pedals cycle normally. `Driver Throttle Pos` and `Brake Pos` are selected and normalize as fractions. No lap/GPS source exists in this reduced export; missing-lap warning is intentional. |

All five native conversions passed `verify`, and reopened with the same
interval/complete-lap counts. Unknown placement metadata still produces
native catalog warnings; no UTC or timezone was invented.

Original paths, relative to the archive:

- `2025/2025-01-23_IMSA_Daytona-24H/R/VBOX202501261223570001-vbox.vbo`
- `2025/2025-01-23_IMSA_Daytona-24H/FP7/250119152711_25IMSAT01_DAY_FP7_Run005_Unknown_Car11_#477.pds`
- `2025/2025-09-19_IMSA_Indianapolis-6H/misc/VBOX202508231411470001.vbo`
- `2026/2026-02-23_IMSA-TEST_Sebring/CT1/260223155458_26IMSA02_T02_SEB_CT1_Run000_Unknown_MQ12Di_LMP2 #443.pds`
- `2025/unsorted/misc/00_04_42_Oreca07_2026_DIS_Winter_MB_CT3_FL.ld`

The fresh attempt at `2026/unsorted/misc/SCHD0021.MP4` and `SCHD0032.MP4`
timed out after six minutes without a report. Their previous observations
(no GPS fix and a lap-timer resync) have synthetic regressions, but they have
**not** been revalidated with the final build on the real MP4s. No full NAS
walk was restarted during this targeted check.

## Confirmed defects and regression coverage

The initial work addressed VBO unit alignment, stamped PDS chunks, missing
role aliases, LD units in the short-name slot, lap-counter spikes and lag,
timer resyncs, and mmap fallback. The continuation found and corrected these
additional issues in that work and the existing shared code:

- **Grid gaps returned future samples.** End-based chunk lookup selected the
  next run inside a gap. Core lookup now requires a run that has started;
  linear interpolation bridges contiguous splits only.
- **PDS overlap trimming could delete real data.** Equal timestamps do not
  imply equal values. All source values/stamps are now preserved. Genuine
  overlaps are diagnosed; valid sub-period re-phasing is not shifted.
- **Long second-valued timers were interpreted as milliseconds.** Declared
  units now win; magnitude inference is only a fallback for unitless exports.
  Zero/out-of-recording reset boundaries are discarded and duplicates removed.
- **Metadata-only VBO reads ignored satellite quality.** They now retain the
  satellite channel, reject nonfinite satellite counts, and make the same
  gate decision as full reads. A long gap between fixes cannot time a crossing.
- **Default pedals picked the wrong physical signal.** Driver demand precedes
  throttle-plate position; `Brake Pos` precedes pressure. Speed preference now
  requires a convertible speed unit, not just a nonempty string. The healthy
  `Speed_Wspd_App` precedes `vehRefSpeed` (which showed a stationary sentinel
  in Le Mans files during the original investigation).
- **Physical validation compared display values against SI limits.** For
  example, −40 °C is 233.15 K, not negative absolute temperature. Validation
  now applies both scale and affine offset before checking its physical band.
- **Motion validation was unbounded in declared duration.** A shared bounded
  motion estimator now excludes invalid/stale samples, counts gaps as missing
  rather than stationary, and weights fractional final bins correctly. The
  `u64::MAX`-duration synthetic test does at most 4096 speed lookups.
- **The audit probe emitted invalid JSON for NaN/infinity and false alarms
  from unit mismatches.** It now uses `serde_json`, represents missing extrema
  as null, converts comparison units, retains unnormalizable channels as raw,
  and includes combined reader/plausibility diagnostics.

Synthetic coverage lives in the affected crate unit tests, the probe's tests,
and `crates/motorsport-telemetry/tests/audit_regressions.rs`. The latter writes
an LD with the observed unit/name combination, opens it through the public
facade, checks normalized pedals, converts it to native, and checks again.

## Recovery limits — don't fabricate data

- VBO gate laps are **empirical inference**, not vendor-certified times. The
  implementation uses a perpendicular 50 m gate and quality checks. GPS
  dropouts, pit-lane geometry or unusual gate definitions can hide crossings.
  See [the VBO reader's limitations](../crates/racelogic-telemetry/README.md).
- A PDS with no usable beacon/GPS evidence remains without reliable laps.
  A long moving interval is a review warning, not proof of multiple laps.
- Cosworth PPS/TPS channels declared as radians do not establish a pedal
  full scale. Their normalized fraction stays unavailable. Pressure is not
  silently turned into pedal travel either.
- A unitless AiM CAN speed cannot safely replace unavailable GPS speed in a
  normalized speed field. The probe exposes missing normalized data instead.
- Source corruption and incomplete chunk directories are diagnosed, not
  repaired by guessing sample ownership. Timestamp-inconsistent PDS files
  retain the legacy placement fallback with a warning.
- Raw overlapping PDS samples are retained even if a downstream format cannot
  faithfully represent them. No lossless claim is made for ambiguous time lookup.

## Validation and reproduction

Final tree: **311 tests passed**, zero failed/ignored across workspace targets
and doctests. `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check`, and `git diff --check` passed. The five native
archive conversions verified successfully; that does not remove the
source-data warnings described above.

Upstream CI now runs example regressions as well as library/integration tests
and doctests on Linux, macOS and Windows. Packaging verifies the unpublished
atlas/core workspace pair together using current stable Cargo. Windows CI
also exposed a native-migration bug: replacing the old archive while its
memory map was still alive failed with access denied. Migration now releases
that map after writing the replacement and before renaming it. A synthetic
regression checks byte-for-byte preservation of every non-catalog ZIP member.

```sh
cargo test --workspace --all-targets
cargo test --workspace --doc
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release --bins --example probe

# A targeted manifest avoids recursively enumerating a slow NAS.
target/release/examples/probe --files-from recording-paths.txt > report.jsonl
# Bounded preview traces can be plotted alongside the recovered lap boundaries.
target/release/examples/probe --preview path/to/recording.vbo > preview.jsonl

# Always supply an output under a scratch directory, not the source archive.
target/release/motorsport-telemetry convert --no-passes input.pds scratch/output.telemetry
target/release/motorsport-telemetry verify scratch/output.telemetry
```

Fresh local evidence is under `target/real-file-audit/`: `final-report.jsonl`,
PNG/SVG previews, `native-inspect.json`, `native-verify.log`, red/green
regression logs and workspace/clippy logs. These generated artifacts and the
real-file cache are intentionally not committed. The durable record is this
report plus the NAS-independent synthetic tests.

## 2026-10-04 lap/state collection audit

The new verifier pass attempted all 3,533 supported-extension recordings in
`/mnt/nas-home/Racing/collection`, including native MP4/PDS/LD/VBO and
`.telemetry` documents. Each manifest path has exactly one final result;
supporting documents/media and `@eaDir` are excluded. Earlier partial audits
above remain historical evidence. The parser/verifier revision is `37eb222`
(with lap-normalization semantics from `770dadd`).

| Final category | Files |
|---|---:|
| No review/error findings | 75 |
| Needs review | 2,667 |
| Hard state/physical audit violations | 0 |
| Video without AiM telemetry | 677 |
| Legacy ZIP telemetry archives | 32 |
| Cannot open/decode as supported telemetry | 82 |

These counts are verification outcomes, not physical certification. Missing
trusted GPS, timers, counters or motion evidence remains visible. Review
findings also expose disagreements with stored lap annotations. Unavailable
files include malformed/unsupported vendor files and three PDS recordings
with widespread implausible decoded magnitudes; the verifier refuses those
sources rather than presenting lap conclusions. Legacy ZIP archives require
reconversion from original vendor data; this pass did not migrate them.

All original files were read only. Three large Indianapolis race MP4s initially
timed out; all three succeeded after sequential local staging. The remaining
slow MP4 checks likewise used local byte copies retaining original filenames
and adjacent track metadata. Previously completed final-parser audits were
retained, and older missing-input flags were checked against the shared parser
channel-name lists. Full initial/intermediate/final JSONL, the manifest and
HTML reports are retained in BB thread storage. No NAS rewrites occurred.

The 27-file Road Atlanta weekend was rechecked with explicit 72–120-second
review bounds. Full and metadata-only readers agree on timing, classification,
numbering, stints, completeness and boundary evidence for all 367 intervals;
there are no hard audit violations or lap-duration/file-size coverage
mismatches. Long race laps remain flying laps and may be FCY. GPS pit gates
and motion departure timestamps are estimates, not surveyed boundaries.

The private acceptance test in
`crates/motorsport-telemetry/tests/road_atlanta_verify.rs` passed all nine
reported cases against byte-identical real recordings at the final revision:

| Recording | Required verifier finding |
|---|---|
| `26IMSAR07_PLM_FP1_Run01_MB.MP4` | `recovered-first-crossing` |
| `26IMSAR07_PLM_FP1_Run04_DHH.MP4` | `ignored-pit-lane-beacon` |
| `26IMSAR07_PLM_FP2_Run06_TL.MP4` | `recovered-timer-dropout` |
| `26IMSAR07_PLM_FP3_Run01_TL.MP4` | `recovered-moving-pit-pass` |
| `26IMSAR07_PLM_Q_Run01_TL.MP4` | `ignored-counter-rearm` |
| `SCHD0295.MP4` | `recovered-motion-departure` |
| `SCHD0301.MP4` | `rejected-short-crossing` |
| `SCHD0304.MP4` | `recovered-motion-departure` |
| `SCHD0306.MP4` | `short-in-lap` |

Recovery findings are informational: corrected metadata still explains the
native ambiguity that triggered recovery. `short-in-lap` and unresolved
activity remain review findings. A located stop on the circuit is `Stopped`,
not pit activity; termination alone does not prove a crash. The reducer's
bounded TLC results and mutation checks are documented in `specs/laps/`;
those checks do not prove sensor inference correct or unbounded behavior.

The collection scan also exposed a false 24.907-second flying lap in four
Road America FP2 copies. The unrecognized `Lap_Number_001` counter and an
always-zero reference timer caused timer-only recovery to ignore the sustained
counter reset. Strict numeric-suffix counter recognition plus a permissive
trusted-GPS/atlas physical floor removes that flying lap. All four copies now
verify without hard errors; the short in-lap fragment remains flagged for
review. No upper classification cutoff was added for FCY laps.

To repeat the private acceptance checks:

```sh
ROAD_ATLANTA_CORPUS=/mnt/nas-home/Racing/collection/2026/2026-10-01_IMSA_Road-Atlanta-10H \
  cargo +1.97.1 test --release -p motorsport-telemetry --test road_atlanta_verify -- --ignored
```

For a fresh recursive scan, `motorsport-telemetry verify --json <directory>`
emits one result per candidate, including failures. `--track`, `--layout`,
`--min-lap`, `--max-lap`, `--max-speed` and `--corridor` control the track
checks. The default mixed-track scan uses a permissive atlas physical lower
bound and no upper lap-time cutoff; a default atlas layout does not establish
the recording's actual configuration.
