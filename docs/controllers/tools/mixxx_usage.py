#!/usr/bin/env python3
"""Measure which Mixxx controls the Mixxx controller mappings use, and how.

Reads:
  - Mixxx's generated control list, res/controllers/mixxx-controls.d.ts (made from
    the manual's controls appendix), for every documented control.
  - The installed mappings (*.midi.xml, *.hid.xml, *.bulk.xml and their scripts).

Writes a JSON file of counts that mixxx_tables.py turns into the tables in
docs/controllers/mixxx-controls.md, and prints a summary.

Usage:
  python3 -I mixxx_usage.py [--dts PATH] [--mappings DIR] [--out usage.json]

Counting rules (approximate on purpose; see mixxx-mappings.md):
  - A "mapping" is one XML file plus the script files it lists. Shared libraries
    (files listed by 3+ mappings, and common-controller-scripts.js) are not counted
    as the mapping's own code; components classes are credited by class instead.
  - XML <control> inputs: direct (group + key) or <Script-Binding/> (key is a JS
    function). XML <output>: group + key, the control the LED watches.
  - JS: a control counts as used by a mapping when its name appears as a string
    literal in the mapping's own scripts ("play", 'hotcue_' + i + '_activate',
    `beatloop_${n}_toggle`), whatever group it is used with. This over-counts
    generic words a little ("enabled", "mix") and under-counts names built from
    variables.
"""

import argparse
import collections
import glob
import json
import os
import re
import xml.etree.ElementTree as ET

POT_SUFFIXES = [
    "_up_small", "_down_small", "_up", "_down", "_set_default", "_set_one",
    "_set_minus_one", "_set_zero", "_minus_toggle", "_toggle",
]

# Controls a midi-components-0.0.js class drives without the mapping naming them.
COMPONENT_KEYS = {
    "PlayButton": ["play", "reverse", "play_indicator"],
    "CueButton": ["cue_default", "reverseroll", "start_stop", "cue_indicator"],
    "SyncButton": ["sync_enabled", "beatsync", "quantize"],
    "LoopToggleButton": ["reloop_exit", "loop_enabled"],
    "HotcueButton": ["hotcue_N_activate", "hotcue_N_clear", "hotcue_N_status", "hotcue_N_color"],
    "SamplerButton": ["track_loaded", "LoadSelectedTrack", "cue_gotoandplay", "play", "eject",
                      "repeat", "num_samplers"],
    "EffectAssignmentButton": ["group_[ChannelN]_enable"],
    "JogWheelBasic": ["jog"],
    "EffectUnit": ["show_focus", "focused_effect", "show_parameters", "meta", "mix", "super1",
                   "group_[ChannelN]_enable", "effect_selector", "parameterN", "enabled",
                   "button_parameterN", "num_effectsavailable"],
}

# ---- the documented controls ----------------------------------------------------------


def parse_dts(path):
    """Every documented control: name pattern, groups, section (rw / ro / dep), tags."""
    src = open(path, encoding="utf-8").read()
    start = src.index("type AppControl")
    ro_at = src.index("namespace ReadOnly")
    dep_at = src.index("namespace Deprecated")
    entry = re.compile(r"/\*\*(.*?)\*/\s*\|?\s*(['`])(.*?)\2", re.S)
    out = {}
    for m in entry.finditer(src, start):
        doc, raw = m.group(1), m.group(3)
        tags, desc, cur = {}, [], None
        for line in doc.strip().splitlines():
            line = re.sub(r"^\s*\* ?", "", line)
            t = re.match(r"@(\w+)\s*(.*)", line)
            if t:
                cur = t.group(1)
                tags[cur] = t.group(2).strip()
            elif cur:
                tags[cur] += " " + line.strip()
            elif line.strip():
                desc.append(line.strip())
        sec = "rw" if m.start() < ro_at else ("ro" if m.start() < dep_at else "dep")
        if "readonly" in tags:
            sec = "ro"
        name = raw.replace("${PotMeterSuffix}", "").replace("${number}", "N")
        numbered = "${number}" in raw
        groups = [g.strip() for g in tags.get("groups", "?").split(",")]
        key = (name, tuple(groups))
        if key in out:  # left/right pairs documented twice under one pattern
            continue
        out[key] = {
            "name": name,
            "groups": groups,
            "family": family_of(groups),
            "section": sec,
            "pot_suffixes": "${PotMeterSuffix}" in raw,
            "numbered": numbered,
            "desc": " ".join(desc),
            "range": tags.get("range", ""),
            "feedback": tags.get("feedback", ""),
            "deprecated": tags.get("deprecated", ""),
        }
    return list(out.values())


def family_of(groups):
    g = groups[0]
    if g in ("[ChannelN]", "[PreviewDeckN]", "[SamplerN]"):
        return "deck"
    if g.startswith("[ChannelN_Stem") or "Stem" in g:
        return "stem"
    if g in ("[AuxiliaryN]", "[MicrophoneN]"):
        return "mic_aux"
    if g.startswith(("[EffectRack1", "[EqualizerRack1", "[QuickEffectRack1")):
        return "effects"
    if g in ("[Master]", "[Main]"):
        return "mixer"
    if g in ("[Library]", "[Playlist]"):
        return "library"
    return g.strip("[]").lower()


# ---- normalising names found in mappings -----------------------------------------------


def norm_group(g):
    g = g.strip()
    g = re.sub(r"\[Channel\d+\]", "[ChannelN]", g)
    g = re.sub(r"\[Sampler\d+\]", "[SamplerN]", g)
    g = re.sub(r"\[PreviewDeck\d+\]", "[PreviewDeckN]", g)
    g = re.sub(r"\[(Microphone|Auxiliary)\d*\]", r"[\1N]", g)
    g = re.sub(r"_Stem\d+", "_StemM", g)
    g = re.sub(r"EffectUnit\d+", "EffectUnitN", g)
    g = re.sub(r"_Effect\d+\]$", "_EffectM]", g)
    g = g.replace("[ChannelN]", "[ChannelI]") if "Rack1_" in g else g
    if g.startswith(("[EqualizerRack1_", "[QuickEffectRack1_")):
        g = g.replace("_EffectM]", "_Effect1]")
    return g


class Names:
    """Matches a key from a mapping to a documented control name."""

    def __init__(self, controls):
        self.exact = {c["name"] for c in controls if not c["numbered"]}
        self.pot = {c["name"] for c in controls if c["pot_suffixes"]}
        self.pats = []
        self.templ = []  # (prefix, suffix, name) for names built by concatenation
        for n in sorted({c["name"] for c in controls if c["numbered"]}, key=len, reverse=True):
            parts = n.split("N") if n.count("N") == 1 else None
            # The number: 1, 0.5, r0.5 (beatloop_r…), %1 (C++), or N (our own docs).
            num = r"(?:[0-9.]+|%1|N)"
            if "[ChannelN]" in n or "[SamplerN]" in n:
                rx = re.escape(n).replace("N", num)
            else:
                i = n.rindex("N") if parts is None else n.index("N")
                rx = re.escape(n[:i]) + num + re.escape(n[i + 1:])
            self.pats.append((re.compile("^" + rx + "$"), n))
            if parts and len(parts[0]) > 3:
                self.templ.append((parts[0], parts[1], n))

    def match(self, key):
        key = key.strip()
        if key in self.exact:
            return key
        for rx, n in self.pats:
            if rx.match(key):
                return n
        for s in POT_SUFFIXES:
            if key.endswith(s) and key[: -len(s)] in self.pot:
                return key[: -len(s)]
        return None


# ---- reading mappings ------------------------------------------------------------------


def text(el, tag):
    t = el.find(tag)
    return (t.text or "").strip() if t is not None and t.text else ""


def read_xml(path):
    try:
        root = ET.parse(path).getroot()
    except ET.ParseError:
        raw = open(path, encoding="utf-8", errors="replace").read()
        raw = re.sub(r"&(?![a-z]+;|#)", "&amp;", raw)
        root = ET.fromstring(raw)
    info = {
        "name": text(root.find("info"), "name") if root.find("info") is not None else "",
        "scripts": [f.get("filename") for f in root.iter("file") if f.get("filename")],
        "inputs": [],
        "outputs": [],
    }
    for c in root.iter("control"):
        o_el = c.find("options")
        opts = [o.tag for o in o_el] if o_el is not None else []
        info["inputs"].append({
            "group": text(c, "group"),
            "key": text(c, "key"),
            "options": opts,
            "script": "Script-Binding" in opts or "script-binding" in [o.lower() for o in opts],
        })
    for o in root.iter("output"):
        info["outputs"].append({
            "group": text(o, "group"),
            "key": text(o, "key"),
            "has_minmax": o.find("minimum") is not None or o.find("maximum") is not None,
            "on": text(o, "on"),
            "off": text(o, "off"),
        })
    return info


STRING = re.compile(r"\"((?:[^\"\\\n]|\\.)*)\"|'((?:[^'\\\n]|\\.)*)'|`((?:[^`\\]|\\.)*)`")


def js_strings(src):
    for m in STRING.finditer(src):
        s = m.group(1) if m.group(1) is not None else m.group(2) if m.group(2) is not None else m.group(3)
        yield s


def strip_comments(src):
    src = re.sub(r"/\*.*?\*/", "", src, flags=re.S)
    return re.sub(r"(?m)(^|[^:\\\"'])//.*$", r"\1", src)


API = {
    "engine": re.compile(r"\bengine\.(\w+)\s*\("),
    "midi": re.compile(r"\bmidi\.(\w+)\s*\("),
    "script": re.compile(r"\bscript\.(\w+)\b"),
    "controller": re.compile(r"\bcontroller\.(\w+)\s*\("),
    "components": re.compile(r"\bcomponents\.(\w+)\b"),
    "console": re.compile(r"\bconsole\.(\w+)\s*\("),
}


def js_usage(src, names):
    src = strip_comments(src)
    used = set()
    for s in js_strings(src):
        # Template literals: `beatloop_${size}_toggle` -> beatloop_N_toggle
        s2 = re.sub(r"\$\{[^}]*\}", "N", s)
        n = names.match(s2) or names.match(s)
        if n:
            used.add(n)
    for pre, suf, n in names.templ:
        q = "[\"'`]"
        if re.search(q + re.escape(pre) + q, src) and (not suf or re.search(re.escape(suf) + q, src)):
            used.add(n)
    for cls, keys in COMPONENT_KEYS.items():
        if re.search(r"\bcomponents\." + cls + r"\b", src):
            used.update(k for k in keys)
    api = {k: collections.Counter(rx.findall(src)) for k, rx in API.items()}
    return used, api


# ---- main ------------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dts", default=os.path.expanduser("~/git/mixxx/res/controllers/mixxx-controls.d.ts"))
    ap.add_argument("--mappings", default="/usr/share/mixxx/controllers")
    ap.add_argument("--out", default="usage.json")
    a = ap.parse_args()

    controls = parse_dts(a.dts)
    names = Names(controls)

    xmls = sorted(glob.glob(os.path.join(a.mappings, "*.xml")))
    refs = collections.Counter()
    parsed = {}
    for x in xmls:
        parsed[x] = read_xml(x)
        refs.update(set(parsed[x]["scripts"]))
    libs = {f for f, n in refs.items() if n >= 3} | {"common-controller-scripts.js"}

    per_name = collections.defaultdict(lambda: {"xml_in": collections.defaultdict(set),
                                                 "xml_out": collections.defaultdict(set),
                                                 "js": set()})
    unknown = collections.Counter()
    options = collections.Counter()
    options_maps = collections.defaultdict(set)
    api_calls = collections.defaultdict(collections.Counter)
    api_maps = collections.defaultdict(set)
    maps = []
    for x, info in parsed.items():
        mid = os.path.basename(x)
        kind = mid.rsplit(".", 2)[-2]
        own = [s for s in info["scripts"] if s not in libs]
        uses_components = "midi-components-0.0.js" in info["scripts"]
        n_direct = n_script = 0
        for i in info["inputs"]:
            for o in i["options"]:
                options[o.lower()] += 1
                options_maps[o.lower()].add(mid)
            if i["script"]:
                n_script += 1
                continue
            n_direct += 1
            n = names.match(i["key"])
            if n:
                per_name[n]["xml_in"][norm_group(i["group"])].add(mid)
            else:
                unknown[(norm_group(i["group"]), i["key"])] += 1
        for o in info["outputs"]:
            n = names.match(o["key"])
            if n:
                per_name[n]["xml_out"][norm_group(o["group"])].add(mid)
            else:
                unknown[(norm_group(o["group"]), o["key"] + " (output)")] += 1
        js_used = set()
        js_lines = 0
        for s in own:
            p = os.path.join(a.mappings, s)
            if not os.path.exists(p):
                continue
            src = open(p, encoding="utf-8", errors="replace").read()
            js_lines += src.count("\n")
            used, api = js_usage(src, names)
            js_used |= used
            for k, c in api.items():
                for fn, cnt in c.items():
                    api_calls[f"{k}.{fn}"][mid] += cnt
                    api_maps[f"{k}.{fn}"].add(mid)
        for n in js_used:
            per_name[n]["js"].add(mid)
        maps.append({
            "file": mid, "kind": kind, "name": info["name"], "own_scripts": own,
            "components": uses_components, "inputs_direct": n_direct,
            "inputs_script": n_script, "outputs": len(info["outputs"]), "js_lines": js_lines,
        })

    result = {
        "controls": controls,
        "mappings": maps,
        "libs": sorted(libs),
        # name -> {"xml_in": {group: [mapping, ...]}, "xml_out": {...}, "js": [mapping, ...]}
        "usage": {n: {"xml_in": {g: sorted(m) for g, m in d["xml_in"].items()},
                      "xml_out": {g: sorted(m) for g, m in d["xml_out"].items()},
                      "js": sorted(d["js"])} for n, d in per_name.items()},
        "unknown_keys": [[g, k, c] for (g, k), c in unknown.most_common()],
        "options": {k: [v, len(options_maps[k])] for k, v in options.most_common()},
        "api": {k: [sum(v.values()), len(api_maps[k])] for k, v in
                sorted(api_calls.items(), key=lambda kv: -len(api_maps[kv[0]]))},
    }
    json.dump(result, open(a.out, "w"), indent=1)

    # Summary
    n = len(maps)
    din = sum(m["inputs_direct"] for m in maps)
    sin = sum(m["inputs_script"] for m in maps)
    print(f"documented controls: {len(controls)} "
          f"({collections.Counter(c['section'] for c in controls)})")
    print(f"mappings: {n} {collections.Counter(m['kind'] for m in maps)}")
    print(f"XML inputs: {din} direct, {sin} script-bound ({sin / (din + sin):.0%} script)")
    print(f"XML outputs: {sum(m['outputs'] for m in maps)} in "
          f"{sum(1 for m in maps if m['outputs'])} mappings")
    print(f"mappings with own JS: {sum(1 for m in maps if m['own_scripts'])}; "
          f"using components: {sum(1 for m in maps if m['components'])}; "
          f"no JS at all: {sum(1 for m in maps if not m['own_scripts'] and not m['components'])}")
    print("libs:", ", ".join(sorted(libs)))


if __name__ == "__main__":
    main()
