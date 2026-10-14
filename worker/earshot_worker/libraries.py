"""Other DJ programs' libraries, read through cratelib: one command per process.

    python -m earshot_worker.libraries find [PATH]
    python -m earshot_worker.libraries read FORMAT PATH [--volume-root R] [--root DIR]... [--cues] [--paths JSON]

stdout carries JSON lines only (logs go to stderr): any number of
`{"progress": "..."}`, then one of `{"libraries": [...]}` (find), `{"library": {...}}`
(read) or `{"error": "..."}` (and exit status 1). See ../README.md, "Other libraries".

Separate from the analysis worker (`python -m earshot_worker`): it never imports
torch, so it starts in well under a second, and the app runs it only when the user
opens another library.
"""
from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import traceback
from pathlib import Path


def _claim_stdout():
    """Return a text stream on the real stdout and send everything else to stderr
    (pyrekordbox logs as it imports)."""
    sys.stdout.flush()
    proto_fd = os.dup(1)
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    return os.fdopen(proto_fd, "w", encoding="utf-8", buffering=1)


_proto = _claim_stdout()

import cratelib  # noqa: E402
from cratelib.keys import KeyNotation, format_key  # noqa: E402
from cratelib.offsets import mp3_gapless_offset_ms  # noqa: E402
from cratelib.paths import discover  # noqa: E402

log = logging.getLogger("earshot_worker.libraries")

NAMES = {
    cratelib.Format.REKORDBOX_DB: "Rekordbox",
    cratelib.Format.REKORDBOX_XML: "Rekordbox XML",
    cratelib.Format.REKORDBOX_USB: "Rekordbox USB",
    cratelib.Format.SERATO: "Serato",
    cratelib.Format.MIXXX: "Mixxx",
}


def send(msg: dict) -> None:
    _proto.write(json.dumps(msg, allow_nan=False) + "\n")
    _proto.flush()


def source_json(s: cratelib.Source) -> dict:
    name = NAMES[s.format]
    if isinstance(s, (cratelib.RekordboxXml, cratelib.RekordboxUsb)):
        name = f"{name} ({Path(s.path).name or s.path})"
    return {
        "format": str(s.format),
        "path": str(s.path),
        "volume_root": getattr(s, "volume_root", None),
        "name": name,
    }


def make_source(fmt: str, path: str, volume_root: str | None) -> cratelib.Source:
    match cratelib.Format(fmt):
        case cratelib.Format.MIXXX:
            return cratelib.Mixxx(Path(path))
        case cratelib.Format.REKORDBOX_DB:
            return cratelib.RekordboxDb(Path(path))
        case cratelib.Format.REKORDBOX_XML:
            return cratelib.RekordboxXml(Path(path))
        case cratelib.Format.REKORDBOX_USB:
            return cratelib.RekordboxUsb(Path(path))
        case cratelib.Format.SERATO:
            return cratelib.Serato(Path(path), volume_root or "/")
    raise ValueError(f"unknown format {fmt!r}")


def bounds() -> list[Path]:
    """Where looking for files may go: the home folder and where drives are mounted."""
    home = Path.home()
    return [p for p in (home, Path("/media"), Path("/mnt"), Path("/Volumes")) if p.is_dir()]


def seed(known: dict[str, str]) -> dict[str, str]:
    """The rules to start from: those found before, and Wine's Z: drive, which is /."""
    return ({"/": "Z:/"} if os.name != "nt" else {}) | known


def find_files(library: cratelib.Library, roots: list[Path], known: dict[str, str]) -> dict[str, str]:
    """Find tracks' files that cratelib couldn't, when the program stores other paths
    (it runs under Wine or in a container, or the library came from another computer).
    `known`: rules found before ({our folder: the program's}). New ones are searched
    for from `roots` (earshot's music folders) outwards, within the home folder and
    mounted drives (cratelib's `discover`). Returns the rules that apply now."""
    mapping = seed(known)
    tracks = [(t.location, t.file_size) for t in library.tracks.values() if t.file is None]
    if not tracks:
        return known
    paths = discover(
        tracks,
        start=roots or [Path.home() / "Music"],
        bounds=bounds(),
        paths=cratelib.PathMap(mapping),
        progress=lambda m: send({"progress": m}),
    )
    for t in library.tracks.values():
        if t.file is None:
            t.file = paths.find(t.location)
    return {k: v for k, v in paths.mapping.items() if (k, v) != ("/", "Z:/")}


def shift_s(t: cratelib.Track) -> float:
    """Seconds to take off cratelib's times (Rekordbox's timeline) to get earshot's.

    earshot decodes with symphonia: for an MP3 it drops the Info frame and trims the
    LAME tag's encoder delay (cratelib's gapless offset). Its MP4 reader ignores
    edit lists, so AAC priming is kept, as Rekordbox keeps it; other formats have
    nothing to trim."""
    if t.file is None or t.extension != "mp3":
        return 0.0
    try:
        return mp3_gapless_offset_ms(t.file) / 1000
    except Exception as e:
        log.warning("%s: can't read its MP3 header, so its cues aren't shifted: %s", t.file, e)
        return 0.0


def cues_json(t: cratelib.Track, shift: float) -> list[dict]:
    return [
        {
            "role": str(c.role),
            "start_s": max(0.0, c.position_ms / 1000 - shift),
            "end_s": max(0.0, c.end_ms / 1000 - shift) if c.end_ms is not None else None,
            "slot": c.slot,
            "name": c.name,
            "colour": c.colour,
        }
        for c in t.cues
    ]


def grid_json(t: cratelib.Track, shift: float) -> list[dict]:
    return [{"start_s": m.position_ms / 1000 - shift, "bpm": m.bpm, "beat": m.beat} for m in t.grid if m.bpm > 0]


def track_json(t: cratelib.Track, cues: bool) -> dict:
    timed = {}
    if cues:
        shift = shift_s(t)
        timed = {"cues": cues_json(t, shift), "grid": grid_json(t, shift)}
    return {
        "id": t.id,
        "location": t.location,
        "file": str(t.file) if t.file else None,
        "title": t.title,
        "artist": t.artist,
        "album": t.album,
        "genre": t.genre,
        "comment": t.comment,
        "bpm": t.bpm if t.bpm > 0 else None,
        "key": format_key(t.key, KeyNotation.MUSICAL) or None,
        "rating": t.rating,
        "colour": t.colour,
        "duration_s": t.duration_s if t.duration_s > 0 else None,
        **timed,
    }


def playlist_json(p: cratelib.Playlist) -> dict:
    if p.is_folder:
        return {"name": p.name, "kind": "folder", "children": [playlist_json(c) for c in p.children]}
    return {"name": p.name, "kind": "crate" if p.is_crate else "playlist", "tracks": list(p.track_ids or [])}


def read(source: cratelib.Source, roots: list[Path], cues: bool, known: dict[str, str]) -> dict:
    """`cues`: with each track's cues and grid too, on earshot's timeline. Slower:
    that means opening every audio file (Serato) or analysis file (Rekordbox)."""
    # The rules found before go into the read too: Serato's cues and grids are in
    # the audio files, and Mixxx's offsets depend on them, so cratelib has to find the
    # files as it reads.
    library = cratelib.read(
        source, cratelib.PathMap(seed(known)), file_tags=cues, progress=lambda m: send({"progress": m})
    )
    paths = find_files(library, roots, known)
    return {
        "source": library.source,
        "paths": paths,
        "tracks": [track_json(t, cues) for t in library.tracks.values()],
        "playlists": [playlist_json(c) for c in library.playlists.children],
        "warnings": library.warnings,
    }


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(level=logging.WARNING, stream=sys.stderr)
    parser = argparse.ArgumentParser(prog="earshot_worker.libraries")
    sub = parser.add_subparsers(dest="cmd", required=True)
    find = sub.add_parser("find", help="the libraries installed here, or at PATH")
    find.add_argument("path", nargs="?")
    rd = sub.add_parser("read", help="read one library")
    rd.add_argument("format", choices=[str(f) for f in cratelib.Format])
    rd.add_argument("path")
    rd.add_argument("--volume-root")
    rd.add_argument("--root", action="append", default=[], help="a music folder, to find files under")
    rd.add_argument("--cues", action="store_true", help="with cues and grids, on earshot's timeline")
    rd.add_argument("--paths", default="{}", help="path rules found before, JSON {our folder: the program's}")
    args = parser.parse_args(argv)
    try:
        if args.cmd == "find":
            found = cratelib.detect(Path(args.path)) if args.path else cratelib.installed()
            send({"libraries": [source_json(s) for s in found]})
        else:
            source = make_source(args.format, args.path, args.volume_root)
            send({"library": read(source, [Path(r) for r in args.root], args.cues, json.loads(args.paths))})
    except Exception as e:
        traceback.print_exc(file=sys.stderr)
        send({"error": str(e) or type(e).__name__})
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
