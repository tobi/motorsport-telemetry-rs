# Motorsport Telemetry JSONL (MTJ)

Interchange format for a time-aligned motorsport recording.

This document is the standard. A file is an MTJ document if and only if it
satisfies every MUST below. The native `.telemetry` zip remains the lossless
archive; MTJ is the compact, line-oriented, inspectable form.

| | |
|---|---|
| Identifier | `mtj` |
| Current version | `1` |
| Media type | `application/vnd.motorsport-telemetry+jsonl` |
| Preferred names | `.telemetry.jsonl`, `.telemetry.jsonl.zstd` |
| Extension names | `.telemetry.ext.jsonl`, `.telemetry.ext.jsonl.zstd` |
| Also accepted | `.jsonl`, `.mtj`, `.ext.jsonl`, `.mtx.jsonl`, and those names with `.zstd` / `.zst` |
| Encoding | UTF-8, no BOM. A zstd frame (`28 B5 2F FD`) MAY wrap the UTF-8 document. |
| Newline | `LF` (`U+000A`). Readers MUST also accept `CRLF`. |

## 1. Design

JSONL cannot store raw sample columns. Compactness therefore comes from **not
storing time**. Every sample sits on a single integer-nanosecond lattice. The
time of sample `i` of a channel is determined only by that channel's `t0`,
`hz`, and `i`. Two samples with the same computed timestamp are contemporaneous.

A file that needs per-sample timestamps is not an MTJ file. Event or jittered
streams MUST be snapped onto the lattice, resampled, or omitted by the writer.
They MUST NOT be encoded as `[t, v]` pairs.

## 2. Document shape

A document is a sequence of JSON values, one value per line, in this order:

1. Exactly one **header** object.
2. Exactly one **laps** array (empty if the recording has no laps).
3. Zero or more **channel** objects, one channel per line.

No blank lines. No JSON insignificant whitespace (space, tab, CR inside a
line). A single trailing `LF` after the last record is REQUIRED.

Unknown object keys on the header or a channel MUST be ignored. Unknown
positions in a lap tuple beyond the fields defined here MUST be ignored.
A first line that is not an object containing `"mtj": 1` is not an MTJ
recording. An object containing `"mtx": 1` is an **extension** (see §11),
not a recording.

```jsonl
{"mtj":1,"q":10000000,"dur":40000000,"src":"pds","drv":"Tobi","ven":"Road America"}
[[1,0,40000000,0]]
{"n":"Speed","hz":100,"u":"km/h","v":[10,11,12,13]}
{"n":"GPS Speed","hz":25,"u":"m/s","v":[2.8]}
```

## 3. Time

The **primary key is integer nanoseconds**. Every time in this format —
lap `start`/`end`, sample `t[i]`, span `s`/`e`, `t0`, `dur` — is that
integer. There is no other key.

There is **one** sample timeline per file: **file-relative** nanoseconds.
`t = 0` is the start of that file. These integers are not civil datetimes.
They are not local Sebring time, not Eastern, not DST, not ISO-8601.

To place that axis on the real world, the header stamps `utc`: Unix-epoch
nanoseconds (UTC) at file `t = 0`.

```
utc_epoch_ns = file_relative_ns + utc
```

A sidecar MUST carry `utc` so it can be lined up with a host. Join is only
this subtraction (§11.3). `tz` is the IANA venue zone
(`America/New_York`, not `EDT`, not `-04:00`). It formats a paddock wall
clock from `utc_epoch_ns`. It is **never** a join key.

`date` and `time` in the header are decorative source text. They are not
the clock. Optional `clk` + `abs` is leftover vendor-scale metadata
(`gps`, Motec civil-as-`utc`, `time_of_day`). It is not the primary key
and MUST NOT be used to join. `time_of_day` wraps at midnight and MUST NOT
be used as `utc`.

## 3.1 Time lattice (normative)

Let `q` be the header field `q` and `o` the header field `o` (default `0`).
Both are integers in nanoseconds. `q` MUST be greater than zero.

The **lattice** is the set

```
{ o + k·q | k ∈ ℕ₀ }
```

Every timestamp in the file — channel `t0`, every implied sample time, every
lap `start` and `end`, and `dur` — MUST be a lattice point.

`dur` is the exclusive end of the file-relative timeline. It MUST satisfy
`dur ≥ o` and `dur ≡ o (mod q)`.

### 3.2 Channel time

For a channel with frequency `hz` and optional start `t0` (default `o`):

```
period_ns(hz) =
    1_000_000_000 / hz     if hz is a positive integer that divides 1e9
    round(1e9 / hz)        otherwise

t[i] = t0 + i · period_ns(hz)     for i = 0 .. len(v)-1
```

`hz` MUST be a finite JSON number greater than zero such that `period_ns(hz)`
is a positive integer. `period_ns` MUST be a multiple of `q`. `t0` MUST be a
lattice point and MUST satisfy `t0 ≥ o`.

The last sample occupies `t[n-1]`. The channel's exclusive end is
`t0 + n · period_ns`. That end MAY exceed `dur` by less than one period; it
MUST NOT exceed `dur + period_ns`.

`null` in `v` is a missing value at that lattice instant, not a hole in time.
Index `i` always refers to `t[i]`.

### 3.3 Alignment across channels

Two samples (including two samples of different rates) are **aligned** when
their computed `t[i]` values are equal. Readers MUST treat equal timestamps as
the same instant. Readers MUST NOT apply any other time offset, phase, or
interpolation in order to interpret the file.

Channels MAY use different `hz` values. The lattice is what makes a 100 Hz
sample and a 25 Hz sample comparable: 25 Hz is 40 ms, 100 Hz is 10 ms, and
`q` is their GCD (here 10 ms), so every 25 Hz sample coincides with a 100 Hz
sample.

### 3.4 Writer duty

A writer producing MTJ from a native recording:

1. Drops channels with no samples.
2. Accepts a channel only when every native sample time is within
   `min(2_000_000 ns, period_ns / 2)` of `chunk.time_base + i · period`.
   Channels that fail this test are unaligned and MUST be omitted (or first
   resampled onto a regular lattice by the writer).
3. Emits one contiguous value array from the first sample to the last, with
   `null` at any in-between lattice point that has no native sample.
4. Computes `q` as the GCD of every emitted `period_ns` and every
   `t0 − o`. If no channel remains, `q` MUST still be a positive integer
   (1 000 000 ns is the recommended default).
5. Snaps each lap boundary to the nearest lattice point. If snapping would
   make `end ≤ start`, `end` becomes `start + q`.
6. Sets `dur` to the least lattice point that is at least the longest channel
   end and at least every snapped lap end.

## 4. Header

Line 1 is a JSON object. Writers SHOULD emit keys in the order listed.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `mtj` | integer | yes | Spec version. This version is `1`. Other values are a different document. |
| `q` | integer ≥ 1 | yes | Lattice quantum, nanoseconds. |
| `dur` | integer ≥ 0 | yes | Exclusive file-relative duration, nanoseconds. |
| `o` | integer ≥ 0 | no | Lattice origin, nanoseconds. Default `0`. |
| `src` | string | no | Format of the original recording: `aimd`, `pds`, `motec`, `vbo`, or `telemetry`. Carried through rewrites: a `.telemetry` -> MTJ hop keeps the vendor id. |
| `srcp` | string | no | Path of the original recording as seen at first conversion. Carried through rewrites like `src`. |
| `drv` | string | no | Driver name. 1–80. |
| `veh` | string | no | Vehicle name or identifier. 1–80. |
| `ven` | string | no | Venue / circuit. 1–80. |
| `evt` | string | no | Event name. 1–80. |
| `ses` | string | no | Session name. 1–80. |
| `date` | string | no | Source date string, unchanged. 1–32. |
| `time` | string | no | Source time string, unchanged. 1–32. |
| `utc` | integer | yes on write when known | Unix-epoch nanoseconds (UTC) at file `t = 0`. |
| `tz` | string | yes on write when known | IANA venue timezone, e.g. `America/New_York`. |
| `clk` | string | no | Source clock name, e.g. `gps` or `utc`. Not a timezone. |
| `abs` | integer | no | Source-clock reading at file `t = 0`. |
| `abe` | integer | no | Source-clock reading at file `dur`. |
| `hint` | string | no | Session-hint component used by the native catalog. |
| `vo` | integer | no | Recording-level video presentation offset, nanoseconds: `player_ns = t + vo`. See 4.2. |
| `vf` | array | no | Linked video files, in index order. See 4.2. |
| `vpts` | array | no | Presentation-order video frame timestamps, nanoseconds on the movie timeline. Requires `vf`. See 4.2. |
| `passes` | array | no | Processing passes applied to this file, in application order. See 4.1. |
| `hash` | string | no | 16-digit lowercase hex schema hash. |

Omit any optional key whose value is empty, unknown, or `0` for `o`. Do not
write `null` values. Writers MUST emit `utc` and `tz` whenever they are
known. Readers of a recording MAY accept a document that lacks them (that
file cannot be placed on the absolute axis).

`clk` without `abs` is ignored. `abs` is leftover vendor-clock metadata,
not the join key. `utc` is the primary key.

`mtj`, `q`, `dur`, `o`, `utc`, `abs`, `abe`, `vo`, and every `vpts` entry
MUST be JSON integers, not quoted strings and not non-integral numbers.

### 4.1 Pass provenance (`passes`)

Every entry records one lossless processing pass that appended derived
channels to this file. Passes never modify or remove source channels;
dropping every channel named in an entry's `out` list recovers the raw
conversion exactly.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `n` | string | yes | Pass name, e.g. `gps.clean`. |
| `v` | integer | yes | Pass algorithm version. |
| `p` | object | no | Parameters as string values, e.g. `{"max_speed_mps":"150"}`. |
| `in` | array of strings | no | Names of the channels the pass read. |
| `out` | array of strings | no | Names of the channels the pass appended. |

```json
{"passes":[{"n":"gps.clean","v":1,"p":{"max_speed_mps":"150","reanchor_after":"8"},
  "in":["GPS Latitude","GPS Longitude","GPS Fix Valid"],
  "out":["GPS Latitude Clean","GPS Longitude Clean"]}]}
```

Readers that do not understand a pass name MUST still treat its `out`
channels as ordinary channels; the entry only explains where they came from.

### 4.2 Video linkage (`vo`, `vf`, `vpts`)

A recording converted from a camera container — or from a `.telemetry` file
that carried the linkage — keeps its video synchronization. The pixels stay
in the original video file; the header stores the mapping onto it. Two
timelines are involved (consumer recipe: `docs/VIDEO_SYNC.md`): telemetry
time `t` (file-relative nanoseconds) and the player's presentation timeline.

- `vo` — recording-level presentation offset, nanoseconds:
  `player_ns = t + vo`.
- `vf` — the linked video files, in index order. Each entry:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `n` | string | yes | Video filename (basename; resolve next to this document). |
| `i` | integer ≥ 1 | yes | File index; multi-file rolls count up from 1. |
| `fc` | integer ≥ 0 | yes | Frame count, `0` when unknown. |
| `b3` | string | no | BLAKE3-256 of the video file, 64 hex digits, when it was present at convert time. Verify before trusting frame-accurate sync. |
| `po` | integer | no | Per-file presentation offset: `video_presentation_ns = file_relative_ns + po`. |

- `vpts` — the presentation-order frame timestamp table: one integer per
  frame, nanoseconds on the movie timeline, non-decreasing. Byte-for-byte
  the same values as the native `video_frames.bin` member. `vpts` without
  `vf` is invalid.

The frame shown at telemetry time `t` is the last index whose `vpts` entry
is `<= t + vo` (clamped to `0`). Never derive frames from a nominal frame
rate; real containers drop frames and carry edit-list shifts. Sidecar
(`mtx`) documents MUST NOT carry any of these keys: video belongs to the
host recording.

```json
{"vo":101833333,
 "vf":[{"n":"1602_Driver02_lap0-15_SCHD0060.MP4","i":1,"fc":7556,
        "b3":"6d10ed8ecbe469d65f411ac5eea30bc34c8df7f4738c02c204cb78cae9578c1d",
        "po":101833333}],
 "vpts":[0,16666666,33333333]}
```

## 5. Laps

Line 2 is a JSON array. Each element is a lap tuple:

```
[number, start_ns, end_ns, complete]
[number, start_ns, end_ns, complete, first_video_frame]
```

| Position | Type | Meaning |
|---|---|---|
| 0 | integer | Lap number as reported or derived. |
| 1 | integer ≥ 0 | Inclusive start, file-relative nanoseconds. |
| 2 | integer ≥ 0 | Exclusive end, file-relative nanoseconds. |
| 3 | `0` or `1` | `1` when both boundaries fall inside the recording. |
| 4 | integer ≥ 0 | Optional presentation-order video frame at `start_ns`. |

`end_ns` MUST be greater than `start_ns`. Both MUST be lattice points.
`duration_ns` is `end_ns - start_ns` and MUST NOT be stored separately.
Laps MUST be in non-decreasing `start_ns` order.

Writers emit `complete` as the integers `0` and `1`. Readers MUST accept those
integers and MAY accept JSON `true` / `false`.

An empty recording writes `[]`.

## 6. Channels

Each remaining line is one object:

| Key | Type | Required | Meaning |
|---|---|---|---|
| `n` | string | yes | Channel name. Unique in the file. 1–64 Unicode code points. |
| `hz` | number > 0 | yes | Sample rate in hertz. See §3.1. |
| `u` | string | no | Unit as declared by the source. 1–24. Omit when unknown. |
| `v` | array | yes | Values, oldest first. Each element is a JSON number or `null`. |
| `t0` | integer ≥ 0 | no | First-sample time, file-relative nanoseconds. Default `o`. |
| `vis` | `0` or `1` | yes in MTX | Default visibility. Optional on `mtj` (default `1`). |
| `plt` | string | no | Plot class: `trace` (default), `gauge`, or `compass`. |
| `sc` | `[min, max]` | no | Suggested display scale. Either bound may be `null`. |
| `rnd` | integer 0–15 | no | Decimal places to show. |
| `fmt` | string | no | Rounding / unit format hint. 1–16, no whitespace: `0.0°C`, `000`, `0°`. |
| `lbl` | array | no | Sparse comments on **traces only**. `[[ns,"text"],…]`. |

`v` MUST contain at least one element. `NaN` and infinities are not JSON;
missing or non-finite native samples are `null`.

Writers MUST write an integral value that is exactly representable in IEEE-754
binary64 as a JSON integer (`12`, not `12.0`). Other finite values are JSON
numbers with no insignificant trailing zeros. A value that is a promoted
IEEE-754 binary32 SHOULD be written as that binary32's shortest decimal
(`0.2`, not `0.20000000298023224`). Readers store the parsed JSON number;
bit-identical binary64 is not required. Values are already in engineering
units: no scale or bias is applied by the reader.

`u` is the source unit string. Do not invent a unit from the channel name.
Readers MAY normalize aliases through the shared unit registry; the file
stores what the source declared.

Channel names are case-sensitive. Two channels that differ only in case are
distinct. Writers SHOULD preserve the source spelling.

`hz` SHOULD be a JSON integer when `period_ns` divides one second exactly
(100, 50, 25, 10, …). Otherwise it is the JSON number `1e9 / period_ns`.

### 6.1 Plot class and display (`plt`, `sc`, `rnd`, `fmt`)

`plt` says how to draw the channel. Omit it for a normal overlay **trace**.

| `plt` | Use for | How to treat it |
|---|---|---|
| `trace` (default) | Speed, throttle, brake, steering, RPM, ride height | Y-vs-time strip. May share an axis with other traces of the same unit. |
| `gauge` | Temperature, BPM, SpO2, and other foreign scalars | Own pane or gauge. Do **not** overlay on a speed/throttle strip. |
| `compass` | Wind direction, heading | Compass / rose. Domain wraps at 360°. Do not plot as an unbounded Y axis. |

`sc` is a suggested display range, not a clip and not a unit conversion.
`rnd` is decimal places. `fmt` is a hint of at most 16 characters with
**no whitespace** (`0.0°C`, `000`, `0°`). Viewers MAY ignore both. Do not
invent `sc` / `rnd` / `fmt` from the channel name if the writer omitted them.

Writer guidance for common foreign signals:

| Signal | `plt` | typical `sc` | `rnd` | `fmt` |
|---|---|---|---|---|
| Coolant / tyre / oil temp | `gauge` | e.g. `[60,120]` or `[0,150]` | `1` | `0.0°C` |
| Heart rate (BPM) | `gauge` | `[40,200]` | `0` | `000` |
| SpO2 | `gauge` | `[80,100]` | `0` | `000%` |
| Wind direction | `compass` | `[0,360]` | `0` | `0°` |

### 6.2 Channel labels (`lbl`)

Optional sparse comments. **Allowed only when `plt` is omitted or `trace`.**
A `gauge` or `compass` channel MUST NOT carry `lbl`.

Each element is `[time_ns, text]`. `time_ns` is file-relative, a lattice
point, `≥ o`. Entries MUST be in strictly increasing time order. `text` is
a non-empty string. Omit the key when there are no comments.

A viewer draws a **dot** on this channel's trace at each `time_ns`. On
hover the comment expands to a **dotted vertical** across the full height
of the trace view. Labels are not samples and do not use `v`.

```
{"n":"Speed","hz":100,"u":"km/h","v":[10,11,12],"lbl":[[10000000,"brake lock"]]}
{"n":"Water Temp","hz":1,"u":"°C","plt":"gauge","sc":[60,120],"rnd":1,"fmt":"0.0°C","v":[88.4]}
```

## 7. What this format does not contain

These are deliberate omissions. Recover them from the original vendor file or
from a `.telemetry` zip.

- Per-sample timestamps
- Native integer encodings, scale, and bias
- Video payloads. The linkage — file references, presentation offsets,
  and the frame timestamp table — is in the header (§4.2); the pixels stay
  in the video file
- Driver-stint lists (derive from a driver-id channel when present)
- Irregular / event streams that cannot sit on the lattice

Spans (`k:"s"`) and per-channel `vis` **are** stored in a `.telemetry`
catalog (v5). Converting MTJ ↔ native keeps them. An MTX sidecar file
itself is still JSONL-only.

## 8. Compression

The preferred on-disk names are `.telemetry.jsonl` (plain) and
`.telemetry.jsonl.zstd` (one zstd frame wrapping the UTF-8 document).
`.zst` is an accepted alias for `.zstd`.

This crate's writer compresses by default (zstd level 11). A reader MUST
treat a file as compressed when it begins with the zstd magic `28 B5 2F FD`,
even if the suffix is `.telemetry.jsonl`. The decompressed bytes are then the
MTJ document defined above.

## 9. Conformance

A **reader** is conforming when it:

- Rejects a document whose first record is not `{"mtj":1, ...}` with valid
  `q` and `dur`.
- Rejects a channel whose `period_ns` or `t0` is not a multiple of `q`.
- Rejects duplicate names, empty names, non-positive `hz`, and values other
  than number or `null`.
- Computes sample times only by the formula in §3.1.
- Treats `null` as a missing value at that instant.

A **writer** is conforming when it:

- Emits the three-section order, compact JSON, and `LF` newlines.
- Emits only lattice-aligned channels and snapped lap boundaries.
- Computes `q` as specified in §3.3.
- Does not write insignificant whitespace or a UTF-8 BOM.

## 10. Versioning

`mtj` is the recording-document version, independent of the `.telemetry`
catalog `FORMAT_VERSION`. `mtx` is the extension-document version and is
versioned independently. A new version is required when the section layout,
the lattice rule, or the meaning of a defined key changes. Adding an optional
key is not a new version; v1 readers ignore unknown keys.

## 11. Extensions (MTX)

An extension is extra channels for a recording that already exists. It is
JSONL-only (plain or zstd). It is not a `.telemetry` zip member and it does
not carry laps, identity, or video.

The point is to load it into a host MTJ document and have the new channels
line up on **time**. Time is the primary key. The sidecar MAY cover a slice of
the host, the whole host, or a window that starts later or runs longer.

### 11.1 Minimum document

An MTX document contains one or more groups. Each group is:

1. One **header** object whose identifying key is `mtx`, not `mtj`.
2. Zero or more **records**: sample channels (§6) and/or spans (§12), ending
   at the next `mtx` header or end of file.

Writing another complete `mtx` header starts another folder in the same file.
There is no separate folder record type. Header `n` names that folder and
header `vis` controls whether it starts expanded. Channel names remain unique
across the whole file so every group can attach to one host without ambiguity.

No laps line. No blank lines. Same compact JSON and `LF` rules as a recording.
Same zstd wrapping as §8. Preferred names:

| | |
|---|---|
| Plain | `.telemetry.ext.jsonl` |
| Compressed | `.telemetry.ext.jsonl.zstd` |
| Also accepted | `.ext.jsonl`, `.mtx.jsonl`, and those names with `.zstd` / `.zst` |

```jsonl
{"mtx":1,"n":"Ride height","q":10000000,"dur":50000000,"vis":1,"utc":1742040000000000000,"tz":"America/New_York"}
{"n":"Ride Height FL","hz":100,"u":"mm","vis":1,"v":[42,41,40,39],"t0":10000000}
```

### 11.2 Header

| Key | Type | Required | Meaning |
|---|---|---|---|
| `mtx` | integer | yes | Extension spec version. This version is `1`. |
| `n` | string | yes | Group name shown as the folder title, e.g. `Sebring 12H 2025`. 1–64. |
| `q` | integer ≥ 1 | yes | Lattice quantum of **this group**, nanoseconds. |
| `dur` | integer ≥ 0 | yes | Exclusive end of this group's own timeline. |
| `vis` | `0` or `1` | yes | `1` group starts expanded. `0` starts collapsed. |
| `utc` | integer ≥ 1 | yes | Unix-epoch nanoseconds (UTC) at this group's `t = 0`. |
| `tz` | string | yes | IANA venue timezone for this group. Copy from the host when the host has one. |
| `r` | array | no | Right-aligned chrome. See below. |
| `o` | integer ≥ 0 | no | Lattice origin. Default `0`. |
| `clk` | string | no | Vendor clock name. Not a join key. |
| `abs` | integer | no | Vendor-clock reading. Not a join key. |
| `abe` | integer | no | Source-clock end. |
| `hash` | string | no | 16-digit hex schema hash of the **host** this was built for. |

A header MUST contain `mtx` and MUST NOT contain `mtj`. `q`, `dur`, and `o`
obey §3.

Every repeated header is complete and independently defines `q`, `dur`, `o`,
`utc`, `tz`, and optional `hash` for the records that follow it. These fields
MAY differ between groups; attach computes the host shift from each group's
`utc`.

**Right-aligned chrome `r`.** Order is left-to-right on the right side of the
group header. Each element is one of:

| Shape | Meaning |
|---|---|
| `{"t":"LMP2 stints during the race"}` | Description text. |
| `{"p":["Avg lap","1:52.1"]}` | Fact pill. First string is the label, second is the value. |

Use `t` for sentences. Use `p` for short facts.

Do not put driver, vehicle, venue, laps, or `src` here. Those belong on the
host. Unknown keys are ignored.

### 11.3 Join (nanoseconds is the primary key)

An extension does not store per-sample timestamps. The primary key is the
same integer nanosecond as the host. Every group header MUST stamp `utc`
(Unix-epoch ns at that group's `t = 0`) and `tz`. Join uses only those
nanoseconds:

```
t[i] = t0 + i · period_ns(hz)
host_file_ns = ext_file_ns + ext.utc − host.utc
```

`tz`, `clk`, `abs`, filename, phase, and civil dates are not join keys.

**Write host file-relative nanoseconds.** That is the default and the usual
path. Open the host, read `laps[].start_ns` / `end_ns` or any
`sample_time_ns`, snap to that group's `q`, put those integers in `t0` / `s` /
`e`. Copy the host's `utc` and `tz` onto the group header (then
`group.utc == host.utc` and the shift is zero). Do not convert through a
timezone. A sidecar that
only covers lap 3 starts at that lap's host `start_ns`; it does not reset
to zero.

If you have a UTC instant and the host has `utc`:

```
s = utc_epoch_ns − host.utc
```

Then copy `host.utc` and `host.tz` onto the sidecar — the times are already
on the host axis. If the host has no `utc`, you cannot join by wall clock.

If the sidecar keeps its own `t = 0`, set sidecar `utc` to that instant.
`attach` applies the subtraction above. If the host has no `utc`, the
sidecar times are treated as already host-relative (shift zero).

After the shift, a host time `t` hits sidecar sample `i` when
`t = t0_on_host + i · period_ns`. Between samples the host's usual
`sample_at` rule applies (step vs linear). Outside
`[t0_on_host, t0_on_host + n · period_ns)` the sidecar channel is absent.

The sidecar's `dur` is independent of the host's `dur`. A sidecar MAY start
after the host, end before it, or extend past it. Overlay MUST NOT replace a
host channel. A name that already exists on the host is an error. If the
sidecar carries `hash` and the host has a schema hash, they MUST match.

`q` need not equal the host's `q`. Each file keeps its own lattice. Equal
computed timestamps are still the same instant (§3.2).

### 11.4 What an extension is not

- Not a second recording. It has no laps and no identity.
- Not a patch that edits host channels.
- Not a way to smuggle event streams. Channels MUST be lattice-aligned.
- Not defined for `.telemetry` zip files.

## 12. Spans

A **span** is an interval on the same file-relative timeline: beginning, end,
and string metadata. It is the OpenTelemetry idea on a race clock — a stint,
a yellow, a pit — not a sampled signal.

There is no folder record. In an MTX file, each `mtx` header starts a folder:
header `n` is its title and header `vis` is whether that group starts open.

Spans MAY appear in a recording (`mtj`) or an extension (`mtx`). They do not
use `hz` or `v`. Time join for an extension (§11.3) shifts every span `s`/`e`
by the same offset as channel `t0`.

### 12.1 Record kinds

Every non-header, non-laps line is a JSON object. Discriminate on `k`:

| `k` | Kind | Required keys |
|---|---|---|
| omitted or `c` | sample channel (§6) | `n`, `hz`, `v`, and `vis` in an MTX file |
| `s` | span | `s`, `e`, and `vis` in an MTX file |

`k` of `f` is an error. Unknown `k` is an error.

In an MTX file every record MUST include `vis` (`0` hidden by default, `1`
shown). A host `mtj` recording MAY omit `vis` (treated as `1`).

### 12.2 Span

```
{"k":"s","n":"443-stint-1","s":0,"e":3600000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","18"],["Best",{"v":110332,"u":"timespan_ms"}],["Avg",{"v":112104,"u":"timespan_ms"}],["License","IMSA"]]}
```

| Key | Type | Required | Meaning |
|---|---|---|---|
| `k` | `"s"` | yes | Span. |
| `n` | string | no | Stable id. 1–64. Not shown if `p.title` is set. |
| `s` | integer ≥ 0 | yes | Inclusive start, file-relative nanoseconds. |
| `e` | integer ≥ 0 | yes | Exclusive end, file-relative nanoseconds. `e > s`. |
| `vis` | `0` or `1` | yes in MTX | Default visibility inside the sidecar group. |
| `c` | string | no | Display color `#RRGGBB` (six hex digits, `#` required). |
| `p` | object | no | **Primary** labels, drawn on the span. |
| `p.title` | string | no | Main label (car number, stint name). 1–32. |
| `p.sub` | string | no | Secondary label (`primary.subtitle`: driver, avg lap). 1–48. |
| `m` | array | no | **Meta** hover fields. Each element is `["Name", value]`. Value is a string, a `timespan_ms` integer, or `{"v":ms,"u":"timespan_ms"}`. |

`s` and `e` MUST be lattice points. The span occupies `[s, e)`.

**Primary vs meta.** `p` is on-span chrome. `m` is on-hover only. Text
stays a string (`"IMSA"`, `"28"`). Race times MUST be `timespan_ms`
(§13.1) so they can be averaged.

```
["Best",{"v":110332,"u":"timespan_ms"}]
["Best",110332]
```

A legacy `"1:50.332"` string is accepted and parsed. Writers emit the
object (or the integer).

### 12.3 Example: 12 h of LMP2 stints

This example has one group named in its header. Collapsing the group hides
every stint. Individual `vis` still controls whether a span is on when the
group is open. Additional folders can follow by writing another complete
`mtx` header and its records.

```jsonl
{"mtx":1,"n":"Sebring 12H 2025","q":1000000,"dur":12600000000000,"vis":1,"utc":1742040000000000000,"tz":"America/New_York","r":[{"t":"LMP2 stints during the race"},{"p":["Avg lap","1:52.1"]}]}
{"k":"s","n":"443-1","s":0,"e":5400000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","28"],["Best",{"v":110332,"u":"timespan_ms"}],["Avg",{"v":112104,"u":"timespan_ms"}],["Total drive time",{"v":5400000,"u":"timespan_ms"}],["Driver License","IMSA"]]}
{"k":"s","n":"443-2","s":5400000000000,"e":12600000000000,"vis":1,"c":"#2563eb","p":{"title":"#443","sub":"MB · 1:51.8"},"m":[["Laps","38"],["Best",{"v":110110,"u":"timespan_ms"}],["Avg",{"v":111804,"u":"timespan_ms"}],["Total drive time",{"v":7200000,"u":"timespan_ms"}],["Driver License","IMSA"]]}
```

## 13. Writer-strict string lengths

Lengths are Unicode code points (JSON Schema `maxLength`). A writer MUST
omit the key rather than emit `""`. The canonical table and worked
`examples` are in [`telemetry.schema.json`](../../telemetry.schema.json).

| Field | min | max |
|---|---:|---:|
| Channel `n`, span `n`, MTX group `n` | 1 | 64 |
| `drv` `veh` `ven` `evt` `ses` | 1 | 80 |
| `u` | 1 | 24 |
| `fmt` (no whitespace) | 1 | 16 |
| `tz` | 3 | 64 |
| Label text | 1 | 80 |
| Chrome `r[].t` | 1 | 120 |
| Chrome pill part | 1 | 32 |
| `p.title` / `p.sub` | 1 | 32 / 48 |
| `m` name / value | 1 | 32 / 48 |
| `date` `time` `clk` | 1 | 32 |
| `hint` | 1 | 64 |
| `hash` | 16 | 16 |
| `c` | 7 | 7 |

`r` MUST have at most 8 items, `m` at most 16 pairs, `lbl` at most 256
pairs.

### 13.1 `timespan_ms`

Race durations (lap, sector, stint, drive time) are stored as **integer
milliseconds**. The type is `u32`. The legal range is `0..=360000000`
(100 hours inclusive). That is enough for a 24 h race and still exact
for `avg = round(sum(v) / n)`.

| | |
|---|---|
| Unit / format token | `timespan_ms` (aliases `laptime_ms`, `racetime_ms`) |
| Storage | integer ms, `0..=360000000` |
| SI factor | `0.001` (same dimension as `ms` / `s`) |
| Display < 1 h | `M:SS.FFF` — `110332` → `1:50.332` |
| Display ≥ 1 h | `H:MM:SS.FFF` — `5400000` → `1:30:00.000` |

On a channel: `"u":"timespan_ms"` and/or `"fmt":"timespan_ms"`, `v` is
milliseconds. On span meta: `{"v":110332,"u":"timespan_ms"}` or the
integer `110332`. Readers still parse a racing-time string. Writers MUST
NOT write a float.

`timespan_ms` converts to `s`, `ms`, `min`, `h` through the unit
registry. It does not convert to a civil clock.

### 13.2 Convertible units

The file MAY store any 1–24 unit string the source declared. Conversion
is defined only for the registry in
`crates/telemetry-core/src/units.rs` and documented in
`telemetry.schema.json` `$defs.unitCatalog`. Same dimension only.

Accepted both ways, among others:

| Dimension | Canonical | Also accepted |
|---|---|---|
| Speed | `km/h` | `kph`, `kmh`, `kmph`, `km/hr` |
| Speed | `mph` | `mi/h`, `mp/h` |
| Pressure | `bar` | `Bar` |
| Pressure | `psi` | `PSI`, `lbf/in^2` |
| Temperature | `C` | `°C`, `degC` |
| Time | `timespan_ms` | `laptime_ms`, `racetime_ms` |

`convert(100, "km/h", "mph")` and `convert(1, "bar", "psi")` are the
supported pair conversions. A writer SHOULD emit the canonical name.
