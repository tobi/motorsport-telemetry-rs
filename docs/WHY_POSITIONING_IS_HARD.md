# Why Positioning Is Hard

*Lap progress: what it is, why every sensor lies to us, what this repo already
does, and the architecture for doing it right.*

This document is about one channel: **`Lap Progress`** — a number in `[0, 1)`
that says how far around the track a car is. It sounds like a division. It is
actually the hardest signal in the whole pipeline, because it is the only one
that has to be *comparable*: between laps, between drivers, between cars,
between recorders, and between a `.telemetry` file and the video frame it
points at. Everything else we record is allowed to be merely true for one car
at one instant. Progress has to be true for the *track*.

---

## 1. What we are actually computing

### 1.1 Progress is a property of the track, not of the car

Two cars take different lines through a corner. The car on the wide line
drives measurably farther — over a lap the driven distance differs by tens of
meters between drivers, and it differs lap-to-lap for the *same* driver. This
is not noise; it is the sport. So "meters driven since start/finish" is a
per-car quantity and can never be the comparable channel.

The only definition that is comparable across lines is geometric. Take a
reference curve for the layout — a centerline spline `C(s)`, parameterized by
arc length `s ∈ [0, L)`. At every `s`, erect the **gate** `G(s)`: the line
through `C(s)` perpendicular to the local direction of travel, spanning the
track (and a tolerance beyond the edges). Then:

> **A car is at progress `p = s / L` at the instant it crosses gate `G(s)`.**

When driver A is at `p = 0.59693` and driver B is at `p = 0.59693`, they are
on the *same gate* — the same crack in the same piece of asphalt, projected
across the track width — regardless of the line either one took to get there.
That is exactly the "spline that intersects the center of the track in the
direction of travel" definition, and it is the contract every algorithm below
must satisfy.

Two consequences fall out immediately:

1. **Progress rate is not proportional to wheel speed.** A car on a wide line
   crosses gates *slower per meter driven* than a car hugging the apex. Any
   scheme that integrates the odometer and divides by lap length is
   implicitly assuming everyone drives the reference line. It is wrong by
   exactly the line difference — meters, i.e. tenths of seconds — and worst
   in corners, which is where we look.
2. **The reference curve is part of the answer.** Progress values computed
   against two different centerlines (or the same centerline resampled
   differently) are *not comparable*. The reference must be versioned and its
   identity persisted next to the channel (§6.3).

### 1.2 Requirements

| Requirement | Meaning |
|---|---|
| **Monotone** | Never decreases within a lap, except genuine reversal (spin, reverse gear, tow). A spin that loses no gates holds progress flat; only actually re-crossing gates backwards may decrease it. |
| **Precise** | Target ~1 m of along-track error where data supports it. At Road America (~6.5 km) 1 m ≈ `1.5e-4` of a lap; the channel needs `f64` and honest error bars, not false precision. |
| **Comparable** | Same `p` ⇒ same gate, across cars, sessions, recorders, and reference version. |
| **Total** | Defined for every sample of every file, including files with no GPS and files with no laps — degrading *explicitly* (a quality channel), never silently. |
| **Indexed** | Round-trips to video: `time → frame` (exists today, §4.4) and `progress → time → frame` (the missing index, §6.2). |
| **Wrap-correct** | `p` lives on a circle. Filtering, interpolation, and comparison must be done in unwrapped coordinates (`lap + p`, or cumulative `s`), never on the raw fraction, or every start/finish crossing becomes a discontinuity artifact. |

### 1.3 What "rarely goes backwards" really means

Monotonicity is a *prior*, not a constraint. Enforcing it blindly (e.g.
`p = max(p, prev)`) turns every GPS glitch into a permanent forward jump that
the rest of the lap must absorb. The correct statement: the along-track
velocity `ds/dt` is non-negative with overwhelming prior probability, is
tightly coupled to wheel speed while the tires are rolling, and goes genuinely
negative only when independent evidence agrees (reverse gear, yaw rate
inconsistent with forward travel, speed collapse of a spin). Estimators below
encode it exactly that way.

### 1.4 Where alignment matters most: the end of the straight

Comparability is not uniformly valuable around the lap. The product moment is
two cars approaching the same corner: still flat, end of the straight, just
before one of them lifts. That is where tens of milliseconds of misalignment
are *visible* (one car's braking marker slides past the other's) and where
the differences worth studying — lift point, brake point, entry speed — live.
Mid-corner, speeds are lower and lines legitimately diverge, so the same
absolute error costs far less. Consequences:

- **Weight the error budget by phase.** Smoothing and validation (§7) must
  prize straight-end alignment: a meter mid-corner is acceptable; a meter at
  turn-in is the whole game. Gate-crossing checks should anchor on the gates
  just before braking zones.
- **This is exactly why manual damper alignment works** (§5, S3): bumps and
  surface joints — the good landmarks — live in braking zones and curbs, so
  registering on them puts the error *minimum* at the point of maximum value
  and lets uncertainty grow where it matters least, down the following
  straight.

---

## 2. Why it is hard: measurements from our own data

Everything in this section was measured from the files in
`~/Documents/Telemetry` using this repo's readers (August 2026, IMSA Road
America weekend + Sebring test).

### 2.1 GPS: present ≠ valid, and "valid" ≠ accurate

Three AiM MP4s from the same car, same camera install, same weekend:

| File | GPS samples valid | Detail |
|---|---|---|
| `D1_FP1/…Run01_MB.MP4` | **0 / 45 858** | Entire 30-min session: `GPS Fix Type = 0`, `GPS Satellites = 0`, `GPS Position Accuracy = 4 294 967.29 m` (u32 sentinel), `GPS DOP = 99.99`, lat/lon/speed/heading all NaN. The channels exist at a clean 25 Hz the whole time. |
| `D1_FP1/…Run02_TL.MP4` | **0 / 30 477** | Same. |
| `D2_Quali/…Run01_TL.MP4` | **26 281 / 29 768 (88 %)** | First fix 33.2 s in. Position accuracy (receiver's own estimate): p50 **5.1 m**, p90 12.1 m, p99 41 m, max 1 526 m. Satellites p50 = 7 (min 3). 1 559 samples degraded to 2-D fix. **91 dropout gaps ≥ 0.2 s**, worst gaps 10.8 s and 10.7 s — at race speed, ~500 m of missing position each. |

**Scope: these numbers are per-video, not per-library.** The table is the
2026 Road America weekend (`26R05_RAM`), current-generation cameras — only
those later videos carry the new GPS tracks at all. The library also holds
older SmartyCam 1/2 recordings that predate GPS hardware; no fix will ever
appear in them and S3 (§5) is their ceiling. The trap is that the aimd
container declares the same full channel layout either way (54 channels, GPS
included, on every file scanned), so "has GPS channels" is a property of the
*container*, "has GPS" is a property of the *session*, and only per-region
validation (`gps.quality`, §5.5) tells them apart.

Lessons, in increasing order of brutality:

1. **Channel presence tells you nothing.** Both FP1 files carry a full 25 Hz
   GPS channel set with zero information in it. Capability detection must
   look at fix type / finiteness per *region of time*, not at the channel
   list. (Today, nothing in the workspace reads `GPS Fix Type`,
   `GPS Position Accuracy`, or `GPS Satellites` at all.)
2. **The quality channels themselves need interpretation.** `4294967.29 m` is
   not an accuracy, it's an uninitialized register (u32 max through the cm
   scale factor). Sentinels must be recognized before any gating logic sees
   them.
3. **Even good GPS is an anchor, not a ruler.** A 5 m median means raw
   projection is worth ~`8e-4` of a lap, ~70 ms at straightaway speed —
   thirty times coarser than our 1 m target, before adding the *second* car's
   independent 5 m when comparing two videos. And it drops out for 10 s at a
   time, under exactly the trees/bridges/grandstands that repeat every lap,
   so the errors are *spatially correlated lap after lap* — they do not
   average out where they always happen.
4. **GPS time lies too.** The FP1 receiver reports GPS week 2117
   (2020-08-02) in 2026 — six years off (stale firmware epoch). The repo
   already distrusts it for event dating (`event_date_warning: rejected
   gps_clock date 2020-08-02` from `placement`-fed metadata), but the same
   wrong clock flows into the MTJ header `utc` field, which is the *join key*
   for sidecars (§2.4).

### 2.2 The other sensors, and what each one is actually good for

| Sensor | Rate (our files) | Good | Bad |
|---|---|---|---|
| Wheel speed (`Speed_Wspd_App`, `uSpeed`, `vehRefSpeed`) | 10 Hz (AiM CAN), 50 Hz (PDS) | Superb *short-range relative* distance: 0.5 % scale error over a 100 m braking zone is 0.5 m. High availability. | Unknown and drifting scale: tire radius moves ≥ 1 % with pressure, temperature, wear, and load — 0.5 % is 32 m per Road America lap. Slip under braking/traction, lockups, and (on driven wheels) wheelspin are *signed, correlated* errors, concentrated exactly at corner entry/exit. |
| Lateral g + steering (`STEER`, `I_ACCEL_*`, `G_FORCE_LAT`) | 10–50 Hz | Curvature `κ ≈ a_lat / v²` is a *track-shaped* signal: the sequence of corners is a fingerprint of position, independent of GPS. Great for map-matching and for detecting spins (κ and yaw diverge from any forward-travel hypothesis). | Line-dependent in magnitude (different lines → different κ through the same corner), noisy at low speed, useless on straights (κ ≈ 0 carries no position information). |
| Dampers (`X_FL_DAMPER`, `Damper_FL`) | 50 Hz (PDS) | Bumps, curbs, and surface joints are **fixed to the asphalt** with sub-meter spatial stability. A distinctive bump right before a corner is a natural fiducial — this is the manual PDS alignment trick, and it works because it is genuinely position-locked. | Sparse (only where the track has features), amplitude varies with line/speed/setup, needs a per-track landmark library to use automatically. |
| Lap beacons / timing (`Lap_Number_001`, `Current_Lap_Time` @ 100 Hz, `LapTrigger`, `lap_beacon`) | 10–100 Hz | Hard resets: a beacon crossing pins `p = 0` (well: pins the *beacon gate*) to a few ms. Our FP1 files have working 100 Hz lap timing even with zero GPS. | Only fires ~once per 2 minutes; beacon position must itself be surveyed onto the reference line; missed/double triggers happen. |
| Vendor distance (`Lap Distance Corrected`, `Distance_Wspd_App`, PDS) | 50 Hz | Someone else already ran an odometer+beacon fusion; when present it is a strong input and today's normalizer rightly prefers it. | It is *driven distance*, so it inherits the line problem (§1.1); different vendors correct differently; cross-car comparability is not guaranteed. |

The frequency-domain summary — the whole game in one sentence: **odometry
(wheels) is precise at high frequency and drifts at low frequency; GPS is
unbiased at low frequency and noisy-to-absent at high frequency; landmarks
(beacons, damper bumps, curvature) are sparse zero-drift pins.** They are
exactly complementary, which is why fusion is not optional.

### 2.3 The video clock is its own coordinate system

> Consumer-facing recipe: [`VIDEO_SYNC.md`](VIDEO_SYNC.md).

Telemetry time, media time, and presentation time are three different axes:

- Each AiM MP4 carries an edit list (`elst`) shifting media onto the movie
  timeline. Measured: **101.333 ms** on `…FP1_Run01_MB.MP4`, **104 ms** on the
  test fixture — *it differs per file*. Any consumer that maps telemetry time
  to frames while ignoring `video_presentation_offset_ns` is wrong by ~3
  frames, by a *different* amount per video. That is precisely the signature
  "sync issues with variance between videos".
- Frame times are not a constant-rate assumption: `video_frame_times_ns()`
  (`crates/aim-telemetry/src/lib.rs:373`) decodes the real `stts`/`ctts`
  tables, and `.telemetry` persists them verbatim as `video_frames.bin`.
- VBOX rolls span multiple files (`avifileindex`/`avisynctime`), each with its
  own offset (`VideoFileRef`, `crates/telemetry-core/src/metadata.rs:80`).
- Some videos have **no telemetry track at all** (all of `D3_Race/*.mp4`:
  `NoAimdTrack`). For those, sync can only come from outside — a sidecar
  aligned by clock or by content (§5.3).

### 2.4 Where the current sidecar path loses the plot (measured)

The report that "sync issues came back when we started extracting to
sidecars" is reproducible in this repo today, and it is not one bug but a
stack of three:

1. **The JSONL interchange silently drops the channels positioning needs.**
   MTJ is strictly lattice-aligned; `collect_aligned`
   (`crates/telemetry-format/src/jsonl.rs:880`) rejects any channel whose
   samples stray > 2 ms (`ALIGN_JITTER_NS`) from its chunk grid — and any
   all-NaN channel. Measured: converting `…FP1_Run01_MB.MP4` keeps **13 of 54
   channels** (voltage + the garbage GPS quality set; lat/lon dropped for
   being all-NaN — correct, if brutal). Converting the *quali* file — the one
   with working GPS — keeps **3 of 54**: battery voltages only. GPS
   (2 chunks after the dropout gaps), wheel speed, steering, and the 100 Hz
   lap timer all fail the alignment gate. The native `.telemetry` container
   keeps all of them (event channels get a `.time.bin` timestamp column);
   the JSONL path starves any downstream consumer of every sync-critical
   channel and leaves it to re-derive position from whatever survived.
2. **The sidecar join key is a clock we know can be six years wrong.** MTX
   attachment is `host_file = ext_file + ext.utc − host.utc` (JSONL.md §11.3,
   `join_shift_ns`). That is exact *if and only if* both `utc` values were
   derived by the same logic from the same source. The moment a sidecar
   writer "fixes" the 2020-vs-2026 GPS week bug (or falls back to file
   mtime, as the metadata CLI does for `event_date`) while the host header
   still carries the raw GPS clock, the join silently shifts by the
   correction. Rule that must be written down and enforced: **a sidecar
   derived from a host copies the host's `utc` bit-for-bit** (and should
   carry the host's `hash` for verification); wall-clock truth is a display
   concern, never a join concern.
3. **Per-video presentation offsets must ride along.** A sidecar or
   downstream tool that stores "video time" without the per-file
   `presentation_offset_ns` reintroduces the per-video ±100 ms skew of §2.3
   even when the nanosecond join is perfect.

None of these are positioning algorithms — they are bookkeeping. But they set
the noise floor: there is no point fusing sensors to 1 m (~14 ms) while the
clock chain can drop 100 ms per video. **Fix the clock chain first.**

---

## 3. What exists in the code today

An honest inventory (August 2026), so the architecture in §5 is a diff, not a
dream.

### 3.1 Already built and load-bearing

- **Role inference & normalization** — `infer_roles`
  (`crates/motorsport-telemetry/src/lib.rs:762`) maps vendor names to roles
  (speed, steering, lat/lon, lap distance…); units convert only when honest
  (`UnitSource`).
- **Lap recovery** — `read_source_metadata`
  (`crates/telemetry-core/src/metadata.rs:427`): authoritative laps (native
  catalog, LDX) → real counters → timer/progress resets → nothing. Works with
  zero GPS (the FP1 files get 14 clean laps from `Lap_Number_001`).
- **Lap progress, three-step fallback** — `normalize_sample`
  (`crates/motorsport-telemetry/src/lib.rs:534`):
  1. vendor lap-distance channel via `normalize_lap_distance` (`lib.rs:928`)
     — `%` and `ratio` pass through; meters need a matched track length;
  2. GPS → `TrackContext::progress` (`lib.rs:633`) — nearest-segment
     projection onto the atlas centerline;
  3. time-through-lap `(t − start) / duration` (`lib.rs:576`).
- **Track atlas** — `motorsport-track-atlas`: offline circuits with
  centerline GeoJSON, lengths, corner/start-finish point layers, sector range
  layers. Matching is nearest-facility to an averaged GPS point
  (`match_track`, facade `lib.rs:403`).
- **The video clock, persisted** — format v5 stores `video_frames.bin`
  (presentation-order PTS per frame), per-recording and per-file
  `presentation_offset_ns`, BLAKE3 of each video, and per-lap
  `first_video_frame` (`LapMetadata`, `metadata.rs:20`; stamped by
  `stamp_lap_video_frames`, `metadata.rs:719`). Round-trip is bit-exact
  (`tests/facade.rs:98`). **So yes: `time → video frame` indexing exists and
  survives the native format.** What does not exist yet is the
  `progress → time/frame` index (§6.2).
- **Sidecar join semantics** — MTX joins on integer ns via header `utc`
  (JSONL.md §11); deliberate, simple, and correct *given* correct `utc`.
- **The pass registry** — `crates/telemetry-passes`: named, versioned,
  lossless processing passes with machine-checked preconditions and persisted
  provenance (§5.5). Shipped: `gps.quality@1`, `gps.clean@1`,
  `speed.distance@1`, applied by default in `telemetry-convert`. Provenance
  and origin identity survive `.telemetry` (format v6) and MTJ
  (`passes`/`src`/`srcp` header keys) round trips;
  `telemetry-convert --strip-passes` recovers the raw conversion
  byte-for-byte (proven by test and on real PDS/MP4 files).

### 3.2 Gaps that matter for positioning

- **GPS quality is now read, but only by the pass layer.** `gps.quality@1`
  grades every fix (sentinel-aware, §2.1 lesson 2) into `GPS Fix Valid` +
  `GPS Position Sigma`, and `gps.clean@1` publishes NaN-masked coordinates
  that role inference prefers when present. Remaining gap: the *progress*
  math (§4) that should consume the sigma does not exist yet.
- **Projection is memoryless and global.** `TrackContext::progress` tests
  *every* centerline segment per sample, takes the nearest, and clamps. No
  continuity between consecutive samples, no hysteresis at the start/finish
  wrap, no protection against locking onto the *other* side of the circuit
  where the track folds back near itself (Road America T5 under the bridge,
  any pit straight parallel to the front straight). A 5 m GPS sample near a
  pinch point can teleport progress by half a lap for one sample.
- **No filtering, no fusion, no distance integration.** Grep confirms: no
  Kalman, no smoothing, no spline fitting, no odometry anywhere in the
  workspace. The three-step fallback is *selection*, not fusion — GPS good
  enough → use it raw; else time-ratio, which is uniform-speed fiction.
- **Atlas matching always takes `layouts.first()`** — layout selection
  (club vs full course) is unimplemented.
- **The MTJ alignment gate** drops sync-critical channels (§2.4.1).
- **Coordinate decode is still not centralized.** The facade and
  `omatrack-folder-scan` now share the arc-minute longitude sign fix
  (`normalize_longitude`, west-positive → east-positive), but VBOX
  packed-coordinate decode still lives only in `main.rs`. Until decode-level
  normalization moves into the VBOX source itself, the GPS passes
  deliberately *skip* arc-minute files rather than grade garbage — their
  `check()` says exactly that.

---

## 4. The estimation problem, stated properly

Fusing §2's sensors is a classic along-track state estimation problem, with
one twist that makes it *easier* than robotics: **we are offline.** Files are
complete before we look at them. We never need a causal filter with lag — we
can run a forward-backward smoother and let evidence from the future repair
the past. A 10 s GPS gap is interpolated from both ends; a wheel-speed
misinterpretation before a good anchor is corrected *backwards*, which is
exactly the "if GPS says we're outside the implied accuracy, we got something
wrong earlier" instinct.

**State** (per instant, continuous):

```
s      along-track position on the reference line, in meters, unwrapped
       (never mod L inside the estimator — wrap only on output)
v_s    along-track velocity
k      odometer scale (tire-radius / line-length factor), random walk,
       expected ≈ 1.00 ± few %
b_xy   slowly-varying GPS bias (correlated multipath), optional
```

**Process model:** `ds = k · v_wheel · dt` (+ noise that grows under braking
and traction, where slip lives — gate the noise on brake pressure and
longitudinal g, which we record). `k` random-walks slowly (tire wear over a
stint) and re-initializes on pit stops.

**Measurement updates**, each with honest noise and χ² innovation gating:

| Measurement | Model | Noise / gating |
|---|---|---|
| GPS position | project (lat,lon) onto reference → `s_gps` (+ lateral offset, discarded or used as lane info) | σ from `GPS Position Accuracy` after sentinel filtering; reject fix type < 3, sats < 5, DOP high, and any innovation > gate. The projection itself must be *local*: search a window around predicted `s`, never globally (kills the fold-back teleport). |
| Lap beacon / timer reset | `s ≡ s_beacon (mod L)` | few-ms timing → sub-meter at speed; the strongest anchor we have; survives zero-GPS files. |
| Curvature match | `κ_measured(t) = a_lat/v² ` vs `κ_ref(s)` | continuous map-matching term; informative in corners, mute on straights; robust-weighted because lines vary. |
| Damper landmark | bump signature at `s_landmark` | cross-correlation peak of the damper trace against a per-track landmark library → sub-meter pins, sparse (§5.3). |
| Vendor `Lap Distance Corrected` | `ds/dt` pseudo-odometer or direct `s` after per-lap affine fit | trust as odometry, not as identity — it's driven distance. |

**Smoother:** RTS (Rauch–Tung–Striebel) or an equivalent factor-graph batch
solve per session. Cheap: the state is ~4-dimensional at 25–100 Hz.

**Monotonicity & spins:** the prior `v_s ≥ 0` is encoded in the process
model. Reverse requires agreement of reverse gear / near-zero wheel speed +
yaw evidence. A spin (yaw rate wildly inconsistent with `κ_ref(s)·v`, speed
collapse) switches the model to "position ≈ frozen, large uncertainty" until
rolling resumes — progress flat-lines with widening error bars instead of
jittering backwards. Off-track excursions show up as GPS lateral offset
beyond track width: same treatment.

**Pit lane** is a second spline with its own gates, sharing the unwrapped
axis. Mode (track vs pit) is a discrete hypothesis resolved by lateral
offset likelihood + speed profile; progress through the pit maps onto the main
axis so a stationary car in the box still has a defined, flat `p`.

**Output channels** (this is the abstraction the user of the data sees):

```
Lap Progress        f64, unit "ratio", 0 ≤ p < 1, wrap-exact at gates
Lap Progress Sigma  f64, meters along-track (honest error bars)
Lap Progress Source u8 enum: 0 none/time-ratio, 1 odo-only, 2 odo+landmarks,
                    3 odo+gps, 4 gps-primary, 5 vendor-distance   (step interp)
```

Sigma is not decoration: it is what makes "sometimes GPS is just good enough"
a *measured* statement per time region instead of a hope per file, and it
tells any comparison UI how wide to draw the alignment tolerance.

---

## 5. The strategy ladder

Different files earn different algorithms. Detection is per *time region*
(the FP1 lesson: capability can be zero despite the channel list), and every
strategy emits the same three channels so consumers never branch.

### S1 — GPS-primary (good fix, matched track)

When accuracy is honestly ≤ ~2 m with few gaps: locally-windowed projection
onto the reference spline + light smoothing may hit the target with no fusion.
Verify against gate-crossing residuals before trusting (§7); promote to S2
automatically when it doesn't hold. (Our best file — median 5.1 m, 91 gaps —
does **not** qualify. Expect S1 to be rare with camera-GPS, common with
proper RTK loggers.)

### S2 — Fused odometry (the AiM-video case: wheel speed + patchy GPS + beacons)

The full §4 smoother. Wheel speed is the backbone (10 Hz here — interpolation
between samples rides the 100 Hz lap timer and video clock), GPS anchors the
scale `k` and kills drift where fix quality allows, beacons pin the wrap.
This is the workhorse for `26R05_RAM`-style data.

### S3 — No GPS at all (PDS, dead cameras): landmark registration

The FP1 files and most PDS exports. Odometry + beacons alone give a
*self-consistent* but floating `s`: scale `k` is unobservable except through
lap-length closure (each beacon-to-beacon integral must equal one lap —
that alone pins average `k` per lap to ~0.1 %). What's missing is *shape*:
where on the track each meter sits. Two registration sources fix that:

- **Damper landmarks** — the manual trick, formalized. Build, per track, a
  library of bump/curb signatures: short windows of damper velocity,
  *resampled onto the distance axis* (divide out speed — in distance domain a
  bump has the same shape at any speed), stored at known `s` from
  GPS-equipped reference sessions. Registration = normalized
  cross-correlation of the session's damper trace against the library along
  the odometry axis; each confident peak is a `s_landmark` measurement in the
  §4 smoother. Sub-meter local accuracy near corners — which is where the
  landmarks naturally are, because bumps live at braking zones and curbs.
  **Critical rule: landmarks must be track-fixed features (bumps, curbs,
  surface joints), never driver-dependent ones (braking points, lift
  points).** Aligning on braking points erases exactly the differences the
  comparison exists to show.
- **Curvature registration** — steering/lat-g curvature vs `κ_ref(s)`
  (dynamic-programming / DTW alignment of the corner sequence), which needs
  no per-track library at all, only the atlas centerline. Coarser than
  dampers (~meters) but dense and free.

S3's honest output: sigma of a few meters near landmarks/corners, growing on
long straights between them. That's real, and the sigma channel says so.

### S4 — Nothing usable

No laps: no progress (channel absent, not fabricated). Laps but no
speed/GPS/dampers: today's time-through-lap ratio, `Source = 0`, sigma huge.
Never let S4 output masquerade as comparable data — this is what the source
channel is for.

### Cross-session transfer

S3 depends on references that some *other* session provides: GPS-equipped
sessions continuously improve the per-track assets (centerline refinement
from fleet GPS clouds, damper landmark library, beacon-gate positions,
`κ_ref`). Architecturally that means positioning has a **read-write
relationship with the track atlas**, not read-only: sessions consume the
reference and the good ones amend it (§6.3).

### 5.5 Shipped: strategies as named, versioned, lossless passes

The ladder is not implemented as one monolithic estimator but as **passes** —
small, named, versioned transforms in `crates/telemetry-passes`, run at
conversion time and recorded in the file. Making the strategies *distinct,
named things* is itself the product requirement: each pass documents what
has to be true for us to employ it, the converter checks those preconditions
mechanically, and the resulting telemetry says which passes ran — so the
software can show *why* this file has (or lacks) a derived channel, given the
nature of the source.

The contract, enforced by tests:

- **Lossless, append-only.** A pass never mutates or removes source channels;
  it only appends derived ones. `telemetry-convert --strip-passes` recovers
  the raw conversion **byte-for-byte**.
- **Named + versioned.** `gps.clean@1`. Any change to output values bumps the
  version. Re-running a recorded pass at the same version skips
  ("already applied"); a different version is a hard error (strip first) —
  never a silent mix of algorithm generations in one file.
- **Preconditions are prose and code.** `requirements()` is the sentence a
  user reads; `check()` is the machine test. Every skip carries its reason:
  "no GPS coordinate channels present", "coordinates are arc-minutes —
  decode-level normalization required first".
- **Deterministic.** No clocks, no randomness; parameters are recorded in
  provenance, so the same source always yields the same bytes.
- **Honest uncertainty.** Estimates ship with a sigma channel
  (`GPS Position Sigma`, `Distance Odometer Sigma`) instead of false
  precision — §1.2's *Total* requirement, made concrete.
- **Provenance travels.** `AppliedPass {name, version, params, inputs,
  outputs}` lives in the `.telemetry` v6 catalog and the MTJ `passes` header
  key, alongside the original `source_format`/`source_path` (`src`/`srcp`) —
  a file converted and rewritten three times still knows it began life as
  `…_Run02_TL.MP4`, and the UI can show both.

Implemented today:

| Pass | Employ when (checked by `check()`) | Appends |
|---|---|---|
| `gps.quality@1` | GPS lat/lon present, non-empty, decimal degrees | `GPS Fix Valid` (0/1), `GPS Position Sigma` (m) — sentinel-aware (§2.1), fix-type/satellite gated; sigma from receiver accuracy, else HDOP·UERE, else 15 m |
| `gps.clean@1` | same, downstream of `gps.quality` | `GPS Latitude Clean`, `GPS Longitude Clean` (deg) — invalid fixes NaN-masked, >150 m/s teleports rejected (re-anchor after 8); role inference prefers these over raw |
| `speed.distance@1` | a non-empty speed channel with a declared unit convertible to m/s | `Distance Odometer` (m), `Distance Odometer Sigma` (m) — trapezoidal ∫v dt; 0.5 %-of-distance drift model plus a penalty per sampling gap >1 s |

Planned (names reserved in `PLANNED`, requirements written):

| Pass | Ladder rung | Employ when |
|---|---|---|
| `progress.project` | S1 | clean GPS + matched track + continuity-windowed projection is enough |
| `progress.fuse` | S2 | odometer exists + any anchor source (GPS regions, beacons) |
| `landmark.damper` | S3 | damper channels + a per-track landmark library |
| `progress.time` | S4 | laps only — the explicit lowest rung, sigma huge |

`telemetry-convert` applies the registry by default and reports one line per
pass on stderr (`gps.quality@1 applied → GPS Fix Valid, GPS Position Sigma`;
`gps.clean@1 skipped — no GPS coordinate channels present`);
`--no-passes` converts raw, `--strip-passes` removes recorded outputs.
`motorsport-telemetry <file>` prints `source_format`, `source_path`, and
`passes` for any file.

---

## 6. Architecture

Three layers, one new crate.

### 6.1 `telemetry-passes` (exists) grows the progress passes

The pass crate (§5.5) is the home. `progress.project`, `progress.fuse`,
`landmark.damper`, and `progress.time` are already registered as planned
passes with their requirements written down; implementing them means each
consumes a `TelemetrySource` + a `TrackReference`, runs capability detection
→ strategy ladder → smoother, and appends the three output channels plus a
**gate-crossing table**. Pure, deterministic, versioned — exactly the
existing pass contract, so provenance, skip reasons, and byte-exact strip
come for free. Lives below the facade so the normalizer's `lap_progress`
becomes: *read the computed channel if present; compute on-the-fly (cheap
S1/S4 approximation) if not.* The existing three-step fallback remains as
the zero-dependency path.

### 6.2 Persistence: the missing index is `progress → time → frame`

`.telemetry` already has `time → frame` (§3.1), and format v6 (shipped)
records pass provenance and preserves the original `source_format` /
`source_path` across rewrites. Add, in format v7:

- The three progress channels as ordinary stored channels (they compress
  well; delta-encoded progress is nearly linear).
- **Gate table**: for a configurable gate spacing (e.g. every 0.001 of the
  lap, ~6.5 m at Road America), per lap: `(gate_index → crossing_time_ns)`,
  monotone by construction, plus `first_video_frame` derivable via the
  existing video clock. This is the O(1) answer to the product question
  "show me both cars at gate 0.59693 / at Turn 5": two table lookups, two
  frame indexes, zero scanning. It is also the natural unit of validation
  (§7) and of cross-video alignment — two videos are "in sync" at a gate iff
  their crossing timestamps agree with their lap-time difference.
- `TrackReference` identity: `{track_slug, layout, reference_version,
  reference_hash}` in the catalog. Progress without this tuple is
  uninterpretable; comparisons across differing references must be refused
  loudly.
- Per AGENTS.md: bump `FORMAT_VERSION`, add a `v6_to_v7` migration
  (progress/gates absent after migration — recompute from source, never
  invent; the shipped `v5_to_v6` migration already follows this rule for
  pass provenance).

MTX sidecars carry the same channels for hosts we can't rewrite (the
`.telemetry` next to a `.pds`, or a plain race video with no aimd track,
which gets *only* a sidecar). Sidecar rule from §2.4: copy host `utc`
verbatim + host hash.

### 6.3 `motorsport-track-atlas` grows a reference model

Today: matched centerline GeoJSON. Needed: arc-length parameterized spline
with `κ_ref(s)`, surveyed beacon-gate positions, pit spline, damper landmark
library, and **versioning** (`sebring@3`). Fleet refinement (averaging many
sessions' fused trajectories into a better centerline) is an offline tool
(`scripts/`), not a runtime behavior — runtime only ever reads a pinned
version. Fix `layouts.first()` while in there: select layout by GPS cloud
coverage, not by index.

### 6.4 Clock-chain hardening (do this first, it's cheap)

1. Sidecar writers copy host `utc` + record host `hash`; attach verifies.
2. Every consumer that touches frames goes through
   `video_presentation_time_ns` / `video_frame_at` — audit for raw
   `time / frame_rate` arithmetic anywhere downstream.
3. MTJ: either emit event channels with explicit `t0` re-chunking (per-chunk
   records are already representable — one channel record per aligned chunk
   region), or at minimum *warn loudly* listing dropped channels. Silent
   3-of-54 is how sync bugs get blamed on GPS.
4. ~~Facade VBOX longitude sign bug~~ **Done** — `normalize_longitude` in
   the facade and `omatrack-folder-scan` flips the west-positive arc-minute
   convention to east-positive, matching `main.rs`.

---

## 7. Validation: how we know it works

Positioning bugs are quiet; the harness must be loud.

- **Gate residuals (self):** re-predict each gate crossing with each sensor
  family withheld (leave-one-out). GPS-withheld vs GPS-present crossing
  times quantify what odometry+landmarks are really worth per track region.
- **Beacon truth:** predicted `s` at beacon fire must equal the beacon gate;
  drift of the residual across a stint is the `k`-estimation report card.
  100 Hz `Current_Lap_Time` gives the same check between beacons at every
  timer tick.
- **Cross-video consistency:** two cars' gate tables + their lap-time deltas
  from official timing must agree; disagreement localizes which video's
  chain is off (and by how many frames).
- **Video ground truth (the ultimate arbiter):** at a gate with a visual
  landmark (start/finish gantry, bridge, curb), the indexed frame must show
  the car at the landmark. A dozen hand-labeled (video, frame, gate) triples
  per track make a regression suite; format v5's frame indexing makes the
  check mechanical.
- **Monotonicity & wrap audits:** `p` never decreases without a flagged
  reversal event; unwrapped `s` at lap boundaries advances exactly `L`;
  fixtures for the spin case (progress flat, sigma wide, no backwards
  jitter).
- **Determinism:** same file + same reference version + same `ALGO_VERSION`
  ⇒ bit-identical channels, enforced by fixture hashes like the existing
  round-trip tests.

---

## 8. Roadmap

| Phase | Deliverable | Unblocks |
|---|---|---|
| **0. Clock chain** (§6.4) | Sidecar `utc` copy rule + hash check; MTJ dropped-channel warning or event-channel support; ~~VBOX facade sign fix~~ (done); frame-math audit | Kills the current sync variance without any new math |
| **1. GPS honesty** | **Done as passes** (§5.5): `gps.quality@1` (sentinels, fix/accuracy/DOP gating, sigma) + `gps.clean@1` (masked, teleport-free coordinates preferred by role inference) + `speed.distance@1` odometer, with provenance in format v6 and MTJ. Still open: windowed *local* projection with continuity + wrap hysteresis replacing the global nearest-segment scan in `TrackContext::progress` | Immediately better S1; channel-level teleports gone |
| **2. Odometry + smoother** | `progress.fuse` + `progress.time` passes: wheel-speed process model, beacon anchors, GPS updates, RTS smoother, three output channels | The workhorse; FP1-style zero-GPS files get honest odo+beacon progress |
| **3. Persistence** | Format v7: progress channels, gate table, reference identity; MTX equivalents (v6 shipped: pass provenance + origin identity) | O(1) `progress → frame`; cross-video alignment product features |
| **4. Landmarks** | Distance-domain damper signature library + curvature DTW; atlas versioning + reference model | S3: PDS files and dead-GPS videos reach corner-level comparability |
| **5. Fleet refinement** | Offline centerline/landmark refinement from accumulated fused sessions; layout selection fix | Reference quality compounds with every session ingested |

---

## Appendix A — Field notes from the current data

- AiM camera GPS (25 Hz, u-blox NAV-SOL in `GPS0` aggregates) can spend
  entire sessions at fix type 0 while every other subsystem works; the 100 Hz
  lap timer and 10 Hz CAN set are the reliable spine of those files.
- The GPS week/date can be years stale (week 2117 in 2026); `placement.rs`
  policy of only trusting `gps`/`utc` clocks still needs the sanity check
  against file dates that `main.rs` applies to `event_date` — that check
  belongs in placement, not per-consumer.
- `GPS Position Accuracy = 4294967.29` and `DOP = 99.99` are sentinels, not
  measurements.
- Edit-list presentation offsets measured so far: 101.333 ms and 104 ms —
  same camera family, different files. Never assume a constant.
- `telemetry-convert` → MTJ on a real session keeps 3/54 channels
  (`jsonl.rs` alignment gate). Native `.telemetry` keeps 54/54.
- PDS exports in `sebring-2026` carry `Lap Distance Corrected`,
  `Distance_Wspd_App`, 50 Hz FIA GPS (when the export includes it), `STEER`,
  `X_FL_DAMPER` — i.e. everything S2/S3 need, at better rates than the
  videos. The `.telemetry` sidecars already being written next to them are
  the natural home for computed progress.
