#!/usr/bin/env bash
# Rebuild the video fixtures from iscc-samples 0.6.0 demo.mp4. Needs ffmpeg (7.0 or later, for
# -display_rotation) and uv.
#
# demo.mp4 is eight seconds of the sample (from 8 s, past its black intro), re-encoded small:
# H.264 at 176x144 with a mono AAC track, keeping the sample's title. demo.mov and demo.m4v are
# stream copies into the QuickTime and M4V containers; demo.avi re-encodes the clip as MPEG-4
# Part 2 (FMP4) with MP3 audio, the codecs of the sample's own AVI. rotated.mp4 is demo.mp4 with
# a 90 degree display matrix (ffmpeg rotates before filtering). no-video.mp4 and no-video.mov
# have only the audio track, which the app reads like an M4A; no-video-opus.mp4 has it re-encoded
# as Opus (needs an ffmpeg with libopus), which the app decodes with another decoder than
# fpcalc, and no-video-ac3.mp4 has two seconds of it as AC-3, which the app cannot decode. The
# tags-*.mp4 files are one-second clips without the sample's tags, each carrying one
# case of iscc-sdk's VIDEO_META_MAP order (written through ffmetadata files with
# use_metadata_tags, as iscc-sdk's video_meta_embed writes them). Regenerate expected_video.json
# and expected_audio.json afterwards.
set -euo pipefail
cd "$(dirname "$0")"

FFMPEG=${FFMPEG:-ffmpeg}
SAMPLES=$(uv run --quiet --with iscc-samples==0.6.0 python -c \
  "import iscc_samples, pathlib; print((pathlib.Path(iscc_samples.__file__).parent / 'files' / 'video').as_posix())")

ff() {
  "$FFMPEG" -hide_banner -loglevel error -y "$@"
}

ff -ss 8 -t 8 -i "$SAMPLES/demo.mp4" -c:v libx264 -crf 30 -preset slow -pix_fmt yuv420p \
  -c:a aac -ac 1 -b:a 24k -fflags +bitexact -movflags +faststart demo.mp4
ff -i demo.mp4 -c copy -fflags +bitexact demo.mov
ff -i demo.mp4 -c copy -fflags +bitexact demo.m4v
ff -i demo.mp4 -c:v mpeg4 -vtag FMP4 -q:v 9 -c:a libmp3lame -b:a 24k -fflags +bitexact demo.avi
ff -display_rotation 90 -i demo.mp4 -c copy -fflags +bitexact rotated.mp4
ff -i demo.mp4 -vn -c:a copy -fflags +bitexact no-video.mp4
ff -i demo.mov -vn -c:a copy -fflags +bitexact no-video.mov
ff -i demo.mp4 -vn -c:a libopus -b:a 24k -fflags +bitexact no-video-opus.mp4
ff -t 2 -i demo.mp4 -vn -c:a ac3 -b:a 32k -fflags +bitexact no-video-ac3.mp4

# One-second clip without any tags, the base of every tags-*.mp4.
ff -t 1 -i demo.mp4 -map_metadata -1 -c:v libx264 -crf 40 -an -fflags +bitexact clip.mp4

meta_json=$(printf '{"genre": "demo", "frames": 5}' | base64 | tr -d '\n')
tag() {
  local out=$1
  shift
  printf ';FFMETADATA1\n' >"$out.ffmeta"
  printf '%s\n' "$@" >>"$out.ffmeta"
  ff -i clip.mp4 -i "$out.ffmeta" -map_metadata 1 -movflags use_metadata_tags -c copy \
    -fflags +bitexact "$out"
  rm "$out.ffmeta"
}
# iscc_* keys outrank title, description and comment; the creator comes from author.
tag tags-iscc.mp4 "iscc_name=ISCC name" "iscc_description=ISCC description" \
  "iscc_meta=data:application/json;base64,$meta_json" "title=Plain title" \
  "description=Plain description" "comment=A comment" "author=The Author" "artist=An Artist"
# Standard keys; artist names the creator; an escaped '=' and ';' survive.
tag tags-title.mp4 "title=Title with \= and \; escaped" "description=The description" \
  "synopsis=The synopsis" "artist=An Artist" "album_artist=Album Artist"
# A comment is the description when there is nothing better; no name: the file name.
tag tags-comment-only.mp4 "comment=Only a comment" "composer=A Composer"
# Name from track before show and album; description from synopsis before comment.
tag tags-track.mp4 "track=Track name" "show=Show name" "album=Album name" \
  "synopsis=The synopsis" "comment=A comment"
# An empty title is skipped; album names the asset.
tag tags-empty-title.mp4 "title=" "album=Album name"
rm clip.mp4
ls -l demo.mp4 demo.mov demo.m4v demo.avi rotated.mp4 no-video*.mp4 no-video.mov tags-*.mp4
