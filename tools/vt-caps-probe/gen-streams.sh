#!/bin/bash
# Generate the real test streams the probe feeds to VideoToolbox: one tiny stream per
# codec x chroma x bit-depth at 640x480 (streams/), and the cases that matter at native
# 5K 5120x2880 (streams5k/). 8 frames each, GOP 4, so every stream has two keyframes.
#
# Needs: ffmpeg with libx264, libx265, libvpx-vp9, libsvtav1 (Homebrew's has all four),
# and cargo (the AV1 4:4:4 / 4:2:2 streams come from the rav1e crate in ./av1gen,
# because SVT-AV1 is 4:2:0-only and no other AV1 encoder is assumed installed).
set -e
cd "$(dirname "$0")"
mkdir -p streams streams5k

small() { ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=640x480:rate=30 -frames:v 8 "$@"; }
big()   { ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=5120x2880:rate=30 -frames:v 8 "$@"; }
X="-x265-params log-level=none"

echo "== 640x480 =="
small -pix_fmt yuv420p     -c:v libx264 -profile:v high    -g 4 -f h264 -y streams/h264_420_8.h264
small -pix_fmt yuv420p10le -c:v libx264 -profile:v high10  -g 4 -f h264 -y streams/h264_420_10.h264
small -pix_fmt yuv422p     -c:v libx264 -profile:v high422 -g 4 -f h264 -y streams/h264_422_8.h264
small -pix_fmt yuv444p     -c:v libx264 -profile:v high444 -g 4 -f h264 -y streams/h264_444_8.h264
small -pix_fmt yuv420p     -c:v libx265 -g 4 $X -f hevc -y streams/hevc_420_8.h265
small -pix_fmt yuv420p10le -c:v libx265 -g 4 $X -f hevc -y streams/hevc_420_10.h265
small -pix_fmt yuv422p     -c:v libx265 -g 4 $X -f hevc -y streams/hevc_422_8.h265
small -pix_fmt yuv422p10le -c:v libx265 -g 4 $X -f hevc -y streams/hevc_422_10.h265
small -pix_fmt yuv444p     -c:v libx265 -g 4 $X -f hevc -y streams/hevc_444_8.h265
small -pix_fmt yuv444p10le -c:v libx265 -g 4 $X -f hevc -y streams/hevc_444_10.h265
small -pix_fmt yuv420p     -c:v libvpx-vp9 -g 4 -deadline realtime -f ivf -y streams/vp9_420_8.ivf
small -pix_fmt yuv420p10le -c:v libvpx-vp9 -g 4 -deadline realtime -f ivf -y streams/vp9_420_10.ivf
small -pix_fmt yuv444p     -c:v libvpx-vp9 -g 4 -deadline realtime -f ivf -y streams/vp9_444_8.ivf
small -pix_fmt yuv420p     -c:v libsvtav1 -g 4 -preset 12 -f ivf -y streams/av1_420_8.ivf 2>/dev/null
small -pix_fmt yuv420p10le -c:v libsvtav1 -g 4 -preset 12 -f ivf -y streams/av1_420_10.ivf 2>/dev/null
( cd av1gen && CARGO_TARGET_DIR="${TMPDIR:-/tmp}/vt-caps-probe-av1gen-target" cargo run -q -- ../streams )

echo "== 5120x2880 =="
big -pix_fmt yuv420p     -c:v libx264 -preset ultrafast -profile:v high    -g 4 -f h264 -y streams5k/h264_420_8.h264
big -pix_fmt yuv444p     -c:v libx264 -preset ultrafast -profile:v high444 -g 4 -f h264 -y streams5k/h264_444_8.h264
big -pix_fmt yuv420p     -c:v libx265 -preset ultrafast -g 4 $X -f hevc -y streams5k/hevc_420_8.h265
big -pix_fmt yuv444p     -c:v libx265 -preset ultrafast -g 4 $X -f hevc -y streams5k/hevc_444_8.h265
big -pix_fmt yuv444p10le -c:v libx265 -preset ultrafast -g 4 $X -f hevc -y streams5k/hevc_444_10.h265
big -pix_fmt yuv420p     -c:v libsvtav1 -g 4 -preset 12 -f ivf -y streams5k/av1_420_8.ivf 2>/dev/null

echo "== what ffprobe says each stream is =="
for f in streams/* streams5k/*; do
  printf "%-28s " "$f"
  ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,profile,pix_fmt,width,height -of csv=p=0 "$f"
done
