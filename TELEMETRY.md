# Motorsport Telemetry format

This is the accurate description of the on-disk formats and the shared
channel model. The writer-strict JSON Schema is
[`telemetry.schema.json`](telemetry.schema.json). The normative JSONL MUST
rules are [`crates/telemetry-format/JSONL.md`](crates/telemetry-format/JSONL.md).

There are two encodings of the **same** recording model:

| Encoding | Name | Role |
|---|---|---|
| JSONL | MTJ / MTX | Inspectable interchange. Sidecars exist only here. |
| STORE zip + FlatBuffers | `.telemetry` | Lossless archive of one recording. No sidecar member. |

JSONL is the working interchange. `.telemetry` exists to keep native
integer columns, scale/bias, and video frame tables. An MTX sidecar is
never a zip member; join it onto an MTJ host with `JsonlRecording::attach`.

## Files

| Kind | Preferred name | First line |
|---|---|---|
| Native archive | `Name.telemetry` | zip local header, first member `metadata.fb` |
| Recording | `Name.telemetry.jsonl` or `.zstd` | `{"mtj":1,...}` |
| Sidecar | `Name.telemetry.ext.jsonl` or `.zstd` | `{"mtx":1,...}` |

Also accepted: `.jsonl`, `.mtj`, `.ext.jsonl`, `.mtx.jsonl`, and those
names with `.zstd` / `.zst`. UTF-8, no BOM. `LF` newlines. Writers emit
compact JSON (no space after `:` or `,`). A zstd frame (`28 B5 2F FD`)
MAY wrap the UTF-8 document. This crate writes zstd level 11 by default.

```sh
cargo run -p motorsport-telemetry -- convert run.pds
cargo run -p motorsport-telemetry -- convert run.pds run.telemetry.jsonl
cargo run -p motorsport-telemetry -- verify run.telemetry run.telemetry.jsonl
python3 crates/telemetry-format/scripts/validate-mtx.py stints.telemetry.ext.jsonl
```

## Time

The primary key is **integer nanoseconds**. There is no other key.

```
file-relative  t = 0 is the start of that file
absolute       utc_epoch_ns = file_relative_ns + utc
join           host_file_ns = ext_file_ns + ext.utc − host.utc
sample         t[i] = t0 + i · period_ns(hz)
```

These integers are not civil datetimes, not Eastern, not DST, not ISO-8601.

`utc` is Unix-epoch nanoseconds at this file's `t = 0`. `tz` is an IANA
zone (`America/New_York`) used only to format a paddock wall clock. Never
join with `tz`. `date` / `time` are decorative vendor strings. `clk`+`abs`
is leftover vendor-clock metadata and is not a join key.

Header `q` is the lattice quantum. `o` defaults to `0`. Every `t0`, sample
time, lap boundary, span `s`/`e`, label `ns`, and `dur` is a lattice point
`o + k·q`. `dur` is exclusive.

```
period_ns(hz) = 1e9/hz     if hz is a positive integer that divides 1e9
              = round(1e9/hz) otherwise
```

No per-sample timestamps. Unaligned streams are omitted, not stored as
`[t,v]` pairs.

## Shared channel model

These capabilities exist in **both** MTJ and `.telemetry` catalog v9:

| Feature | JSONL | Native catalog |
|---|---|---|
| Sample channel `n` `hz` `u` `v` `t0` | yes | yes (`v` is a typed column) |
| Visibility | `vis` | V(26) u8 per channel (v5) |
| Plot class | `plt` | V(29) u8 (v7) |
| Display scale | `sc` | V(29) f64le min/max (v7) |
| Rounding | `rnd` `fmt` | V(29) u8 + string (v7) |
| Trace comments | `lbl` | V(28) (v6). Trace only. |
| Spans + string / `timespan_ms` meta | `k:"s"` | V(27) (v5 strings, v8 typed ms) |
| `utc` + IANA `tz` | header | V(23–25) (v4) |
| Video linkage (offset, file refs + BLAKE3, frame table) | `vo` `vf` `vpts` | V(20–22) + `video_frames.bin` |
| Pass provenance + origin | `passes` `src` `srcp` | V(6, 7, 30) (v9) |

Native-only (not in JSONL): integer encodings, scale/bias, event time
columns, driver-stint lists.

JSONL-only: MTX sidecar files, lattice `q`/`o`, group chrome `r`, attach.

`fmt` has **no whitespace** and is at most 16 characters: `0.0°C`, `000`,
`000%`, `0°`. Never `0.0 °C`.

`lbl` is legal only when `plt` is omitted or `trace`. String length caps
(Unicode code points) live in [`telemetry.schema.json`](telemetry.schema.json):
names 64, identity 80, units 24, labels 80, chrome 120.

## JSONL recordings (MTJ)

Three sections, no blanks:

1. Header object (`mtj`, never `mtx`).
2. Laps array (`[]` if none).
3. Channel and/or span objects.

```jsonl
{"mtj":1,"q":10000000,"dur":40000000,"src":"pds","drv":"Tobi","ven":"Sebring","utc":1742040000000000000,"tz":"America/New_York"}
[[1,0,40000000,0]]
{"n":"Speed","hz":100,"u":"km/h","v":[10,11,12,13],"lbl":[[10000000,"brake lock"]]}
{"n":"Water Temp","hz":1,"u":"°C","plt":"gauge","sc":[60,120],"rnd":1,"fmt":"0.0°C","v":[88.4]}
{"k":"s","n":"out-lap","s":0,"e":40000000,"p":{"title":"Out"},"m":[["Note","install lap"]]}
```

### MTJ header

`mtj` `q` `dur` required. `utc` `tz` required on write when known. Optional:
`o` `src` `srcp` `drv` `veh` `ven` `evt` `ses` `date` `time` `clk` `abs`
`abe` `hint` `vo` `vf` `vpts` `passes` `hash`. `src` is `aimd` `pds`
`motec` `vbo` `telemetry`. Video linkage (`vo` recording presentation
offset ns, `vf` file refs with BLAKE3, `vpts` per-frame presentation
times) and pass provenance are normative in
[`JSONL.md`](crates/telemetry-format/JSONL.md) §4.2; `vpts` requires `vf`,
and MTX sidecars must not carry any of the three.

### Laps

`[number, start_ns, end_ns, complete]` or with `first_video_frame`.
`complete` is `0` or `1`. `end > start`. Both lattice points. Duration is
`end-start` and is not stored in JSONL.

### Sample channel

Required: `n` `hz` `v`. MTX also requires `vis`. Optional: `k` (`c`), `u`,
`t0`, `vis`, `plt`, `sc`, `rnd`, `fmt`, `lbl`.

`null` in `v` is a missing value at that instant. `t[i] = t0 + i·period`.

### Foreign channels

Do not overlay these on Speed/throttle.

| Signal | `plt` | typical `sc` | `rnd` | `fmt` |
|---|---|---|---|---|
| Temp | `gauge` | `[60,120]` | `1` | `0.0°C` |
| BPM | `gauge` | `[40,200]` | `0` | `000` |
| SpO2 | `gauge` | `[80,100]` | `0` | `000%` |
| Wind / heading | `compass` | `[0,360]` | `0` | `0°` |

`gauge` is its own pane. `compass` wraps 0–360°. Omit `sc` if you do not
know an honest range.

### Labels (`lbl`)

Trace only. `[[ns,"text"],…]`, lattice, strictly increasing, non-empty
text. Dot on that channel at `ns`. Hover: dotted vertical across the full
trace view.

### Spans

`k:"s"`. `[s,e)` lattice, `e>s`. `p` on-bar, `m` hover `["Name",value]`.
`c` is `#RRGGBB`. `k:"f"` is invalid.

Race-time meta is **`timespan_ms`**: integer milliseconds, `0..=360000000`
(100 h, stored as `u32`). Renders `M:SS.FFF` (`1:50.332`) or, from 1 h,
`H:MM:SS.FFF` (`1:30:00.000`). Write `{"v":110332,"u":"timespan_ms"}` or
the integer `110332`. Math is on `v` (mean of Best across stints). A
legacy `"1:50.332"` string is still accepted and parsed.

Every unit the registry can convert is listed in
[`telemetry.schema.json`](telemetry.schema.json) `$defs.unit` /
`$defs.unitCatalog`. `km/h` and `mph` (`mp/h`, `mi/h`) convert; `bar`
and `psi` convert. Same dimension only.

## JSONL sidecars (MTX)

JSONL only. One or more **groups**. Each group is an `mtx` header plus
records until the next `mtx` header or EOF. The group is the folder
(header `n` + `vis`). No laps line. No identity. No video.

Required header keys: `mtx` `n` `q` `dur` `vis` `utc` `tz`.
Optional: `o` `r` `clk` `abs` `abe` `hash`. Every record has `vis`.

```jsonl
{"mtx":1,"n":"Sebring 12H 2025","q":1000000,"dur":12600000000000,"vis":1,"utc":1742040000000000000,"tz":"America/New_York","r":[{"t":"LMP2 stints during the race"},{"p":["Avg lap","1:52.1"]}]}
{"k":"s","n":"443-1","s":0,"e":5400000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","28"],["Best",{"v":110332,"u":"timespan_ms"}]]}
{"n":"Ride Height FL","hz":100,"u":"mm","vis":1,"v":[42,41],"t0":10000000,"lbl":[[10000000,"bottomed"]]}
```

Join: `host_file_ns = ext_file_ns + ext.utc − host.utc`. Usual path: write
host file-relative ns and copy host `utc`/`tz` (shift is then zero).

Validate:

```sh
python3 crates/telemetry-format/scripts/validate-mtx.py PATH.telemetry.ext.jsonl
```

## Native `.telemetry` memory layout

A **STORE** zip (compression method 0). CRC-32 of each member. Each
payload starts on a **64-byte** boundary (padding lives in the extra
field of the local header). First member **must** be `metadata.fb`.

```
metadata.fb                 FlatBuffers catalog
video_frames.bin            optional; u64le presentation times, one per frame
channels/0000.bin           native samples for channel 0
channels/0001.bin
channels/0001.time.bin      only if catalog kind=1 (event / irregular)
```

### `metadata.fb` root table

Field index `N` is FlatBuffers vtable slot `V(N) = 4 + 2N`.

| N | Type | Content |
|---|---|---|
| 0 | u16 | `FORMAT_VERSION` (current **9**) |
| 1 | table | Identity: V(0..6) driver, vehicle, venue, event, session, date, time |
| 3 | `[u8]` | Packed laps |
| 4 | `[u8]` | Packed channel metadata |
| 6 | string | `source_format` (`pds`, `motec`, …) |
| 7 | string | `source_path` |
| 8 | u64 | schema hash |
| 9 | u64 | `duration_ns` |
| 10 | u64 | `sample_count` |
| 11 | u32 | `channel_count` |
| 12 | u32 | `sampled_channel_count` |
| 13 | u32 | `valid_laps` |
| 14 | string | comment |
| 15 | string | session hint |
| 16 | `[u8]` | Packed driver stints |
| 17–19 | string, u64, u64 | optional vendor clock name / start / end |
| 20 | `[u8]` | Packed video handles |
| 21–22 | u32, i64 | presentation offset present + ns |
| 23–24 | u32, u64 | `utc_start_ns` present + value (v4) |
| 25 | string | IANA `timezone` (v4) |
| 26 | `[u8]` | per-channel visibility (v5) |
| 27 | `[u8]` | packed spans (v5) |
| 28 | `[u8]` | packed labels (v6) |
| 29 | `[u8]` | packed display (v7) |
| 30 | `[u8]` | packed pass provenance (v9) |

`pack_string` = `u32le` UTF-8 length + bytes. All multi-byte integers
little-endian.

**Laps (V(3))** — `u32 count`, then each: `i64 number`, `u64 start`,
`u64 end`, `u64 duration`, `u8 complete`. If version ≥ 3: `u8` present
and optional `u64 first_video_frame`.

**Channels (V(4))** — `u32 count`, then each: `u32 id`, name, member,
time_member, unit_raw, unit_canonical, `u8 unit_source`, `u8 dimension`,
`u8 sample_type`, `u8 uses_step`, `u8 kind`, `f64 scale`, `f64 bias`,
`u64 sample_count`, `u64 duration_ns`, `u32 chunk_count`, then each chunk
`u64 period`, `u64 count`, `u64 sample_base`, `u64 time_base_ns`.

`kind` 0 = regular column, 1 = event (has `.time.bin`). `unit_source` 0
unknown, 1 declared, 2 spec default. `sample_type` 1 u8, 2 i16, 3 u16,
4 i32, 5 u32, 6 f32, 7 f64.

**Stints (V(16))** — `u32 count`, then `i64 driver_id`, `u64 start`,
`u64 end`.

**Videos (V(20))** — `u32 count`, then filename, `u32 index`, `u8` hashed
+ optional 32-byte BLAKE3, `u64 frame_count`. If version ≥ 3: `u8` present
+ optional `i64 presentation_offset_ns`.

**Visibility (V(26))** — one `u8` per channel, 1 = shown.

**Spans (V(27))** — `u32 count`, then name, `u64 s`, `u64 e`, `u8 vis`,
color, title, subtitle, `u32 meta_count`, then each pair: name,
`u8 kind` (v8: `0` text + `pack_string`, `1` `timespan_ms` + `u32le` ms).
v5–v7 stored two strings; a racing-time string is reread as `timespan_ms`.

**Labels (V(28))** — `u32 channel_count`, then per channel `u32 n` and
`n × (u64 time_ns, pack_string text)`.

**Display (V(29))** — `u32 channel_count`, then per channel: `u8 plot`
(0 trace, 1 gauge, 2 compass), `u8 flags` (bit0 min, bit1 max, bit2 rnd,
bit3 fmt), optional `f64` min, `f64` max, `u8` decimals, `pack_string` fmt.

**Passes (V(30))** — `u32 count`, then per applied pass: name, `u32
version`, `u32 param_count` × (key, value), `u32 input_count` × channel
name, `u32 output_count` × channel name. Strings are `pack_string`.
Records which processing passes produced which derived channels; the
v8 → v9 migration leaves it empty rather than inventing provenance.

**Sample column** — `channels/NNNN.bin` is `sample_count × byte_width`
native values, chunk after chunk, no per-sample timestamps. Decode
`raw * scale + bias`. Event channels add `channels/NNNN.time.bin` as
`u64le` file-relative ns per sample.

`NativeRecording::open` rewrites a writable older catalog in place.
Header-only reads do not. Migration never invents missing payload.

## Schema

[`telemetry.schema.json`](telemetry.schema.json) is the single writer-strict
schema. Every defined object is `additionalProperties: false`. Each `$defs`
entry has `description`, `examples` of how to write the property, and
`minLength` / `maxLength` on every string. `$comment` states the JSONL key
and the FlatBuffers slot / packed layout. Readers still ignore unknown keys
so old v1 JSONL clients can skip `plt` / `lbl` / `utc`.

`$defs.mtj_header` `laps` `channel` `span` `mtx_header` are the line
shapes. A file is still JSONL: validate line 1 as the matching header,
line 2 of an MTJ file as `laps`, and every other line as `channel` or
`span`.

Lengths are Unicode code points. Omit the key instead of writing `""`.

| Kind | max | Examples |
|---|---:|---|
| Channel / span / group `n` | 64 | `Speed`, `Ride Height FL`, `443-1` |
| Identity `drv` `veh` `ven` `evt` `ses` | 80 | `Tobi`, `Sebring` |
| Unit `u` | 24 | `km/h`, `°C`, `mm` |
| `fmt` (no whitespace) | 16 | `0.0°C`, `000`, `000%`, `0°` |
| `tz` | 64 | `America/New_York`, `UTC` |
| Label text | 80 | `brake lock` |
| Chrome `r[].t` | 120 | `LMP2 stints during the race` |
| Chrome pill part | 32 | `Avg lap`, `1:52.1` |
| Span `p.title` / `p.sub` | 32 / 48 | `#443`, `EL · 1:52.1` |
| Span `m` name / text value | 32 / 48 | `Best`, `IMSA` |
| `timespan_ms` | 0–360000000 | `110332` → `1:50.332` |
| `date` / `time` / `clk` | 32 | `16/03/2025`, `gps` |
| `hash` | 16 | `0123456789abcdef` |
| Color `c` | 7 | `#e11d48` |

`r` ≤ 8 items, `m` ≤ 16 pairs, `lbl` ≤ 256 pairs.

```sh
python3 crates/telemetry-format/scripts/validate-mtx.py --self-check
```
