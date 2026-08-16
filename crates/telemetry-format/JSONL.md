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
| `src` | string | no | Format of the original recording (`aimd`, `pds`, `motec`, `vbo`, ...). Carried through rewrites: a `.telemetry` -> MTJ hop keeps the vendor id. |
| `srcp` | string | no | Path of the original recording as seen at first conversion. Carried through rewrites like `src`. |
| `drv` | string | no | Driver name. |
| `veh` | string | no | Vehicle name or identifier. |
| `ven` | string | no | Venue / circuit. |
| `evt` | string | no | Event name. |
| `ses` | string | no | Session name. |
| `date` | string | no | Source date string, unchanged. |
| `time` | string | no | Source time string, unchanged. |
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
| `n` | string | yes | Channel name. Unique in the file. Non-empty. |
| `hz` | number > 0 | yes | Sample rate in hertz. See §3.1. |
| `u` | string | no | Unit as declared by the source. Omit when unknown. |
| `v` | array | yes | Values, oldest first. Each element is a JSON number or `null`. |
| `t0` | integer ≥ 0 | no | First-sample time, file-relative nanoseconds. Default `o`. |
| `vis` | `0` or `1` | yes in MTX | Default visibility. Optional on `mtj` (default `1`). |

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

Two sections only:

1. Exactly one **header** object whose identifying key is `mtx`, not `mtj`.
2. Zero or more **records**: sample channels (§6) and/or spans (§12).

The sidecar **is** the group. Host software treats every record in one MTX
file as one folder named by the header `n`. There is no folder record type.

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
| `n` | string | yes | Group name shown as the folder title, e.g. `Sebring 12H 2025`. Non-empty. |
| `q` | integer ≥ 1 | yes | Lattice quantum of **this sidecar**, nanoseconds. |
| `dur` | integer ≥ 0 | yes | Exclusive end of this sidecar's own timeline. |
| `vis` | `0` or `1` | yes | `1` group starts expanded. `0` starts collapsed. |
| `utc` | integer ≥ 1 | yes | Unix-epoch nanoseconds (UTC) at this sidecar's `t = 0`. |
| `tz` | string | yes | IANA venue timezone. Copy from the host when the host has one. |
| `r` | array | no | Right-aligned chrome. See below. |
| `o` | integer ≥ 0 | no | Lattice origin. Default `0`. |
| `clk` | string | no | Vendor clock name. Not a join key. |
| `abs` | integer | no | Vendor-clock reading. Not a join key. |
| `abe` | integer | no | Source-clock end. |
| `hash` | string | no | 16-digit hex schema hash of the **host** this was built for. |

A header MUST contain `mtx` and MUST NOT contain `mtj`. `q`, `dur`, and `o`
obey §3.

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
same integer nanosecond as the host. A sidecar MUST stamp `utc` (Unix-epoch
ns at its own `t = 0`) and `tz`. Join uses only those nanoseconds:

```
t[i] = t0 + i · period_ns(hz)
host_file_ns = ext_file_ns + ext.utc − host.utc
```

`tz`, `clk`, `abs`, filename, phase, and civil dates are not join keys.

**Write host file-relative nanoseconds.** That is the default and the usual
path. Open the host, read `laps[].start_ns` / `end_ns` or any
`sample_time_ns`, snap to `q`, put those integers in `t0` / `s` / `e`. Copy
the host's `utc` and `tz` onto the sidecar header (then `ext.utc == host.utc`
and the shift is zero). Do not convert through a timezone. A sidecar that
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

There is no folder record. The MTX file is the group: header `n` is the
folder title, header `vis` is whether that group starts open.

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
{"k":"s","n":"443-stint-1","s":0,"e":3600000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","18"],["Best","1:50.332"],["Avg","1:52.104"],["License","IMSA"]]}
```

| Key | Type | Required | Meaning |
|---|---|---|---|
| `k` | `"s"` | yes | Span. |
| `n` | string | no | Stable id. Not shown if `p.title` is set. |
| `s` | integer ≥ 0 | yes | Inclusive start, file-relative nanoseconds. |
| `e` | integer ≥ 0 | yes | Exclusive end, file-relative nanoseconds. `e > s`. |
| `vis` | `0` or `1` | yes in MTX | Default visibility inside the sidecar group. |
| `c` | string | no | Display color `#RRGGBB` (six hex digits, `#` required). |
| `p` | object | no | **Primary** labels, drawn on the span. |
| `p.title` | string | no | Main label (car number, stint name). |
| `p.sub` | string | no | Secondary label (`primary.subtitle`: driver, avg lap). |
| `m` | array | no | **Meta** hover fields. Each element is `["Name","value"]`. Order is the hover order. |

`s` and `e` MUST be lattice points. The span occupies `[s, e)`.

**Primary vs meta.** `p` is on-span chrome. `m` is on-hover only. Values in
`m` stay strings (`"1:50.332"`, not a typed duration).

### 12.3 Example: 12 h of LMP2 stints

One sidecar, one group named in the header. Collapsing the group hides every
stint. Individual `vis` still controls whether a span is on when the group
is open.

```jsonl
{"mtx":1,"n":"Sebring 12H 2025","q":1000000,"dur":12600000000000,"vis":1,"utc":1742040000000000000,"tz":"America/New_York","r":[{"t":"LMP2 stints during the race"},{"p":["Avg lap","1:52.1"]}]}
{"k":"s","n":"443-1","s":0,"e":5400000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","28"],["Best","1:50.332"],["Avg","1:52.104"],["Total drive time","1:30:00"],["Driver License","IMSA"]]}
{"k":"s","n":"443-2","s":5400000000000,"e":12600000000000,"vis":1,"c":"#2563eb","p":{"title":"#443","sub":"MB · 1:51.8"},"m":[["Laps","38"],["Best","1:50.110"],["Avg","1:51.804"],["Total drive time","2:00:00"],["Driver License","IMSA"]]}
```
