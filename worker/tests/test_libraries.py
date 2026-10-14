"""`python -m earshot_worker.libraries`, run as the app runs it."""
import json
import shutil
import subprocess
import sys
from pathlib import Path

import cratelib
import pytest

WORKER_DIR = Path(__file__).resolve().parent.parent


def run(*args) -> list[dict]:
    p = subprocess.run(
        [sys.executable, "-m", "earshot_worker.libraries", *map(str, args)],
        cwd=WORKER_DIR,
        capture_output=True,
        text=True,
        timeout=60,
    )
    lines = [json.loads(line) for line in p.stdout.splitlines()]
    assert ("error" in lines[-1]) == (p.returncode != 0), p.stderr
    return lines


def wine_xml(tmp_path: Path) -> Path:
    """A Rekordbox XML as rekordbox under Wine writes it (`C:/` paths), with one
    track whose file is in `tmp_path/Music` and one that's gone."""
    (tmp_path / "Music/house").mkdir(parents=True)
    (tmp_path / "Music/house/a.mp3").write_bytes(b"not really audio")
    (tmp_path / "Music/house/b.mp3").write_bytes(b"nor this")
    lib = cratelib.Library("test")
    lib.add_track(cratelib.Track("3", "C:/users/dj/Music/house/b.mp3", title="B", file_size=8))
    lib.add_track(
        cratelib.Track(
            "1",
            "C:/users/dj/Music/house/a.mp3",
            file_size=16,
            title="A",
            artist="X",
            genre="House",
            bpm=124.0,
            key=cratelib.Key(9, True),
            rating=3,
            colour=0xFF0000,
        )
    )
    lib.add_track(cratelib.Track("2", "C:/users/dj/Music/gone.mp3", title="Gone"))
    lib.playlists.children.append(
        cratelib.Playlist.folder("Sets", [cratelib.Playlist("Friday", ["2", "1"])])
    )
    xml = tmp_path / "rekordbox.xml"
    cratelib.write(lib, cratelib.RekordboxXml(xml))
    return xml


def test_read_finds_files_behind_wine_paths(tmp_path):
    xml = wine_xml(tmp_path)
    *progress, last = run("read", "rekordbox_xml", xml, "--root", tmp_path / "Music")
    assert all("progress" in m for m in progress)
    lib = last["library"]
    by_title = {t["title"]: t for t in lib["tracks"]}
    a = by_title["A"]
    assert a["file"] == str(tmp_path / "Music/house/a.mp3")
    assert a["location"] == "C:/users/dj/Music/house/a.mp3"
    assert (a["artist"], a["genre"], a["bpm"], a["key"], a["rating"]) == ("X", "House", 124.0, "Am", 3)
    assert by_title["Gone"]["file"] is None
    assert lib["paths"] == {str(tmp_path): "C:/users/dj"}
    assert by_title["Gone"]["bpm"] is None and by_title["Gone"]["key"] is None
    [sets] = lib["playlists"]
    assert (sets["name"], sets["kind"]) == ("Sets", "folder")
    [friday] = sets["children"]
    assert friday["kind"] == "playlist"
    assert friday["tracks"] == [by_title["Gone"]["id"], a["id"]]


def test_rules_found_before_are_used_without_searching(tmp_path):
    xml = wine_xml(tmp_path)
    rules = json.dumps({str(tmp_path / "Music"): "C:/users/dj/Music"})
    *progress, last = run("read", "rekordbox_xml", xml, "--paths", rules)
    by_title = {t["title"]: t for t in last["library"]["tracks"]}
    assert by_title["A"]["file"] == str(tmp_path / "Music/house/a.mp3")
    assert last["library"]["paths"] == json.loads(rules)


def test_find_detects_a_library_at_a_path(tmp_path):
    xml = wine_xml(tmp_path)
    [msg] = run("find", tmp_path)
    assert msg["libraries"] == [
        {"format": "rekordbox_xml", "path": str(xml), "volume_root": None, "name": "Rekordbox XML (rekordbox.xml)"}
    ]


def test_an_unreadable_library_is_an_error(tmp_path):
    bad = tmp_path / "rekordbox.xml"
    bad.write_text("not xml")
    [msg] = run("read", "rekordbox_xml", bad)
    assert msg["error"]


@pytest.mark.skipif(shutil.which("ffmpeg") is None, reason="needs ffmpeg")
def test_cues_and_grids_come_on_earshots_timeline(tmp_path):
    """An MP3's times move earlier by what symphonia trims; other formats' don't."""
    from cratelib.offsets import mp3_gapless_offset_ms

    music = tmp_path / "Music"
    music.mkdir()
    files = {}
    for ext in ("mp3", "flac"):
        files[ext] = music / f"t.{ext}"
        subprocess.run(
            ["ffmpeg", "-nostdin", "-loglevel", "error", "-f", "lavfi", "-i", "sine=d=4", str(files[ext])],
            check=True,
        )
    lib = cratelib.Library("test")
    for i, f in enumerate(files.values()):
        lib.add_track(
            cratelib.Track(
                str(i),
                str(f),
                title=f.suffix[1:],
                cues=[
                    cratelib.Cue(cratelib.CueRole.CUE, 1000.0, slot=0, name="drop", colour=0x00FF00),
                    cratelib.Cue(cratelib.CueRole.LOOP, 2000.0, end_ms=3000.0),
                ],
                grid=[cratelib.TempoMarker(500.0, 120.0, 1)],
            )
        )
    xml = tmp_path / "rb.xml"
    cratelib.write(lib, cratelib.RekordboxXml(xml))

    plain = run("read", "rekordbox_xml", xml)[-1]["library"]["tracks"]
    assert all("cues" not in t for t in plain)
    tracks = {t["title"]: t for t in run("read", "rekordbox_xml", xml, "--cues")[-1]["library"]["tracks"]}
    shift = mp3_gapless_offset_ms(files["mp3"]) / 1000
    assert shift > 0.02
    for title, s in (("mp3", shift), ("flac", 0.0)):
        t = tracks[title]
        hot, loop = sorted(t["cues"], key=lambda c: c["start_s"])
        assert hot == pytest.approx(
            {"role": "cue", "start_s": 1.0 - s, "end_s": None, "slot": 0, "name": "drop", "colour": 0x00FF00}
        )
        assert (loop["role"], loop["slot"]) == ("loop", None)
        assert (loop["start_s"], loop["end_s"]) == pytest.approx((2.0 - s, 3.0 - s))
        [m] = t["grid"]
        assert m == pytest.approx({"start_s": 0.5 - s, "bpm": 120.0, "beat": 1})


def test_files_on_another_drive_are_found(tmp_path, monkeypatch):
    """On Windows, a library from another computer whose music is now on another
    drive: searched only once the usual places have been."""
    from earshot_worker import libraries

    music, other = tmp_path / "C/Users/me/Music", tmp_path / "E"
    music.mkdir(parents=True)
    (other / "DJ/house").mkdir(parents=True)
    for name, data in (("a.mp3", b"aaaa"), ("b.mp3", b"bbbbbb")):
        (other / "DJ/house" / name).write_bytes(data)
    lib = cratelib.Library("test")
    lib.add_track(cratelib.Track("1", "D:/Music/house/a.mp3", title="A", file_size=4))
    lib.add_track(cratelib.Track("2", "D:/Music/house/b.mp3", title="B", file_size=6))
    monkeypatch.setattr(libraries, "send", lambda _: None)

    monkeypatch.setattr(libraries, "drives", list)
    assert libraries.find_files(lib, [music], {}) == {}, "not without other drives"
    monkeypatch.setattr(libraries, "drives", lambda: [other])
    assert libraries.find_files(lib, [music], {}) == {str(other / "DJ"): "D:/Music"}
    assert lib.tracks["1"].file == other / "DJ/house/a.mp3"
