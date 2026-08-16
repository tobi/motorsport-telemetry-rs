#!/usr/bin/env python3
"""Validate a Motorsport Telemetry MTX sidecar (.telemetry.ext.jsonl).

Checks each line against telemetry.schema.json (when the jsonschema package
is installed) and always enforces lattice, uniqueness, string lengths, and
MTX document rules from JSONL.md §3 / §11 / §12 / §13. Use --self-check to
validate every examples[] value in the schema.
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from pathlib import Path

ZSTD_MAGIC = b"\x28\xb5\x2f\xfd"

# Unicode code-point limits. Keep in sync with telemetry.schema.json $defs.
MAX_NAME = 64
MAX_UNIT = 24
MAX_FMT = 16
MAX_TZ = 64
MAX_LABEL = 80
MAX_CHROME = 120
MAX_PILL = 32
MAX_TITLE = 32
MAX_SUB = 48
MAX_META_NAME = 32
MAX_META_VALUE = 48
MAX_CLK = 32
MAX_CHROME_ITEMS = 8
MAX_META_ITEMS = 16
MAX_LABELS = 256
TIMESPAN_MS_MAX = 360_000_000
TIMESPAN_UNITS = {"timespan_ms", "laptime_ms", "racetime_ms"}


def schema_path() -> Path:
    here = Path(__file__).resolve()
    candidates = [
        here.parents[3] / "telemetry.schema.json",
        Path.cwd() / "telemetry.schema.json",
        Path.home() / ".agents/skills/create-telemetry-sidecar/schema/telemetry.schema.json",
        here.parent.parent / "schema" / "mtx.schema.json",
    ]
    for path in candidates:
        if path.is_file():
            return path
    raise SystemExit("telemetry.schema.json not found (repo root or skill schema/)")


def read_document(path: Path) -> bytes:
    data = path.read_bytes()
    if data.startswith(ZSTD_MAGIC) or path.suffix.lower() in {".zstd", ".zst"}:
        try:
            return subprocess.check_output(["zstd", "-dc"], input=data)
        except FileNotFoundError as err:
            raise SystemExit("zstd is required to decompress this sidecar") from err
        except subprocess.CalledProcessError as err:
            raise SystemExit(f"zstd failed to decompress {path}") from err
    return data


def load_jsonschema():
    try:
        import jsonschema
        from jsonschema import Draft202012Validator
    except ImportError:
        return None
    return jsonschema, Draft202012Validator


def period_ns(hz: float) -> int | None:
    if not math.isfinite(hz) or hz <= 0:
        return None
    if hz == int(hz) and hz <= 1_000_000_000:
        hz_u = int(hz)
        if hz_u > 0 and 1_000_000_000 % hz_u == 0:
            return 1_000_000_000 // hz_u
    period = round(1e9 / hz)
    if period < 1:
        return None
    return period


def on_lattice(value: int, origin: int, quantum: int) -> bool:
    return value >= origin and (value - origin) % quantum == 0


def is_vis(value) -> bool:
    return value in (0, 1, False, True)


def error(line: int, message: str) -> str:
    return f"line {line}: {message}"


def check_string(value, what: str, max_len: int, line: int, errors: list[str], *, min_len: int = 1) -> None:
    if not isinstance(value, str):
        errors.append(error(line, f"{what} must be a string"))
        return
    n = len(value)
    if n < min_len:
        errors.append(error(line, f"{what} must be at least {min_len} character(s)"))
    elif n > max_len:
        errors.append(error(line, f"{what} is {n} characters; max is {max_len}"))


def has_insignificant_ws(line: str) -> bool:
    in_string = False
    escape = False
    for char in line:
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char in " \t":
            return True
    return False


def ref_schema(schema: dict, name: str) -> dict:
    return {"$defs": schema.get("$defs", {}), "$ref": f"#/$defs/{name}"}


def validate_schema(kind: str, obj: dict, schema: dict, validator_cls, errors: list[str], line: int):
    validator = validator_cls(schema, format_checker=None)
    for err in validator.iter_errors(obj):
        path = "/".join(str(p) for p in err.path)
        where = f" ({path})" if path else ""
        errors.append(error(line, f"{kind} schema{where}: {err.message}"))


def validate_header(obj: dict, line: int, errors: list[str]) -> tuple[int, int, int] | None:
    if not isinstance(obj, dict):
        errors.append(error(line, "header must be a JSON object"))
        return None
    if "mtj" in obj:
        errors.append(error(line, "header cannot contain mtj"))
    if obj.get("mtx") != 1:
        errors.append(error(line, "header mtx must be 1"))
        return None
    for key in ("n", "q", "dur", "vis", "utc", "tz"):
        if key not in obj:
            errors.append(error(line, f"header is missing {key}"))
    name = obj.get("n")
    if not isinstance(name, str) or not name:
        errors.append(error(line, "header n must be a non-empty string"))
    else:
        check_string(name, "header n", MAX_NAME, line, errors)
    q = obj.get("q")
    if not isinstance(q, int) or isinstance(q, bool) or q < 1:
        errors.append(error(line, "header q must be an integer ≥ 1"))
        return None
    o = obj.get("o", 0)
    if not isinstance(o, int) or isinstance(o, bool) or o < 0:
        errors.append(error(line, "header o must be an integer ≥ 0"))
        return None
    dur = obj.get("dur")
    if not isinstance(dur, int) or isinstance(dur, bool) or dur < 0:
        errors.append(error(line, "header dur must be an integer ≥ 0"))
        return None
    if not on_lattice(o, o, q):
        errors.append(error(line, "header o is not on the time lattice"))
    if dur < o or (dur - o) % q != 0:
        errors.append(error(line, "header dur is not on the time lattice"))
    if not is_vis(obj.get("vis")):
        errors.append(error(line, "header vis must be 0 or 1"))
    utc = obj.get("utc")
    if not isinstance(utc, int) or isinstance(utc, bool) or utc < 1:
        errors.append(error(line, "header utc must be Unix-epoch nanoseconds at t=0"))
    tz = obj.get("tz")
    if not isinstance(tz, str) or not tz:
        errors.append(error(line, "header tz must be an IANA timezone"))
    elif tz != "UTC" and "/" not in tz:
        errors.append(error(line, f"header tz is not an IANA timezone: {tz}"))
    else:
        check_string(tz, "header tz", MAX_TZ, line, errors, min_len=3)
    clk = obj.get("clk")
    if clk is not None:
        check_string(clk, "header clk", MAX_CLK, line, errors)
    if "hash" in obj:
        h = obj["hash"]
        if not isinstance(h, str) or len(h) != 16 or any(c not in "0123456789abcdef" for c in h):
            errors.append(error(line, "header hash must be 16-digit lowercase hex"))
    if "r" in obj:
        if not isinstance(obj["r"], list):
            errors.append(error(line, "header r must be an array"))
        elif len(obj["r"]) > MAX_CHROME_ITEMS:
            errors.append(error(line, f"header r has more than {MAX_CHROME_ITEMS} items"))
        else:
            for i, item in enumerate(obj["r"]):
                if not isinstance(item, dict) or ("t" not in item and "p" not in item):
                    errors.append(error(line, f"header r[{i}] must have t or p"))
                    continue
                if "t" in item:
                    check_string(item["t"], f"header r[{i}].t", MAX_CHROME, line, errors)
                if "p" in item:
                    pill = item["p"]
                    if not isinstance(pill, list) or len(pill) < 2:
                        errors.append(error(line, f"header r[{i}].p must be [label, value]"))
                    else:
                        check_string(pill[0], f"header r[{i}].p[0]", MAX_PILL, line, errors)
                        check_string(pill[1], f"header r[{i}].p[1]", MAX_PILL, line, errors)
    return q, o, dur


def validate_channel(obj: dict, line: int, q: int, o: int, dur: int, names: set[str], errors: list[str]):
    name = obj.get("n")
    if not isinstance(name, str) or not name:
        errors.append(error(line, "channel is missing n"))
        return
    check_string(name, "channel n", MAX_NAME, line, errors)
    if name in names:
        errors.append(error(line, f"duplicate channel name {name}"))
    names.add(name)
    if obj.get("k") not in (None, "c"):
        errors.append(error(line, f"channel {name} has invalid k"))
    if not is_vis(obj.get("vis")):
        errors.append(error(line, f"channel {name} is missing vis"))
    hz = obj.get("hz")
    if not isinstance(hz, (int, float)) or isinstance(hz, bool):
        errors.append(error(line, f"channel {name} is missing hz"))
        return
    period = period_ns(float(hz))
    if period is None:
        errors.append(error(line, f"channel {name} has a non-positive hz"))
        return
    if period % q != 0:
        errors.append(error(line, f"channel {name} period {period} is not a multiple of q={q}"))
    t0 = obj.get("t0", o)
    if not isinstance(t0, int) or isinstance(t0, bool):
        errors.append(error(line, f"channel {name} t0 must be an integer"))
        return
    if not on_lattice(t0, o, q):
        errors.append(error(line, f"channel {name} t0 is not on the time lattice"))
    values = obj.get("v")
    if not isinstance(values, list) or not values:
        errors.append(error(line, f"channel {name} is missing v"))
        return
    for i, value in enumerate(values):
        if value is None:
            continue
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            errors.append(error(line, f"channel {name} values must be numbers or null"))
            break
        if isinstance(value, float) and not math.isfinite(value):
            errors.append(error(line, f"channel {name} has a non-finite value"))
            break
    end = t0 + len(values) * period
    if end > dur + period:
        errors.append(error(line, f"channel {name} extends beyond dur + one period"))
    plot = obj.get("plt", "trace")
    if plot not in (None, "trace", "t", "gauge", "g", "compass", "c"):
        errors.append(error(line, f"channel {name} plt must be trace, gauge, or compass"))
        plot = "trace"
    is_trace = plot in (None, "trace", "t")
    sc = obj.get("sc")
    if sc is not None:
        if not isinstance(sc, list) or not sc:
            errors.append(error(line, f"channel {name} sc must be [min, max] numbers"))
        else:
            nums = []
            for part in sc[:2]:
                if part is None:
                    nums.append(None)
                elif isinstance(part, bool) or not isinstance(part, (int, float)):
                    errors.append(error(line, f"channel {name} sc must be [min, max] numbers"))
                    nums = None
                    break
                else:
                    nums.append(float(part))
            if nums and nums[0] is not None and len(nums) > 1 and nums[1] is not None:
                if nums[0] >= nums[1]:
                    errors.append(error(line, f"channel {name} sc min must be less than max"))
    rnd = obj.get("rnd")
    if rnd is not None and (
        isinstance(rnd, bool) or not isinstance(rnd, int) or rnd < 0 or rnd > 15
    ):
        errors.append(error(line, f"channel {name} rnd must be an integer 0..=15"))
    unit = obj.get("u")
    if unit is not None:
        check_string(unit, f"channel {name} u", MAX_UNIT, line, errors)
    fmt = obj.get("fmt")
    if fmt is not None:
        if not isinstance(fmt, str):
            errors.append(error(line, f"channel {name} fmt must be a string"))
        elif any(ch.isspace() for ch in fmt):
            errors.append(
                error(line, f"channel {name} fmt must not contain whitespace (use 0.0°C not '0.0 °C')")
            )
        else:
            check_string(fmt, f"channel {name} fmt", MAX_FMT, line, errors)
    labels = obj.get("lbl")
    if labels is None:
        return
    if not is_trace:
        errors.append(error(line, f"channel {name} labels are only allowed on plt=trace"))
        return
    if not isinstance(labels, list):
        errors.append(error(line, f"channel {name} lbl must be an array"))
        return
    if len(labels) > MAX_LABELS:
        errors.append(error(line, f"channel {name} lbl has more than {MAX_LABELS} items"))
    prev = None
    for i, row in enumerate(labels):
        if not isinstance(row, list) or len(row) < 2:
            errors.append(error(line, f"channel {name} lbl[{i}] must be [ns, text]"))
            continue
        time, text = row[0], row[1]
        if not isinstance(time, int) or isinstance(time, bool):
            errors.append(error(line, f"channel {name} label time must be an integer"))
            continue
        if not on_lattice(time, o, q):
            errors.append(error(line, f"channel {name} label time is not on the time lattice"))
        if prev is not None and time <= prev:
            errors.append(error(line, f"channel {name} labels must be in increasing time order"))
        prev = time
        if not isinstance(text, str) or not text:
            errors.append(error(line, f"channel {name} label text must be non-empty"))
        else:
            check_string(text, f"channel {name} lbl[{i}] text", MAX_LABEL, line, errors)


def validate_span(obj: dict, line: int, q: int, o: int, errors: list[str]):
    if not is_vis(obj.get("vis")):
        errors.append(error(line, "span is missing vis"))
    span_name = obj.get("n")
    if span_name is not None:
        check_string(span_name, "span n", MAX_NAME, line, errors)
    start, end = obj.get("s"), obj.get("e")
    if not isinstance(start, int) or isinstance(start, bool):
        errors.append(error(line, "span is missing s"))
        return
    if not isinstance(end, int) or isinstance(end, bool):
        errors.append(error(line, "span is missing e"))
        return
    if end <= start:
        errors.append(error(line, "span end must be greater than start"))
    if not on_lattice(start, o, q) or not on_lattice(end, o, q):
        errors.append(error(line, "span boundary is not on the time lattice"))
    color = obj.get("c")
    if color is not None:
        if not isinstance(color, str) or not color.startswith("#") or len(color) != 7:
            errors.append(error(line, "span color must be #RRGGBB"))
        elif any(c not in "0123456789abcdefABCDEF" for c in color[1:]):
            errors.append(error(line, "span color must be #RRGGBB"))
    primary = obj.get("p")
    if isinstance(primary, dict):
        if "title" in primary:
            check_string(primary["title"], "span p.title", MAX_TITLE, line, errors)
        if "sub" in primary:
            check_string(primary["sub"], "span p.sub", MAX_SUB, line, errors)
    meta = obj.get("m")
    if meta is not None:
        if not isinstance(meta, list):
            errors.append(error(line, "span m must be an array of [name, value]"))
        elif len(meta) > MAX_META_ITEMS:
            errors.append(error(line, f"span m has more than {MAX_META_ITEMS} items"))
        else:
            for i, row in enumerate(meta):
                if not isinstance(row, list) or len(row) < 2 or not isinstance(row[0], str):
                    errors.append(error(line, "each meta entry must be [name, value]"))
                    break
                check_string(row[0], f"span m[{i}] name", MAX_META_NAME, line, errors)
                validate_meta_value(row[1], line, i, errors)


def validate_meta_value(value, line: int, index: int, errors: list[str]) -> None:
    if isinstance(value, str):
        check_string(value, f"span m[{index}] value", MAX_META_VALUE, line, errors)
        return
    if isinstance(value, bool):
        errors.append(error(line, f"span m[{index}] value cannot be a boolean"))
        return
    if isinstance(value, int):
        if value < 0 or value > TIMESPAN_MS_MAX:
            errors.append(error(line, f"span m[{index}] timespan_ms exceeds 100 hours"))
        return
    if isinstance(value, dict):
        unit = value.get("u")
        if unit not in TIMESPAN_UNITS:
            errors.append(error(line, f"span m[{index}] typed value u must be timespan_ms"))
        raw = value.get("v")
        if isinstance(raw, bool) or not isinstance(raw, int):
            errors.append(error(line, f"span m[{index}] timespan_ms v must be an integer"))
        elif raw < 0 or raw > TIMESPAN_MS_MAX:
            errors.append(error(line, f"span m[{index}] timespan_ms exceeds 100 hours"))
        return
    errors.append(error(line, "each meta value must be a string, millisecond integer, or {v,u}"))


def self_check_schema(schema: dict, validator_cls) -> list[str]:
    """Validate every $defs.*.examples instance against that definition."""
    errors: list[str] = []
    defs = schema.get("$defs", {})
    for name, spec in defs.items():
        if not isinstance(spec, dict):
            continue
        examples = spec.get("examples")
        if not examples:
            continue
        validator = validator_cls(ref_schema(schema, name), format_checker=None)
        for i, example in enumerate(examples):
            for err in validator.iter_errors(example):
                path = "/".join(str(p) for p in err.path)
                where = f" ({path})" if path else ""
                errors.append(f"$defs.{name}.examples[{i}]{where}: {err.message}")
    return errors


def validate_text(text: str, schema: dict | None, validator_cls) -> list[str]:
    errors: list[str] = []
    if text.startswith("\ufeff"):
        errors.append(error(1, "UTF-8 BOM is not allowed"))
        text = text.lstrip("\ufeff")
    if not text.endswith("\n"):
        errors.append("document must end with a trailing LF")
    lines = text.splitlines()
    if not lines:
        errors.append(error(1, "empty document"))
        return errors
    for i, line in enumerate(lines, 1):
        if line == "":
            errors.append(error(i, "blank lines are not allowed"))
        elif has_insignificant_ws(line):
            errors.append(error(i, "insignificant whitespace is not allowed"))
    header_line = lines[0]
    try:
        header = json.loads(header_line)
    except json.JSONDecodeError as err:
        errors.append(error(1, f"header is not JSON: {err}"))
        return errors
    if schema is not None and validator_cls is not None:
        validate_schema("header", header, ref_schema(schema, "mtx_header"), validator_cls, errors, 1)
    lattice = validate_header(header if isinstance(header, dict) else {}, 1, errors)
    if lattice is None:
        return errors
    q, o, dur = lattice
    names: set[str] = set()
    for i, line in enumerate(lines[1:], 2):
        if line == "":
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError as err:
            errors.append(error(i, f"record is not JSON: {err}"))
            continue
        if not isinstance(obj, dict):
            errors.append(error(i, "record must be a JSON object"))
            continue
        if "mtx" in obj:
            if schema is not None and validator_cls is not None:
                validate_schema("header", obj, ref_schema(schema, "mtx_header"), validator_cls, errors, i)
            next_lattice = validate_header(obj, i, errors)
            if next_lattice is not None:
                q, o, dur = next_lattice
            continue
        if "mtj" in obj:
            errors.append(error(i, "mtj header is only allowed on the first line"))
            continue
        kind = obj.get("k")
        if kind == "f":
            errors.append(error(i, "folder records are not used"))
            continue
        if kind == "s":
            if schema is not None and validator_cls is not None:
                validate_schema("span", obj, ref_schema(schema, "span"), validator_cls, errors, i)
            validate_span(obj, i, q, o, errors)
        elif kind in (None, "c"):
            if schema is not None and validator_cls is not None:
                validate_schema("channel", obj, ref_schema(schema, "channel"), validator_cls, errors, i)
            validate_channel(obj, i, q, o, dur, names, errors)
        else:
            errors.append(error(i, f"unknown record kind {kind!r}"))
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument(
        "path",
        nargs="?",
        type=Path,
        help="MTX sidecar (.telemetry.ext.jsonl or .zstd)",
    )
    parser.add_argument("--schema", type=Path, help="Override telemetry.schema.json")
    parser.add_argument(
        "--self-check",
        action="store_true",
        help="Validate every examples[] value in the schema against its $defs entry",
    )
    args = parser.parse_args(argv)
    schema_file = args.schema or schema_path()
    schema = json.loads(schema_file.read_text())
    ref = schema.get("$ref")
    if "$defs" not in schema and isinstance(ref, str):
        target = (schema_file.parent / ref.split("#", 1)[0]).resolve()
        if target.is_file():
            schema = json.loads(target.read_text())
    loaded = load_jsonschema()
    validator_cls = loaded[1] if loaded else None
    if args.self_check:
        if validator_cls is None:
            print("jsonschema is required for --self-check", file=sys.stderr)
            return 2
        errors = self_check_schema(schema, validator_cls)
        if errors:
            for item in errors:
                print(item, file=sys.stderr)
            print(f"{schema_file}: {len(errors)} example error(s)", file=sys.stderr)
            return 1
        print(f"{schema_file}: examples ok")
        if args.path is None:
            return 0
    if args.path is None:
        parser.error("path is required unless --self-check is set")
    if not args.path.is_file():
        print(f"not a file: {args.path}", file=sys.stderr)
        return 2
    text = read_document(args.path).decode("utf-8")
    errors = validate_text(text, schema, validator_cls)
    if errors:
        for item in errors:
            print(item, file=sys.stderr)
        print(f"{args.path}: {len(errors)} error(s)", file=sys.stderr)
        return 1
    extra = "" if validator_cls else " (install jsonschema for Draft 2020-12 checks)"
    print(f"{args.path}: ok{extra}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
