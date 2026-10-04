# Motorsport Telemetry format

This is the accurate description of the on-disk formats and the shared
channel model. The writer-strict JSON Schema is
[`telemetry.schema.json`](telemetry.schema.json). The normative JSONL MUST
rules are [`crates/telemetry-format/JSONL.md`](crates/telemetry-format/JSONL.md).

A recording is an MTJ JSONL document. `.telemetry` and
`.telemetry.jsonl.zstd` contain the same document in one zstd frame; the
short name is the default destination. Readers detect compression by
content. MTX sidecars are separate JSONL documents joined with
`JsonlRecording::attach`.

MTJ aligns samples to each channel's `hz`/`t0` lattice and rounds values.
Irregular event streams are omitted. Keep the original vendor recording
when the exact source encoding or irregular samples are needed.
Header-only reads stream decompression through the header and laps lines;
they never parse channel values in current recordings.

## Files

| Kind | Preferred name | First line |
|---|---|---|
| Recording (default) | `Name.telemetry` | zstd frame `28 B5 2F FD` wrapping `{"mtj":1,...}` |
| Recording | `Name.telemetry.jsonl` or `.zstd` | `{"mtj":1,...}` |
| Sidecar | `Name.telemetry.ext.jsonl` or `.zstd` | `{"mtx":1,...}` |

Also accepted: `.jsonl`, `.mtj`, `.ext.jsonl`, `.mtx.jsonl`, and those
names with `.zstd` / `.zst`. UTF-8, no BOM. `LF` newlines. Writers emit
compact JSON (no space after `:` or `,`). A zstd frame (`28 B5 2F FD`)
MAY wrap the UTF-8 document. This crate writes zstd level 11 by default.

```sh
cargo run -p motorsport-telemetry -- convert run.pds                       # run.pds.telemetry (zstd MTJ)
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

| Feature | JSONL keys |
|---|---|
| Sample channel | `n` `hz` `u` `v` `t0` |
| Visibility | `vis` |
| Plot class and display | `plt` `sc` `rnd` `fmt` |
| Trace comments | `lbl` |
| Spans and typed metadata | `k:"s"` `p` `m` |
| UTC placement | `utc` `tz` |
| Video linkage | `vo` `vf` `vpts` |
| Pass provenance and origin | `passes` `src` `srcp` |
| Sidecar groups | `mtx` `n` `vis` `r` |

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
`abe` `hint` `vo` `vf` `vpts` `passes` `meta` `hash`. `src` is `aimd` `pds`
`motec` `vbo` `telemetry`. Video linkage (`vo` recording presentation
offset ns, `vf` file refs with BLAKE3, `vpts` per-frame presentation
times) and pass provenance are normative in
[`JSONL.md`](crates/telemetry-format/JSONL.md) §4.2; `vpts` requires `vf`,
and MTX sidecars must not carry any of the three.

`meta` stores file-level descriptive metadata, including resolved `TRACK.yml`
fields. It is an optional JSON object, omitted when empty. Unknown keys,
nested objects, arrays, and `null` values survive recording conversion and
rewrites. For example:

```json
{"meta":{"driver":{"name":"Tobi"},"track":{"name":"Sebring"},"session":"Practice","setup":{"tyres":["soft",null],"wet":false}}}
```

The ordinary identity header fields retain the native source identity.
`metadata().identity` applies recognized descriptive overrides from `meta`;
`identity()` still returns native values. Both header-only and full reads expose
the complete object as `FileMetadata.extra`. Date, track, and session overrides
do not change timestamps, timezone placement, or session keys. `meta` is MTJ-only;
MTX export does not persist it. See `JSONL.md` §4.3.

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

## Schema

[`telemetry.schema.json`](telemetry.schema.json) is the single writer-strict
schema. Format objects use `additionalProperties: false`; the MTJ `meta`
object intentionally accepts arbitrary keys and JSON values. Each `$defs`
entry has `description`, `examples` of how to write the property, and
`minLength` / `maxLength` on every string. `$comment` explains the JSONL key. Readers still ignore unknown keys
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
