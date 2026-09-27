#!/usr/bin/env bash
# Rebuild the audio fixtures from iscc-samples 0.6.0. Needs ffmpeg and uv.
#
# demo.mp3, withcover.mp3 and short.wav are copied unchanged. demo.flac, demo.m4a and demo.wav
# are short conversions of iscc-samples' demo.wav that keep its title and cover the paths the
# MP3 does not: an interpolating resampler rate (32 kHz), AAC priming in an MP4 edit list, RIFF
# INFO tags. iscc-tags.mp3 and
# iscc-tags.flac are demo.mp3 and demo.flac tagged by iscc-sdk's audio_meta_embed with a name, a
# description and ISCC metadata. The tags-*.* files are one-second conversions tagged with
# mutagen, each pinning a case of TagLib's tag choice and value rules (a tag with only ISCC
# keys or only a composer still counts, APE carries ISCC keys, MP4 keeps an empty first value).
# Regenerate expected_audio.json afterwards.
set -euo pipefail
cd "$(dirname "$0")"

SAMPLES=$(uv run --quiet --with iscc-samples==0.6.0 python -c \
  "import iscc_samples, pathlib; print((pathlib.Path(iscc_samples.__file__).parent / 'files' / 'audio').as_posix())")

for f in demo.mp3 withcover.mp3 short.wav; do
  cp "$SAMPLES/$f" "$f"
done

convert() {
  ffmpeg -hide_banner -loglevel error -y -i "$SAMPLES/demo.wav" -fflags +bitexact "$@"
}
convert -t 4 -ac 1 -ar 32000 demo.flac
convert -c:a aac -b:a 64k demo.m4a
convert -t 5 -ac 1 -ar 22050 -c:a pcm_s16le demo.wav
convert -t 1 -ac 1 -ar 8000 -map_metadata -1 tags-iscc-only.mp3
cp tags-iscc-only.mp3 tags-ape.mp3
cp tags-iscc-only.mp3 tags-composer-only.mp3
convert -t 1 -ac 1 -ar 8000 -map_metadata -1 tags-iscc-only.flac
convert -t 1 -ac 1 -ar 8000 -map_metadata -1 -metadata title="Info title" -c:a pcm_s16le tags-iscc-only.wav
convert -t 1 -ac 1 -ar 8000 -map_metadata -1 -c:a aac tags-empty-first.m4a

uv run --quiet --with iscc-sdk python - <<'EOF'
import base64
import json
import shutil

import iscc_sdk as idk

meta = "data:application/json;base64," + base64.b64encode(
    json.dumps({"genre": "demo", "bpm": 120}).encode()
).decode()
for source in ("demo.mp3", "demo.flac"):
    target = "iscc-tags." + source.split(".")[1]
    tagged = idk.audio_meta_embed(
        source,
        idk.IsccMeta.model_construct(
            name="Belly Button – tagged by iscc-sdk",
            description="Demo track with ISCC tags, Grüße",
            meta=meta,
        ),
    )
    shutil.move(tagged, target)
EOF

uv run --quiet --with mutagen python - <<'EOF'
from mutagen.apev2 import APEv2
from mutagen.flac import FLAC
from mutagen.id3 import ID3, TCOM, TXXX
from mutagen.mp4 import MP4
from mutagen.wave import WAVE


def id3(*frames):
    tag = ID3()
    for frame in frames:
        tag.add(frame)
    return tag


def txxx(key, value):
    return TXXX(encoding=3, desc=key, text=[value])


id3(txxx("ISCC:NAME", "ISCC name only"), txxx("ISCC:DESCRIPTION", "Only ISCC keys")).save(
    "tags-iscc-only.mp3"
)
ape = APEv2()
ape.update({"Title": "APE title", "ISCC:NAME": "APE ISCC name", "ISCC:DESCRIPTION": "From APE"})
ape.save("tags-ape.mp3")
id3(TCOM(encoding=3, text=["A Composer"])).save("tags-composer-only.mp3")
ape = APEv2()
ape["Title"] = "APE title behind the ID3v2 tag"
ape.save("tags-composer-only.mp3")
flac = FLAC("tags-iscc-only.flac")
flac.clear()
flac.update({"ISCC:NAME": "ISCC name only", "ISCC:DESCRIPTION": "Only ISCC keys"})
flac.save()
wav = WAVE("tags-iscc-only.wav")
wav.add_tags()
wav.tags.add(txxx("ISCC:NAME", "ISCC name only"))
wav.save()
mp4 = MP4("tags-empty-first.m4a")
mp4.clear()
mp4.update({"\xa9nam": ["", "Second title"], "\xa9ART": ["An Artist"]})
mp4.save()
EOF

ls -l demo.mp3 withcover.mp3 short.wav demo.flac demo.m4a demo.wav \
  iscc-tags.mp3 iscc-tags.flac tags-*.*
