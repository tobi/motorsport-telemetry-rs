#!/usr/bin/env python3
"""Generate small deterministic fixtures for every supported telemetry format."""
from __future__ import annotations

import json
import math
import struct
import sys
from pathlib import Path

SAMPLES = {
    "speed": (10.0, 11.0, 12.0, 13.0),
    "throttle": (0.0, 50.0, 100.0, 25.0),
    "brake": (0.0, 10.0, 40.0, 5.0),
    "g_lat": (0.0, 0.2, -0.3, 0.1),
    "g_long": (0.0, -0.5, 0.1, 0.2),
    "distance": (0.0, 10.0, 20.0, 30.0),
    "lap": (1.0, 1.0, 1.0, 1.0),
    "latitude": (43.7978, 43.7979, 43.7980, 43.7981),
    "longitude": (-87.9899, -87.9898, -87.9897, -87.9896),
}


def mp4_box(kind: bytes, payload: bytes) -> bytes:
    return struct.pack(">I4s", len(payload) + 8, kind) + payload


def channel_definition(record_id: int, name: str, width: int) -> bytes:
    payload = bytearray(112)
    struct.pack_into("<I", payload, 0, record_id)
    encoded = name.encode("ascii")
    payload[24 : 24 + len(encoded)] = encoded
    payload[32 : 32 + len(encoded)] = encoded
    struct.pack_into("<I", payload, 72, width)
    return bytes(payload)


def packet_header(signature: bytes) -> bytearray:
    return bytearray(b"\x00\x00\x40\x00\x00\x00" + signature)


def make_aimd(itow_ms: int = 573_634_560, driver_id: float = 3.0, lap_number: float = 1.0) -> bytes:
    schema = packet_header(b"amv0s1")
    for record_id, name, width in (
        (42, "RPM", 4),
        (43, "DRIVER_ID", 4),
        (44, "Lap_Number", 4),
        (55, "GPS0", 56),
    ):
        definition = channel_definition(record_id, name, width)
        schema.extend(b"<hCHS\x00")
        schema.extend(struct.pack("<I", len(definition)))
        schema.extend(b"\x01>")
        schema.extend(definition)
    struct.pack_into(">H", schema, 0, len(schema) - 2)

    values = packet_header(b"amv0")
    values.extend(b"(S")
    values.extend(struct.pack("<I", 100))
    values.extend(struct.pack("<H", 42))
    values.extend(struct.pack("<f", 1234.5))
    values.extend(b")")
    for record_id, value in ((43, driver_id), (44, lap_number)):
        values.extend(b"(S")
        values.extend(struct.pack("<I", 100))
        values.extend(struct.pack("<H", record_id))
        values.extend(struct.pack("<f", value))
        values.extend(b")")
    gps = bytearray(56)
    struct.pack_into("<I", gps, 0, 100)
    struct.pack_into("<I", gps, 4, itow_ms)
    struct.pack_into("<H", gps, 12, 2429)
    struct.pack_into("<3i", gps, 16, 16_174_352, -460_842_617, 439_210_627)
    struct.pack_into("<I", gps, 28, 783)
    struct.pack_into("<3i", gps, 32, 5, -10, 8)
    struct.pack_into("<I", gps, 44, 6)
    gps[14] = 3  # u-blox NAV-SOL gpsFix: 3D
    gps[15] = 1  # u-blox NAV-SOL flags: GPSfixOK
    struct.pack_into("<H", gps, 48, 248)
    gps[51] = 9
    values.extend(b"<hGPS\x00")
    values.extend(struct.pack("<I", len(gps)))
    values.extend(b"\x01>")
    values.extend(gps)
    struct.pack_into(">H", values, 0, len(values) - 2)

    ftyp = mp4_box(b"ftyp", b"isom\x00\x00\x00\x00isom")
    mdat = mp4_box(b"mdat", bytes(schema) + bytes(values))
    chunk_offset = len(ftyp) + 8
    mdhd = bytearray(24)
    struct.pack_into(">I", mdhd, 12, 1000)
    hdlr = bytearray(24)
    hdlr[8:12] = b"meta"
    stsd = bytearray(16)
    stsd[7] = 1
    struct.pack_into(">I", stsd, 8, 8)
    stsd[12:16] = b"aimd"
    stts = bytearray(16)
    stts[7] = 1
    struct.pack_into(">II", stts, 8, 2, 100)
    stsc = bytearray(20)
    stsc[7] = 1
    struct.pack_into(">III", stsc, 8, 1, 2, 1)
    stsz = bytearray(20)
    struct.pack_into(">II", stsz, 8, 2, len(schema))
    struct.pack_into(">I", stsz, 16, len(values))
    stco = bytearray(12)
    stco[7] = 1
    struct.pack_into(">I", stco, 8, chunk_offset)
    stbl = mp4_box(
        b"stbl",
        b"".join(
            mp4_box(kind, payload)
            for kind, payload in (
                (b"stsd", stsd),
                (b"stts", stts),
                (b"stsc", stsc),
                (b"stsz", stsz),
                (b"stco", stco),
            )
        ),
    )
    minf = mp4_box(b"minf", stbl)
    mdia = mp4_box(b"mdia", mp4_box(b"mdhd", mdhd) + mp4_box(b"hdlr", hdlr) + minf)
    movie_timescale = 1000
    empty_edit_ms = 104
    aimd_elst = bytearray(32)
    aimd_elst[7] = 2
    struct.pack_into(">IiI", aimd_elst, 8, empty_edit_ms, -1, 0x00010000)
    struct.pack_into(">IiI", aimd_elst, 20, 200, 0, 0x00010000)
    aimd_trak = mp4_box(b"trak", mp4_box(b"edts", mp4_box(b"elst", aimd_elst)) + mdia)

    video_mdhd = bytearray(24)
    struct.pack_into(">I", video_mdhd, 12, 1000)
    video_hdlr = bytearray(24)
    video_hdlr[8:12] = b"vide"
    video_stts = bytearray(16)
    video_stts[7] = 1
    video_stsd = bytearray(16)
    video_stsd[7] = 1
    struct.pack_into(">I", video_stsd, 8, 8)
    video_stsd[12:16] = b"avc1"
    struct.pack_into(">II", video_stts, 8, 3, 40)
    video_stbl = mp4_box(
        b"stbl", mp4_box(b"stsd", video_stsd) + mp4_box(b"stts", video_stts)
    )
    video_minf = mp4_box(b"minf", video_stbl)
    video_mdia = mp4_box(
        b"mdia",
        mp4_box(b"mdhd", video_mdhd) + mp4_box(b"hdlr", video_hdlr) + video_minf,
    )
    video_trak = mp4_box(b"trak", video_mdia)
    mvhd = bytearray(24)
    struct.pack_into(">I", mvhd, 12, movie_timescale)
    moov = mp4_box(b"moov", mp4_box(b"mvhd", mvhd) + video_trak + aimd_trak)
    return ftyp + mdat + moov


ATLAS = Path(__file__).resolve().parents[2] / "crates/motorsport-track-atlas/data/tracks.jsonl"
PDS_HZ = 5
PDS_TICKS = 2_000_000  # 5 Hz: 2e6 ticks × 100 ns
EARTH_M = 6_371_000.0


def load_road_america() -> tuple[list[tuple[float, float]], float, list[tuple[float, float]]]:
    """Centerline (lat, lon), length m, and (marker, apex_speed) hints."""
    track = None
    for line in ATLAS.read_text().splitlines():
        if '"slug":"road-america"' in line:
            track = json.loads(line)
            break
    if track is None:
        raise SystemExit("road-america missing from motorsport-track-atlas/data/tracks.jsonl")
    layout = track["layouts"][0]
    coords = layout["centerline_geojson"]["features"][0]["geometry"]["coordinates"]
    if coords[0] == coords[-1]:
        coords = coords[:-1]
    points = [(lat, lon) for lon, lat in coords]
    # Slowest legal apex speeds for a mid-pack GT/LMP2-ish car (m/s).
    apex = {
        0.099: 32.0,  # T1 after the front straight
        0.1725: 38.0,  # T3
        0.2775: 55.0,  # Moraine Sweep
        0.3525: 36.0,  # T5
        0.4025: 34.0,  # T6
        0.44: 40.0,  # T7
        0.4995: 42.0,  # T8
        0.5725: 40.0,  # Carousel
        0.66: 62.0,  # Kink
        0.735: 50.0,  # Kettle Bottoms
        0.7905: 28.0,  # Canada Corner
        0.8395: 36.0,  # Bill Mitchell
        0.8905: 38.0,  # T14 onto the straight
    }
    return points, 6514.0, sorted(apex.items())


def haversine_m(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dphi = math.radians(lat2 - lat1)
    dl = math.radians(lon2 - lon1)
    a = math.sin(dphi / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * EARTH_M * math.atan2(math.sqrt(a), math.sqrt(1 - a))


def heading_rad(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dl = math.radians(lon2 - lon1)
    y = math.sin(dl) * math.cos(p2)
    x = math.cos(p1) * math.sin(p2) - math.sin(p1) * math.cos(p2) * math.cos(dl)
    return math.atan2(y, x)


def unwrap(angles: list[float]) -> list[float]:
    out = [angles[0]]
    for angle in angles[1:]:
        prev = out[-1]
        delta = (angle - prev + math.pi) % (2 * math.pi) - math.pi
        out.append(prev + delta)
    return out


def road_america_limit() -> tuple[list[float], list[float], list[float], list[float]]:
    """Progress samples with GPS and a curvature-limited target speed."""
    points, length_m, apexes = load_road_america()
    n = len(points)
    dist = [0.0]
    heads = []
    for i in range(n):
        a, b = points[i], points[(i + 1) % n]
        dist.append(dist[-1] + haversine_m(a[0], a[1], b[0], b[1]))
        heads.append(heading_rad(a[0], a[1], b[0], b[1]))
    total = dist[-1]
    heads = unwrap(heads + [heads[0]])
    kappa = []
    for i in range(n):
        ds = max(dist[i + 1] - dist[i], 1.0)
        kappa.append(abs(heads[i + 1] - heads[i]) / ds)
    # Smooth one-two neighbours so a single jagged vertex is not a hairpin.
    smooth = []
    for i in range(n):
        window = [kappa[(i + j) % n] for j in (-2, -1, 0, 1, 2)]
        smooth.append(sorted(window)[2])
    a_lat = 11.5
    v_max, v_min = 72.0, 20.0
    limit = []
    progress = []
    lats = []
    lons = []
    for i in range(n):
        p = dist[i] / total
        v = math.sqrt(a_lat / max(smooth[i], 1e-4))
        for marker, apex_v in apexes:
            width = 0.018
            fall = max(0.0, 1.0 - abs(p - marker) / width)
            if fall > 0:
                v = min(v, apex_v + (v_max - apex_v) * (1.0 - fall) ** 2)
        limit.append(max(v_min, min(v_max, v)))
        progress.append(p)
        lats.append(points[i][0])
        lons.append(points[i][1])
    # Closed loop for wrap-around braking.
    progress.append(1.0)
    limit.append(limit[0])
    lats.append(lats[0])
    lons.append(lons[0])
    # Forward / backward speed pass (accel 5.5, brake 11 m/s²).
    v = limit[:]
    for i in range(n):
        ds = (progress[i + 1] - progress[i]) * length_m
        v[i + 1] = min(v[i + 1], math.sqrt(v[i] ** 2 + 2 * 5.5 * ds))
    for i in range(n - 1, -1, -1):
        ds = (progress[i + 1] - progress[i]) * length_m
        v[i] = min(v[i], math.sqrt(v[i + 1] ** 2 + 2 * 11.0 * ds))
    return progress, v, lats, lons


def interp_loop(progress: list[float], values: list[float], p: float) -> float:
    p %= 1.0
    for i in range(len(progress) - 1):
        if progress[i] <= p <= progress[i + 1]:
            span = progress[i + 1] - progress[i]
            t = 0.0 if span <= 0 else (p - progress[i]) / span
            return values[i] + (values[i + 1] - values[i]) * t
    return values[0]


def simulate_stint() -> dict[str, list[float]]:
    """Out-lap from pit exit, three flying laps, in-lap to pit entry. 5 Hz."""
    progress, v_limit, lats, lons = road_america_limit()
    length_m = 6514.0
    dt = 1.0 / PDS_HZ
    # (lap_number, p0, p1, speed_scale, crawl_after)
    legs = [
        (1.0, 0.056, 1.0, 0.70, None),  # out, from pit exit
        (2.0, 0.0, 1.0, 1.00, None),
        (3.0, 0.0, 1.0, 0.985, None),
        (4.0, 0.0, 1.0, 0.970, None),
        (5.0, 0.0, 0.94, 0.82, 0.88),  # in, peel off toward pits
    ]
    speed: list[float] = []
    throttle: list[float] = []
    brake: list[float] = []
    g_lat: list[float] = []
    g_long: list[float] = []
    distance: list[float] = []
    laps: list[float] = []
    driver: list[float] = []
    latitude: list[float] = []
    longitude: list[float] = []
    prev_v = 12.0
    for lap_number, p0, p1, scale, crawl_after in legs:
        p = p0
        lap_s = 0.0
        while p < p1 - 1e-6:
            target = interp_loop(progress, v_limit, p) * scale
            if crawl_after is not None and p >= crawl_after:
                fade = min(1.0, (p - crawl_after) / 0.04)
                target = target * (1.0 - fade) + 8.0 * fade
            # First-order chase of the target so throttle/brake are not binary.
            if target > prev_v:
                v = min(target, prev_v + 5.5 * dt)
            else:
                v = max(target, prev_v - 11.0 * dt)
            a_long = (v - prev_v) / dt
            ds = max(v, 4.0) * dt
            # Finite-difference heading over a short lookahead for lateral g.
            h0 = heading_rad(
                interp_loop(progress, lats, p),
                interp_loop(progress, lons, p),
                interp_loop(progress, lats, p + 0.002),
                interp_loop(progress, lons, p + 0.002),
            )
            h1 = heading_rad(
                interp_loop(progress, lats, p + 0.002),
                interp_loop(progress, lons, p + 0.002),
                interp_loop(progress, lats, p + 0.004),
                interp_loop(progress, lons, p + 0.004),
            )
            dpsi = (h1 - h0 + math.pi) % (2 * math.pi) - math.pi
            kappa = abs(dpsi) / max(haversine_m(
                interp_loop(progress, lats, p),
                interp_loop(progress, lons, p),
                interp_loop(progress, lats, p + 0.002),
                interp_loop(progress, lons, p + 0.002),
            ), 1.0)
            speed.append(round(v, 4))
            if a_long >= 0.4:
                throttle.append(round(min(100.0, 25.0 + a_long / 5.5 * 75.0), 2))
                brake.append(0.0)
            elif a_long <= -0.8:
                throttle.append(0.0)
                brake.append(round(min(100.0, (-a_long) / 11.0 * 100.0), 2))
            else:
                throttle.append(18.0 if target < 40 else 55.0)
                brake.append(0.0)
            g_lat.append(round(v * v * kappa * (1 if dpsi >= 0 else -1), 4))
            g_long.append(round(a_long, 4))
            distance.append(round(lap_s, 3))
            laps.append(lap_number)
            driver.append(7.0)
            latitude.append(round(interp_loop(progress, lats, p), 7))
            longitude.append(round(interp_loop(progress, lons, p), 7))
            lap_s += ds
            p += ds / length_m
            prev_v = v
        prev_v = min(prev_v, 35.0)
    return {
        "speed": speed,
        "throttle": throttle,
        "brake": brake,
        "g_lat": g_lat,
        "g_long": g_long,
        "distance": distance,
        "laps": laps,
        "driver": driver,
        "latitude": latitude,
        "longitude": longitude,
    }


def make_pds() -> bytes:
    series = simulate_stint()
    count = len(series["speed"])
    channels = [
        (1, "Speed", "m/s", series["speed"]),
        (2, "Throttle Pos", "%", series["throttle"]),
        (3, "Brake Pedal Pos", "%", series["brake"]),
        (4, "G_FORCE_LAT", "m/s^2", series["g_lat"]),
        (5, "G_FORCE_LONG", "m/s^2", series["g_long"]),
        (6, "Lap Distance", "m", series["distance"]),
        (7, "Lap Number", "count", series["laps"]),
        (8, "Driver ID", "count", series["driver"]),
        (9, "GPS Latitude", "deg", series["latitude"]),
        (10, "GPS Longitude", "deg", series["longitude"]),
    ]
    definition_width = 0xC0
    defs = 0x200
    chunks = defs + definition_width * len(channels)
    chunk_width = 0x40
    chunk_count = len(channels)
    end = chunks + chunk_width * chunk_count
    data_start = 0x1000
    data = bytearray(data_start + 8 * count * len(channels) + 0x40)
    data[0x40:0x80] = b"\xff" * 0x40

    def u32(offset: int, value: int) -> None:
        struct.pack_into("<I", data, offset, value)

    def directory(offset: int, section: int, count: int, class_a: int, class_b: int, next_count: int) -> None:
        u32(offset, section)
        u32(offset + 8, count)
        u32(offset + 0x10, class_a)
        u32(offset + 0x14, class_b)
        u32(offset + 0x18, next_count)

    def definition(offset: int, channel_id: int, name: str, unit: str) -> None:
        u32(offset, channel_id)
        name_bytes = name.encode("utf-16le")
        unit_bytes = unit.encode("utf-16le")
        data[offset + 8 : offset + 8 + len(name_bytes)] = name_bytes
        data[offset + 0x90 : offset + 0x90 + len(unit_bytes)] = unit_bytes

    def chunk(offset: int, order: int, channel_id: int, pointer: int, samples: int) -> None:
        u32(offset, order)
        u32(offset + 4, channel_id)
        u32(offset + 8, channel_id)
        u32(offset + 0x18, PDS_TICKS)
        u32(offset + 0x1C, samples)
        u32(offset + 0x38, pointer)

    directory(0x80, defs, len(channels), 8, 1, chunk_count)
    directory(0xA0, chunks, chunk_count, 1, 3, 0)
    directory(0xC0, end, 0, 1, 1, 0)
    pointer = data_start
    for index, (channel_id, name, unit, values) in enumerate(channels):
        definition(defs + index * definition_width, channel_id, name, unit)
        chunk(chunks + index * chunk_width, index + 1, channel_id, pointer, count)
        struct.pack_into(f"<{count}d", data, pointer, *values)
        pointer += 8 * count
    return bytes(data)


def make_motec_channels(
    channels: list[tuple[str, str, int, tuple[float, ...] | list[float]]],
) -> bytes:
    block_size = 124
    first_block = 0x200
    data_start = first_block + block_size * len(channels) + 0x40
    data = bytearray(data_start + sum(4 * len(values) for _, _, _, values in channels))
    struct.pack_into("<I", data, 0, 0x40)
    struct.pack_into("<I", data, 0x08, first_block)
    pointer = data_start
    for index, (name, unit, frequency, values) in enumerate(channels):
        block = first_block + index * block_size
        next_block = block + block_size if index + 1 < len(channels) else 0
        struct.pack_into("<I", data, block + 4, next_block)
        struct.pack_into("<I", data, block + 8, pointer)
        struct.pack_into("<I", data, block + 0x0C, len(values))
        struct.pack_into("<H", data, block + 0x12, 0x07)
        struct.pack_into("<H", data, block + 0x14, 4)
        struct.pack_into("<H", data, block + 0x16, frequency)
        encoded_name = name.encode("ascii")
        encoded_unit = unit.encode("ascii")
        data[block + 0x20 : block + 0x20 + len(encoded_name)] = encoded_name
        data[block + 0x48 : block + 0x48 + len(encoded_unit)] = encoded_unit
        struct.pack_into(f"<{len(values)}f", data, pointer, *values)
        pointer += 4 * len(values)
    return bytes(data)


def make_motec() -> bytes:
    return make_motec_channels([
        ("Speed", "m/s", 2, SAMPLES["speed"]),
        ("Throttle Pos", "%", 2, SAMPLES["throttle"]),
        ("Brake Pedal Pos", "%", 2, SAMPLES["brake"]),
        ("G_FORCE_LAT", "m/s^2", 2, SAMPLES["g_lat"]),
        ("G_FORCE_LONG", "m/s^2", 2, SAMPLES["g_long"]),
        ("Lap Distance", "m", 2, SAMPLES["distance"]),
        ("Lap Number", "count", 2, SAMPLES["lap"]),
        ("GPS Latitude", "deg", 2, SAMPLES["latitude"]),
        ("GPS Longitude", "deg", 2, SAMPLES["longitude"]),
    ])


# These rates and channel roles mirror aggregate structure seen in the local
# reference corpus. Durations and every sample value below are invented.
MULTILAP_DURATIONS_SECONDS = (
    12.5, 13.0, 11.5, 14.0, 12.0, 15.0,
    11.0, 13.5, 12.5, 14.0, 13.0, 9.0,
)


def multilap_segment(time_seconds: float) -> tuple[int, float]:
    start = 0.0
    for index, duration in enumerate(MULTILAP_DURATIONS_SECONDS):
        end = start + duration
        if time_seconds < end or index + 1 == len(MULTILAP_DURATIONS_SECONDS):
            return index, start
        start = end
    raise AssertionError("unreachable")


def make_motec_multilap() -> bytes:
    high_rate = 100
    low_rate = 2
    total_seconds = sum(MULTILAP_DURATIONS_SECONDS)
    high_times = [index / high_rate for index in range(round(total_seconds * high_rate))]
    low_times = [index / low_rate for index in range(round(total_seconds * low_rate))]

    speed = []
    progression = []
    for time_seconds in high_times:
        segment, start = multilap_segment(time_seconds)
        duration = MULTILAP_DURATIONS_SECONDS[segment]
        progress = 100.0 * (time_seconds - start) / duration
        progression.append(progress)
        # A deterministic triangle wave keeps the high-rate payload nontrivial.
        speed.append(42.0 + 0.18 * min(progress, 100.0 - progress))

    lap_count = []
    previous_lap_time = []
    invalidated = []
    for time_seconds in low_times:
        segment, _ = multilap_segment(time_seconds)
        active_lap = 4 + segment
        # Model a logger shutdown reset followed by a low-count restart. The
        # high-water fallback must not turn 0 -> 1 into a new lap crossing.
        if time_seconds >= total_seconds - 1.0:
            active_lap = 1
        elif time_seconds >= total_seconds - 2.0:
            active_lap = 0
        lap_count.append(float(active_lap))
        previous_lap_time.append(
            0.0 if segment == 0 else MULTILAP_DURATIONS_SECONDS[segment - 1]
        )
        invalidated.append(float(segment == 4))

    return make_motec_channels([
        ("Speed", "m/s", high_rate, speed),
        ("Lap Progression", "%", high_rate, progression),
        ("Lap Count", "", low_rate, lap_count),
        ("Lap Invalidated", "", low_rate, invalidated),
        ("Lap Length", "m", low_rate, [5000.0] * len(low_times)),
        ("Previous Lap Time", "s", low_rate, previous_lap_time),
        ("Reference Lap Time", "s", low_rate, [12.5] * len(low_times)),
    ])


def make_motec_multilap_ldx() -> bytes:
    marker_times = []
    elapsed = 0.0
    for duration in MULTILAP_DURATIONS_SECONDS[:-1]:
        elapsed += duration
        marker_times.append(elapsed)
    markers = "\n".join(
        f'     <Marker Version="100" ClassName="BCN" Name="Manual.{index}" '
        f'Flags="77" Time="{seconds * 1_000_000:.17e}"/>'
        for index, seconds in enumerate(marker_times, 1)
    )
    return f'''<?xml version="1.0"?>
<LDXFile Locale="English_United States.1252" DefaultLocale="C" Version="1.6">
 <Layers>
  <Layer>
   <MarkerBlock>
    <MarkerGroup Name="Beacons" Index="3">
{markers}
    </MarkerGroup>
   </MarkerBlock>
  </Layer>
  <Details>
   <String Id="Total Laps" Value="12"/>
   <String Id="Fastest Time" Value="0:11.000"/>
   <String Id="Fastest Lap" Value="7"/>
  </Details>
 </Layers>
</LDXFile>
'''.encode("ascii")


def make_vbo() -> bytes:
    columns = [
        "sats", "time", "lat", "long", "velocity", "heading", "height",
        "vert-vel", "Tsample", "solution_type", "avifileindex", "avitime",
        "throttle", "brake", "gforce_lat", "gforce_long", "distance", "lap",
    ]
    rows = []
    for index in range(4):
        rows.append([
            9, 120000.0 + index * 0.5, SAMPLES["latitude"][index],
            SAMPLES["longitude"][index], 10.0 + index * 10.0, 90,
            291.74, 0, 0.5, 3, 0, 0, SAMPLES["throttle"][index],
            SAMPLES["brake"][index], SAMPLES["g_lat"][index],
            SAMPLES["g_long"][index], SAMPLES["distance"][index],
            SAMPLES["lap"][index],
        ])
    units = ["%", "%", "m/s^2", "m/s^2", "m", "count"]
    return (("[column names]\n" + " ".join(columns) + "\n[channel units]\n")
            + "\n".join(units) + "\n[data]\n"
            + "\n".join(" ".join(str(value) for value in row) for row in rows)
            + "\n").encode()


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} OUTPUT_DIRECTORY")
    output = Path(sys.argv[1])
    output.mkdir(parents=True, exist_ok=True)
    files = {
        "synthetic_aimd.mp4": make_aimd(),
        "synthetic_aimd_part2.mp4": make_aimd(573_634_760, 3.0, 1.0),
        "synthetic_cosworth.pds": make_pds(),
        "synthetic_motec.ld": make_motec(),
        "synthetic_motec_multilap.ld": make_motec_multilap(),
        "synthetic_motec_multilap.ldx": make_motec_multilap_ldx(),
        "synthetic_vbo.vbo": make_vbo(),
    }
    for name, content in files.items():
        (output / name).write_bytes(content)
        print(f"{output / name}: {len(content)} bytes")


if __name__ == "__main__":
    main()
