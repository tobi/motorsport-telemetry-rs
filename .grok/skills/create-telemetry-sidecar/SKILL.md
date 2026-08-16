---
name: create-telemetry-sidecar
description: >
  Author and validate Motorsport Telemetry JSONL sidecars (MTX). Extra sample
  channels and/or time spans joined onto a host recording by integer
  nanoseconds. Header utc (Unix-epoch ns at t=0) is required. Use when the
  user says sidecar, MTX, .telemetry.ext.jsonl, span, stint overlay, LMP2
  field, extra channels, ride height overlay, validate MTX, or runs
  /create-telemetry-sidecar or /telemetry-sidecar.
---

# MTX sidecar

JSONL only (plain or zstd). Not a `.telemetry` zip. Each `mtx` header starts a
folder and governs records until the next `mtx` header or end of file. There is
no folder record and no `k:"f"`.

The primary key is integer nanoseconds. Normative rules:
`crates/telemetry-format/JSONL.md` §3 and §11.3.

## File

Preferred: `Name.telemetry.ext.jsonl` or `Name.telemetry.ext.jsonl.zstd`.

One or more groups, no laps line, no blank lines, compact JSON, `LF`. Each
group is:

1. One complete header object (`mtx`, never `mtj`).
2. Zero or more sample channels and/or spans.

Write another complete `mtx` header to start another folder in the same file.
Each header independently defines the group's `q`, `dur`, `o`, `utc`, `tz`,
and optional `hash`.

Write with `write_jsonl_timeline` (spans only) or
`write_jsonl_extension_from_source` (channels). Default write is zstd level 11.

## Header (required)

| Key | Type | Meaning |
|---|---|---|
| `mtx` | `1` | Sidecar document |
| `n` | non-empty string | Group title, e.g. `Sebring 12H 2025` |
| `q` | integer ≥ 1 | This group's lattice quantum, nanoseconds |
| `dur` | integer ≥ 0 | Exclusive end of this group's timeline |
| `vis` | `0` or `1` | `1` group starts expanded, `0` starts collapsed |
| `utc` | integer ≥ 1 | Unix-epoch nanoseconds (UTC) at this group's `t = 0` |
| `tz` | IANA string | Venue timezone for this group. Copy from the host. |

`utc` is required: Unix-epoch nanoseconds at this group's `t = 0`. That is how
the group sits on the same axis as the host. Copy host `utc` and `tz` when
times are host-relative.

Optional: `r` (right chrome), `o` (origin, default 0), `hash` (host schema
hex). Do not join with `clk`/`abs`.

### Right chrome `r`

Left-to-right on the **right** of the group header. Mix freely:

- Text (descriptions): `{"t":"LMP2 stints during the race"}`
- Pill (facts): `{"p":["Avg lap","1:52.1"]}`

## Records

Every MTX record MUST include `vis` (`1` shown when the group is open, `0` hidden).

### Sample channel (`k` omitted or `"c"`)

Same as a recording channel, plus `vis`:

```
{"n":"Ride Height FL","hz":100,"u":"mm","vis":1,"v":[42,41],"t0":10000000}
```

`t[i] = t0 + i/hz`. `t0` defaults to header `o`. Unique `n`.

`plt` is `trace` (default), `gauge`, or `compass`. Foreign signals
(temperature, BPM, SpO2, wind direction) are `gauge` or `compass` with
optional `sc`/`rnd`/`fmt`. Do not overlay them on Speed.

Optional `lbl` **only on traces**: `[[ns,"comment"],…]`. Dot on the
trace; hover expands a dotted vertical across the full view height.

### Span (`k":"s"`)

An interval, not a sampled signal.

```
{"k":"s","n":"443-1","s":0,"e":5400000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","28"],["Best","1:50.332"]]}
```

| Key | Role |
|---|---|
| `s`, `e` | `[start, end)` file-relative ns, lattice points, `e > s` |
| `c` | `#RRGGBB` on the bar |
| `p.title` / `p.sub` | Drawn **on** the span |
| `m` | Hover only. `["Name", value]`. Race times are `timespan_ms`. |

Do not put hover facts in `p`. Do not put the title in `m`.

Race times (Best, Avg, drive time) use `timespan_ms`: integer milliseconds
`0..=360000000` (100 h, `u32`). Renders `M:SS.FFF` / `H:MM:SS.FFF`. Write
`["Best",{"v":110332,"u":"timespan_ms"}]` so averages are `mean(v)`.
`km/h` and `mph` (`mp/h`) convert; `bar` and `psi` convert.

`k:"f"` is invalid.

## Primary key (integer nanoseconds)

Every `t0`, `s`, `e`, `dur` is file-relative ns on its current group's axis.
That group's header `utc` (required) is Unix-epoch ns at its `t = 0`:

```
absolute_ns = file_relative_ns + utc
host_t      = ext_t + ext.utc − host.utc
```

Usual path: copy host lap/`sample_time_ns` integers, snap to `q`, copy
host `utc`+`tz`. A stint in lap 3 uses that lap's `start_ns`, not `0`.

Wall-clock instant: `s = utc_epoch_ns − host.utc`, then copy host `utc`.
Sidecar-local zero: set sidecar `utc` to that instant; `attach` subtracts.

`tz` is display only. Do not write ISO strings, seconds, or a timezone
inside `s`/`e`. Do not join with `clk`/`abs`. Full rules: JSONL.md §3,
§11.3.

## Example

```jsonl
{"mtx":1,"n":"Sebring 12H 2025","q":1000000,"dur":12600000000000,"vis":1,"utc":1742040000000000000,"tz":"America/New_York","r":[{"t":"LMP2 stints during the race"},{"p":["Avg lap","1:52.1"]}]}
{"k":"s","n":"443-1","s":0,"e":5400000000000,"vis":1,"c":"#e11d48","p":{"title":"#443","sub":"EL · 1:52.1"},"m":[["Laps","28"],["Best",{"v":110332,"u":"timespan_ms"}],["Avg",{"v":112104,"u":"timespan_ms"}],["Total drive time",{"v":5400000,"u":"timespan_ms"}],["Driver License","IMSA"]]}
{"k":"s","n":"443-2","s":5400000000000,"e":12600000000000,"vis":1,"c":"#2563eb","p":{"title":"#443","sub":"MB · 1:51.8"},"m":[["Laps","38"],["Best",{"v":110110,"u":"timespan_ms"}],["Avg",{"v":111804,"u":"timespan_ms"}],["Total drive time",{"v":7200000,"u":"timespan_ms"}],["Driver License","IMSA"]]}
```

## Validate

```sh
python3 crates/telemetry-format/scripts/validate-mtx.py PATH.telemetry.ext.jsonl
```

Schema: `telemetry.schema.json` at the repo root. Validator:
`crates/telemetry-format/scripts/validate-mtx.py`. Global copy of both:
`~/.agents/skills/create-telemetry-sidecar/`.

## String lengths (Unicode code points)

Omit the key instead of `""`. `fmt` has no whitespace (`0.0°C`).

| Field | max | Example |
|---|---:|---|
| `n` (group, channel, span) | 64 | `Sebring 12H 2025` |
| `u` | 24 | `mm` |
| `fmt` | 16 | `0.0°C` |
| `tz` | 64 | `America/New_York` |
| Label text | 80 | `bottomed` |
| `r[].t` | 120 | `LMP2 stints during the race` |
| Pill part | 32 | `Avg lap` |
| `p.title` / `p.sub` | 32 / 48 | `#443` / `EL · 1:52.1` |
| `m` name / value | 32 / 48 | `Best` / `1:50.332` |

`r` ≤ 8, `m` ≤ 16, `lbl` ≤ 256. Worked examples: repo-root `telemetry.schema.json`.

## Author checklist

- Every group header has `n`, `vis`, required `utc` (Unix-epoch ns at t=0), and `tz`.
- `r` uses `t` for prose, `p` for facts.
- Every channel and span has `vis`.
- No folder objects; another complete `mtx` header starts the next folder.
- Times are integer nanoseconds (host file-relative, or epoch minus `host.utc`).
- No ISO strings, no seconds, no timezone inside `s`/`e`.
- Colors are `#` plus six hex digits.
- Meta values are strings.
- Strings fit the length table; `fmt` is `0.0°C` not `"0.0 °C"`.
