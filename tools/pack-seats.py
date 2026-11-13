#!/usr/bin/env python3
"""Puts three format 2 place packs, each baked for one seat, into one format 3 pack.

    tools/pack-seats.py FRONT_PACK MIDDLE_PACK BACK_PACK OUT_PACK --version V [--zip DIR] [--default SEAT]

Each source is a pack folder made by tools/pack-encode.py (format 2) from a bake
done at one seat's eye: its own mesh (UV atlas) and its own textures. The result
carries all three, in one folder: every file renamed `<seat>-<name>`, and a
manifest with a `seat_sets` entry for each seat (its mesh, its textures and its
basis_gain). The contract is `.local/pack-format.md` ("Place pack format v3") and
docs/ENVIRONMENTS.md. Needs only Python 3.

The sources must agree on the room: id, name, screen, seats and their eyes. The
version is given, the default seat is the middle one unless `--default` says
another. With --zip the pack is also written as DIR/<id>-<version>.zip, laid out
as tools/fetch-places.sh wants it (everything inside `<id>/`), and the zip's
size and sha256 are printed (the sha256 goes into packaging/places.lock).
"""
import argparse
import hashlib
import json
import os
import shutil
import sys
import zipfile

SEATS = ("front", "middle", "back")


def die(msg):
    sys.exit("pack-seats: " + msg)


def read_manifest(folder):
    path = os.path.join(folder, "manifest.json")
    try:
        with open(path, encoding="utf-8") as f:
            m = json.load(f)
    except (OSError, ValueError) as e:
        die("%s: %s" % (path, e))
    if m.get("format") != 2:
        die("%s: format %r (a format 2 pack wanted)" % (path, m.get("format")))
    return m


def make_zip(out_dir, zip_dir, pack_id, version):
    os.makedirs(zip_dir, exist_ok=True)
    zpath = os.path.join(zip_dir, "%s-%s.zip" % (pack_id, version))
    with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for n in sorted(os.listdir(out_dir)):
            zi = zipfile.ZipInfo("%s/%s" % (pack_id, n), date_time=(1980, 1, 1, 0, 0, 0))
            zi.compress_type = zipfile.ZIP_DEFLATED
            zi.external_attr = 0o644 << 16
            with open(os.path.join(out_dir, n), "rb") as f:
                z.writestr(zi, f.read(), compresslevel=9)
    h = hashlib.sha256()
    with open(zpath, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return zpath, h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description="Three format 2 packs, one per seat -> one format 3 pack.")
    ap.add_argument("front")
    ap.add_argument("middle")
    ap.add_argument("back")
    ap.add_argument("out", help="format 3 pack folder to write (replaced if it exists)")
    ap.add_argument("--version", required=True, help="the new pack's version, e.g. 2026.11.13-1")
    ap.add_argument("--default", default="middle", choices=SEATS, help="the seat the pack starts in")
    ap.add_argument("--zip", metavar="DIR", help="also write DIR/<id>-<version>.zip and print its sha256")
    a = ap.parse_args()

    sources = dict(zip(SEATS, (a.front, a.middle, a.back)))
    manifests = {seat: read_manifest(folder) for seat, folder in sources.items()}
    first = manifests["middle"]
    for seat, m in manifests.items():
        for key in ("id", "name", "screen", "seats"):
            if m.get(key) != first.get(key):
                die("%s pack and middle pack disagree on %r" % (seat, key))
    names = [s["name"] for s in first["seats"]]
    if sorted(names) != sorted(SEATS):
        die("the room's seats are %r (front, middle and back wanted)" % names)

    out = os.path.abspath(a.out)
    if os.path.isdir(out):
        shutil.rmtree(out)
    os.makedirs(out)

    sets = {}
    for seat, folder in sources.items():
        m = manifests[seat]
        t = dict(m["textures"])
        mesh = dict(m["mesh"])
        files = [mesh["file"], t["house"], t["dark"]] + list(t["basis"])
        total = 0
        renamed = {}
        for name in files:
            new = "%s-%s" % (seat, name)
            shutil.copyfile(os.path.join(folder, name), os.path.join(out, new))
            renamed[name] = new
            if name != mesh["file"]:
                total += os.path.getsize(os.path.join(out, new))
        if total != t["bytes"]:
            die("%s: its texture files add up to %d bytes, its manifest says %r" % (seat, total, t["bytes"]))
        mesh["file"] = renamed[mesh["file"]]
        t["house"], t["dark"] = renamed[t["house"]], renamed[t["dark"]]
        t["basis"] = [renamed[n] for n in t["basis"]]
        sets[seat] = {"mesh": mesh, "textures": t, "basis_gain": m["basis_gain"]}

    shutil.copyfile(os.path.join(a.middle, first.get("credits", "CREDITS.txt")), os.path.join(out, "CREDITS.txt"))
    manifest = {
        "format": 3,
        "id": first["id"],
        "name": first["name"],
        "version": a.version,
        "screen": first["screen"],
        "seats": first["seats"],
        "default_seat": names.index(a.default),
        "seat_sets": sets,
        "basis_gain": manifests[a.default]["basis_gain"],
        "sky": None,
        "credits": "CREDITS.txt",
    }
    with open(os.path.join(out, "manifest.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")

    on_disk = sum(os.path.getsize(os.path.join(out, n)) for n in os.listdir(out))
    print("wrote %s: %d files, %.2f MB" % (out, len(os.listdir(out)), on_disk / 1048576))
    for seat in SEATS:
        s = sets[seat]
        print("  %-6s %d triangles, textures %.2f MB, basis_gain %g" % (
            seat, s["mesh"]["triangles"], s["textures"]["bytes"] / 1048576, s["basis_gain"]))
    if a.zip:
        zpath, digest = make_zip(out, a.zip, manifest["id"], a.version)
        print("zip %s  %.2f MB  (%d bytes)" % (zpath, os.path.getsize(zpath) / 1048576, os.path.getsize(zpath)))
        print("sha256 %s" % digest)


if __name__ == "__main__":
    main()
