#!/usr/bin/env python3
"""Decompose a spike glass run into per-stage latency medians.

Usage:
    decompose.py <glass.jsonl> <client.jsonl> <server.jsonl>

Joins three files taken from the same run:
  glass trials  (CoreMedia clock, Mac)   inject_pts_us, first_us
  client frames (CoreMedia clock, Mac)   recv/decode/present per frame, seq
  client rects  (CoreMedia clock, Mac)   recv/paint/present per rect message, seq
  server frames (QPC clock, quench)      present/acquire/convert/encode/send, frame
  server rects  (QPC clock, quench)      pack/send per rect message, frame

Wire v2 stamps the capture sequence number into every row on both ends, so client
and server rows join exactly on it. A v1 archive (client `seq` null) falls back to
the old arrival-order `au_bytes` join, which is order-fragile but was all v1 had.

The two clocks are never compared directly: cross-machine segments are derived as
residuals of same-clock spans, so no clock synchronisation is needed. The residual
"input+render+net" bundles Mac input dispatch, the tunnel hop for the 8-byte input
record, SendInput delivery, the target app's render, and DWM composition —
everything upstream of the pixel existing.

Each glass trial's detection event is whichever client paint — an H.264 frame or a
rect blit — was presented nearest the detected change. The two kinds decompose
through different server stages, so they are reported as separate tables; the
rect-vs-frame detection split is the fast-path hit rate the HLD §8 gate reads.

First used on the 2026-08-17 stage-2 run (baseline/spike-idd-stage2-*.jsonl); the
per-stage medians summed to ~56 ms against a measured 54.3 ms trial median.
"""
import json
import statistics
import sys

# A paint further than this from the detected change cannot be its cause.
DETECT_WINDOW_US = 25_000


def load(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def join_frames(client, server):
    """Pair client frame rows with server frame rows, exactly by seq where the
    stream carried one, else by the v1 arrival-order au_bytes walk."""
    if any(c.get("seq") is not None for c in client):
        # Server drain orphans stamp frame 0 (no matched submission); a real
        # capture seq starts at 1, so 0 must never join.
        by_seq = {s["frame"]: s for s in server if s["frame"] != 0}
        return [(c, by_seq[c["seq"]]) for c in client
                if c.get("seq") is not None and c["seq"] in by_seq]
    pairs = []
    si = 0
    for c in client:
        while si < len(server) and server[si]["au_bytes"] != c["au_bytes"]:
            si += 1
        if si == len(server):
            break
        pairs.append((c, server[si]))
        si += 1
    return pairs


def percentile_row(name, values):
    v = sorted(values)
    p50 = statistics.median(v) / 1000
    p95 = v[max(0, int(len(v) * 0.95) - 1)] / 1000
    return (f"{name:38s} {len(v):>4d} {p50:8.2f} {p95:8.2f}"
            f" {v[0]/1000:7.2f} {v[-1]/1000:8.2f}")


def print_stages(title, stages):
    """The n column is load-bearing, not decoration: in the rect table the
    server-derived stages exist only for detections whose server rows both
    survived (a dropped AU loses the frame row), so rows can legitimately
    carry different sample counts and must say so."""
    populated = {k: v for k, v in stages.items() if v}
    if not populated:
        return
    print(f"\n{title}")
    print(f"{'stage':38s} {'n':>4s} {'p50 ms':>8s} {'p95 ms':>8s} {'min':>7s} {'max':>8s}")
    for k, v in populated.items():
        print(percentile_row(k, v))


def main():
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    glass = [r for r in load(sys.argv[1])
             if r.get("type") == "trial" and not r["timed_out"] and not r["poisoned"]]
    client_rows = load(sys.argv[2])
    server_rows = load(sys.argv[3])
    client = [r for r in client_rows if r.get("type") == "frame"]
    client_rects = [r for r in client_rows if r.get("type") == "rects"]
    server = [r for r in server_rows if r.get("record") == "frame"]
    server_rects = {r["frame"]: r for r in server_rows if r.get("record") == "rects"}
    server_by_seq = {s["frame"]: s for s in server if s["frame"] != 0}

    pairs = join_frames(client, server)
    print(f"joined {len(pairs)}/{len(client)} client frames to server frames",
          file=sys.stderr)

    # Every presented client paint, whichever path produced it.
    events = [(c["present_done_us"], "frame", c, s)
              for c, s in pairs if c["present_done_us"] is not None]
    events += [(r["present_done_us"], "rect", r, None)
               for r in client_rects if r.get("present_done_us") is not None]

    frame_stages = {k: [] for k in [
        "input+render+net (residual)", "dwm present->dxgi acquire",
        "convert (bgra->nv12)", "encode (convert_end->out)", "server send",
        "recv->decode_in", "decode", "decode_out->present",
        "present->sck detect", "total first_us"]}
    rect_stages = {k: [] for k in [
        "input+render+net (residual)", "dwm present->dxgi acquire",
        "readback+pack (acquire->pack_end)", "rect send",
        "recv->paint", "paint->present",
        "present->sck detect", "total first_us"]}

    used = rect_hits = 0
    for t in glass:
        t_inj = t["inject_pts_us"]
        t_first = t_inj + t["first_us"]
        best = None
        for present_us, kind, c, s in events:
            d = abs(present_us - t_first)
            if d < DETECT_WINDOW_US and (best is None or d < best[0]):
                best = (d, kind, c, s)
        if best is None:
            continue
        _, kind, c, s = best
        used += 1

        if kind == "frame":
            server_span = s["send_done_us"] - s["present_qpc_us"]
            mac_span = c["recv_done_us"] - t_inj
            st = frame_stages
            st["input+render+net (residual)"].append(mac_span - server_span)
            st["dwm present->dxgi acquire"].append(s["acquire_qpc_us"] - s["present_qpc_us"])
            st["convert (bgra->nv12)"].append(s["convert_end_us"] - s["acquire_qpc_us"])
            st["encode (convert_end->out)"].append(s["encode_out_us"] - s["convert_end_us"])
            st["server send"].append(s["send_done_us"] - s["encode_out_us"])
            st["recv->decode_in"].append(c["decode_in_us"] - c["recv_done_us"])
            st["decode"].append(c["decode_out_us"] - c["decode_in_us"])
            st["decode_out->present"].append(c["present_done_us"] - c["decode_out_us"])
            st["present->sck detect"].append(t_first - c["present_done_us"])
            st["total first_us"].append(t["first_us"])
        else:
            rect_hits += 1
            sr = server_rects.get(c["seq"])
            sf = server_by_seq.get(c["seq"])
            st = rect_stages
            if sr is not None and sf is not None:
                # The rect message's server span starts where the frame's does —
                # the compositor present — so the residual stays comparable to
                # the frame path's.
                server_span = sr["send_done_us"] - sf["present_qpc_us"]
                st["input+render+net (residual)"].append(
                    c["recv_done_us"] - t_inj - server_span)
                st["dwm present->dxgi acquire"].append(
                    sf["acquire_qpc_us"] - sf["present_qpc_us"])
                st["readback+pack (acquire->pack_end)"].append(
                    sr["pack_end_us"] - sf["acquire_qpc_us"])
                st["rect send"].append(sr["send_done_us"] - sr["pack_end_us"])
            st["recv->paint"].append(c["paint_done_us"] - c["recv_done_us"])
            st["paint->present"].append(c["present_done_us"] - c["paint_done_us"])
            st["present->sck detect"].append(t_first - c["present_done_us"])
            st["total first_us"].append(t["first_us"])

    print(f"decomposed {used}/{len(glass)} valid trials; "
          f"{rect_hits} detected via the rect path, {used - rect_hits} via frames")
    print_stages("rect-path detections", rect_stages)
    print_stages("frame-path detections", frame_stages)
    return 0


if __name__ == "__main__":
    sys.exit(main())
