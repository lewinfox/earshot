"""Make the audio-hash fixtures for core/hash.rs's tests, with cratelib's own code.

    ~/git/cratelib/.venv/bin/python gui/testdata/make_fixtures.py gui/testdata

The "audio" is random bytes (the hash doesn't care whether it decodes). Each file
gets a title, a picture and an unrelated TXXX frame before any AUDIO_SHA256 one, so
the Rust tag reader has to skip frames. expected.txt holds cratelib's audio_sha256.
"""

import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path.home() / "git/cratelib/src"))
from mutagen.id3 import APIC, ID3, TIT2, TXXX  # noqa: E402

from cratelib.audiofile import audio_sha256, stamp_audio_hash  # noqa: E402

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
rng = random.Random(7)
audio = bytes(rng.randrange(256) for _ in range(6000))
lines = []


def make(name: str, version: int, stamp: bool, id3v1: bool) -> None:
    p = out / name
    p.write_bytes(audio + ((b"TAG" + b" " * 125) if id3v1 else b""))
    t = ID3()
    t.add(TIT2(encoding=3, text="Fixture"))
    t.add(APIC(encoding=0, mime="image/png", type=3, desc="", data=bytes(range(256)) * 4))
    t.add(TXXX(encoding=1, desc="OTHER", text="not it"))
    t.save(p, v2_version=version)
    if stamp:
        stamp_audio_hash(p)
    lines.append(f"{name} {audio_sha256(p)}")


make("v24_stamped.mp3", 4, True, True)
make("v23_stamped.mp3", 3, True, False)
make("v23_plain.mp3", 3, False, True)
(out / "expected.txt").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
