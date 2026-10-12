"""Track info comes from the file's own tags where it has them, and the output is encoded
at the source's bitrate rather than always at 320. Ported from
tempo-lock/tests/test_metadata.py, plus the output's name and earshot's hash tag."""
import json
import shutil
import subprocess

import pytest
import soundfile as sf
from synth import live_drums

from earshot_worker import audio

needs_ffmpeg = pytest.mark.skipif(
    not (shutil.which("ffmpeg") and shutil.which("ffprobe")), reason="ffmpeg not installed"
)

TAGS = {"title": "Slow Burn", "artist": "The Metronomes", "album": "Off The Grid", "date": "2011"}


@pytest.fixture(scope="module")
def wav(tmp_path_factory):
    y, sr, _ = live_drums(duration=6.0, drift_bpm=3.0)
    path = tmp_path_factory.mktemp("src") / "drums.wav"
    sf.write(str(path), y, sr, subtype="PCM_16")
    return path


def _encode(wav, out, kbps, tags=None):
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-i", str(wav), "-c:a", "libmp3lame", "-b:a", f"{kbps}k"]
    for k, v in (tags or {}).items():
        cmd += ["-metadata", f"{k}={v}"]
    subprocess.run([*cmd, str(out)], check=True)
    return out


def _all_tags(path):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_format", "-of", "json", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout
    return {k.lower(): v for k, v in (json.loads(out)["format"].get("tags") or {}).items()}


@needs_ffmpeg
def test_probe_reads_tags_and_the_technical_shape(wav, tmp_path):
    info = audio.probe(_encode(wav, tmp_path / "tagged.mp3", 128, TAGS))
    assert info["tags"]["title"] == "Slow Burn"
    assert info["tags"]["artist"] == "The Metronomes"
    assert info["tags"]["album"] == "Off The Grid"
    assert info["tags"]["date"].startswith("2011")
    assert info["codec"] == "mp3"
    assert info["channels"] == 2
    assert info["sample_rate"] == 44100
    assert 120 <= info["bitrate_kbps"] <= 136
    assert info["lossless"] is False


@needs_ffmpeg
def test_probe_on_an_untagged_file_yields_no_tags_but_still_describes_it(wav):
    info = audio.probe(wav)
    assert info["tags"] == {}
    assert info["lossless"] is True
    assert info["sample_rate"] == 44100


def test_probe_never_raises_on_junk(tmp_path):
    junk = tmp_path / "not-audio.mp3"
    junk.write_bytes(b"definitely not an MP3")
    assert audio.probe(junk) == {}
    assert audio.probe(tmp_path / "missing.mp3") == {}


@pytest.mark.parametrize(
    ("info", "expected"),
    [
        ({"bitrate_kbps": 128, "lossless": False}, 128),
        ({"bitrate_kbps": 192, "lossless": False}, 192),
        ({"bitrate_kbps": 131, "lossless": False}, 128),  # VBR average snaps to a real rate
        ({"bitrate_kbps": 245, "lossless": False}, 256),
        ({"bitrate_kbps": 900, "lossless": False}, 320),  # nothing above 320 exists
        ({"bitrate_kbps": 1411, "lossless": True}, 320),  # CD-quality WAV has no equivalent
        ({}, 320),
        (None, 320),
    ],
)
def test_bitrate_matching(info, expected):
    assert audio.mp3_bitrate_for(info) == expected


@needs_ffmpeg
def test_written_mp3_matches_the_source_bitrate_and_keeps_the_tags(wav, tmp_path):
    src = _encode(wav, tmp_path / "src.mp3", 128, {**TAGS, "AUDIO_SHA256": "ab" * 32})
    y, sr = audio.load(src)
    out = tmp_path / "out.mp3"
    audio.write_mp3(out, y, sr, copy_tags_from=src, bpm=124, title=audio.straightened("Slow Burn", 124))

    info = audio.probe(out)
    assert 120 <= info["bitrate_kbps"] <= 136, "re-encoded away from the source bitrate"
    assert info["tags"]["title"] == "Slow Burn (straightened 124 bpm)"
    assert info["tags"]["artist"] == "The Metronomes"
    # a 320k default would have been ~2.5x the size
    assert out.stat().st_size < src.stat().st_size * 1.4
    tags = _all_tags(out)
    assert tags["tbpm"] == "124"
    # earshot's hash of the source's audio would make the output look like the source
    assert "audio_sha256" in _all_tags(src) and "audio_sha256" not in tags
    assert not out.with_suffix(".tmp.wav").exists()


@needs_ffmpeg
def test_lossless_source_still_gets_a_full_quality_encode(wav, tmp_path):
    y, sr = audio.load(wav)
    out = tmp_path / "out.mp3"
    audio.write_mp3(out, y, sr, copy_tags_from=wav)
    assert audio.probe(out)["bitrate_kbps"] >= 300


def test_output_is_named_from_the_tags_else_the_filename():
    tags = {"tags": {"artist": "The Metronomes", "title": "Slow/Burn"}}
    assert audio.output_stem("x.mp3", tags) == "The Metronomes - Slow_Burn"
    assert audio.output_stem("/music/My Song.flac", {}) == "My Song"
    assert audio.straightened("My Song", 124.5) == "My Song (straightened 124.5 bpm)"
    assert audio.straightened("My Song", 124.0) == "My Song (straightened 124 bpm)"
