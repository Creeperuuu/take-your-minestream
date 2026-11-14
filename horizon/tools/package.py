#!/usr/bin/env python3
"""Package the mod as a source-only `.hnmod`, the file Horizon's launcher and mod registry take.

The archive holds `horizon-native.json`, the bundle's `.rs` and `.toml` files as `mixins/<id>/` and the repository's
LICENSE as `mixins/<id>/LICENSE`, with fixed dates and modes: the same sources always give the same bytes and
the same SHA-256. Nothing is compiled here; the launcher builds it on the pinned Horizon revision.
Based on the registry's packager (horizon-rust/horizon-mods, native/tools/native.py) and the mod template's.

    python3 tools/package.py          # writes target/<id>-<version>.hnmod and prints its size and SHA-256
"""
import hashlib
import json
from pathlib import Path
import re
import sys
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
REVISION = re.compile(r"^[0-9a-f]{40}$")


def fail(message):
    sys.exit(f"package.py: {message}")


def package():
    compat = json.loads((ROOT / "compatibility.json").read_text())
    for key in ("horizon_revision", "horizon_mixins_revision"):
        if not REVISION.match(compat.get(key, "")):
            fail(f"compatibility.json: {key} must be a full 40-character commit")
    for key in ("game", "hooks"):
        if key not in compat:
            fail(f"compatibility.json: {key} is missing")

    manifests = sorted((ROOT / "mixins").glob("*/horizon-mixin.toml"))
    if len(manifests) != 1:
        fail("a .hnmod holds one bundle: keep exactly one mixins/<id>/horizon-mixin.toml")
    bundle = manifests[0].parent
    mixin = tomllib.loads(manifests[0].read_text())
    mod = mixin["mod"]
    if bundle.name != mod["id"]:
        fail(f"rename mixins/{bundle.name}/ to mixins/{mod['id']}/ (the registry finds the bundle by id)")
    if not re.fullmatch(r"[a-z0-9_]+", mod["id"]):
        fail("the registry takes ids of lowercase letters, digits and _ only")
    if mixin.get("compatibility", {}).get("horizon_revision") != compat["horizon_revision"]:
        fail("horizon-mixin.toml and compatibility.json pin different Horizon revisions")

    # Every file the manifest names must be in the bundle.
    named = [m["source"] for m in mixin.get("module", [])] + [i["helper"] for i in mixin.get("injection", [])]
    missing = sorted({n for n in named if not (bundle / n).is_file()})
    if missing:
        fail(f"horizon-mixin.toml names files that are not in the bundle: {', '.join(missing)}")

    manifest = {"schema": 1, "id": mod["id"], "version": str(mod["version"]), "game": compat["game"],
                "horizon": compat["horizon_revision"], "mixins": compat["horizon_mixins_revision"],
                "hooks": compat["hooks"]}
    files = {"horizon-native.json": (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()}
    for path in sorted(bundle.iterdir()):
        if path.is_file() and path.suffix in (".rs", ".toml"):
            files[f"mixins/{mod['id']}/{path.name}"] = path.read_bytes()
    # The licence travels with the sources (Horizon's bundle reader accepts LICENSE* there).
    for candidate in (ROOT / "LICENSE", ROOT.parent / "LICENSE"):
        if candidate.is_file():
            files[f"mixins/{mod['id']}/LICENSE"] = candidate.read_bytes()
            break

    out = ROOT / "target" / f"{mod['id']}-{mod['version']}.hnmod"
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, content in sorted(files.items()):
            info = zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            archive.writestr(info, content)

    # Read it back: the manifest, and nothing in it but what the launcher accepts.
    with zipfile.ZipFile(out) as archive:
        names = archive.namelist()
        read = json.loads(archive.read("horizon-native.json"))
    prefix = f"mixins/{mod['id']}/"
    bad = [n for n in names if n != "horizon-native.json"
           and not (n.startswith(prefix) and (n.endswith((".rs", ".toml")) or n == prefix + "LICENSE"))]
    if bad or read != manifest:
        fail(f"the archive holds something the launcher would refuse: {bad or read}")

    print(json.dumps({"file": str(out), "size": out.stat().st_size,
                      "sha256": hashlib.sha256(out.read_bytes()).hexdigest(),
                      "files": names}, indent=2))


if __name__ == "__main__":
    package()
