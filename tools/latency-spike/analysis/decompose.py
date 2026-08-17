#!/usr/bin/env python3
"""Decompose a spike glass run into per-stage latency medians.

Usage:
    decompose.py <glass.jsonl> <client.jsonl> <server.jsonl>

Joins three files taken from the same run:
  glass trials  (CoreMedia clock, Mac)   inject_pts_us, first_us
  client frames (CoreMedia clock, Mac)   recv/decode/present per frame, au_bytes
  server frames (QPC clock, quench)      present/acquire/convert/encode/send, au_bytes

Client and server frames are joined by au_bytes in arrival order. The two clocks are
never compared directly: cross-machine segments are derived as residuals of same-clock
spans, so no clock synchronisation is needed. The residual "input+render+net" bundles
Mac input dispatch, the tunnel hop for the 8-byte input record, SendInput delivery, the
target app's render, and DWM composition — everything upstream of the pixel existing.

First used on the 2026-08-17 stage-2 run (baseline/spike-idd-stage2-*.jsonl); the
per-stage medians summed to ~56 ms against a measured 54.3 ms trial median.
"""
import json
import statistics
import sys


def load(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def main():
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    glass = [r for r in load(sys.argv[1])
             if r.get("type") == "trial" and not r["timed_out"] and not r["poisoned"]]
    client = [r for r in load(sys.argv[2]) if r.get("type") == "frame"]
    server = [r for r in load(sys.argv[3]) if r.get("record") == "frame"]

    # Join client->server by order + au_bytes.
    pairs = []
    si = 0
    for c in client:
        while si < len(server) and server[si]["au_bytes"] != c["au_bytes"]:
            si += 1
        if si == len(server):
            break
        pairs.append((c, server[si]))
        si += 1
    print(f"joined {len(pairs)}/{len(client)} client frames to server frames",
          file=sys.stderr)

    stages = {k: [] for k in [
        "input+render+net (residual)", "dwm present->dxgi acquire",
        "convert (bgra->nv12)", "encode (convert_end->out)", "server send",
        "recv->decode_in", "decode", "decode_out->present",
        "present->sck detect", "total first_us"]}

    used = 0
    for t in glass:
        t_inj = t["inject_pts_us"]
        t_first = t_inj + t["first_us"]
        # Detection frame: the client frame presented nearest the detected change.
        best = None
        for c, s in pairs:
            d = abs(c["present_done_us"] - t_first)
            if d < 25_000 and (best is None or d < best[0]):
                best = (d, c, s)
        if best is None:
            continue
        _, c, s = best
        used += 1
        server_span = s["send_done_us"] - s["present_qpc_us"]
        mac_span = c["recv_done_us"] - t_inj
        stages["input+render+net (residual)"].append(mac_span - server_span)
        stages["dwm present->dxgi acquire"].append(s["acquire_qpc_us"] - s["present_qpc_us"])
        stages["convert (bgra->nv12)"].append(s["convert_end_us"] - s["acquire_qpc_us"])
        stages["encode (convert_end->out)"].append(s["encode_out_us"] - s["convert_end_us"])
        stages["server send"].append(s["send_done_us"] - s["encode_out_us"])
        stages["recv->decode_in"].append(c["decode_in_us"] - c["recv_done_us"])
        stages["decode"].append(c["decode_out_us"] - c["decode_in_us"])
        stages["decode_out->present"].append(c["present_done_us"] - c["decode_out_us"])
        stages["present->sck detect"].append(t_first - c["present_done_us"])
        stages["total first_us"].append(t["first_us"])

    print(f"decomposed {used}/{len(glass)} valid trials\n")
    print(f"{'stage':38s} {'p50 ms':>8s} {'p95 ms':>8s} {'min':>7s} {'max':>8s}")
    for k, v in stages.items():
        if not v:
            continue
        v = sorted(v)
        p50 = statistics.median(v) / 1000
        p95 = v[max(0, int(len(v) * 0.95) - 1)] / 1000
        print(f"{k:38s} {p50:8.2f} {p95:8.2f} {v[0]/1000:7.2f} {v[-1]/1000:8.2f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
