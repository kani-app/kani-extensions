#!/usr/bin/env python3
"""Lists this repository's publishable extensions and decides which are newer than a live index.

  extensions.py check                    fail if a crate's Cargo.toml disagrees with its source
  extensions.py list                     print every publishable extension as JSON
  extensions.py pending [--index FILE]   print those newer than FILE's entries (all, without FILE)
"""

import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def yaml_field(text, key):
    match = re.search(rf'^{key}:[ \t]*"?([^"#\n]*?)"?[ \t]*(#.*)?$', text, re.MULTILINE)
    return match.group(1) if match else None


def yaml_extensions():
    for path in sorted(ROOT.glob("*.yaml")):
        text = path.read_text()
        ext = {key: yaml_field(text, key) for key in ("id", "name", "version")}
        missing = [key for key, value in ext.items() if not value]
        if missing:
            sys.exit(f"{path.name}: no top-level {', '.join(missing)}")
        yield {**ext, "format": "yaml", "source": path.name}


def source_metadata(crate_dir):
    text = (crate_dir / "src" / "lib.rs").read_text()
    start = text.find("fn metadata()")
    if start < 0:
        return {}
    body = text[start:]
    patterns = {
        "id": r'\bid:\s*"([^"]+)"',
        "name": r'\bname:\s*"([^"]+)"',
        "version": r'ext_version!\(\s*"([^"]+)"\s*\)',
    }
    found = {}
    for key, pattern in patterns.items():
        match = re.search(pattern, body)
        if match:
            found[key] = match.group(1)
    return found


def crate_extensions():
    members = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["members"]
    for member in members:
        crate_dir = ROOT / member
        package = tomllib.loads((crate_dir / "Cargo.toml").read_text())["package"]
        metadata = package.get("metadata", {})
        if metadata.get("repo", True) is False:
            continue
        yield {
            "id": metadata.get("id"),
            "name": metadata.get("name"),
            "version": package["version"],
            "format": "wasm",
            "source": member,
        }


def extensions():
    return [*yaml_extensions(), *crate_extensions()]


def check():
    problems = []
    for ext in crate_extensions():
        declared = source_metadata(ROOT / ext["source"])
        for key in ("id", "name", "version"):
            if not ext[key]:
                problems.append(f"{ext['source']}: Cargo.toml has no {key} for the extension")
            elif declared.get(key) != ext[key]:
                problems.append(
                    f"{ext['source']}: Cargo.toml says {key} {ext[key]!r}, "
                    f"metadata() says {declared.get(key)!r}"
                )
    ids = [ext["id"] for ext in extensions()]
    problems += [f"extension id {i!r} is used twice" for i in sorted({i for i in ids if ids.count(i) > 1})]
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    return 1 if problems else 0


def semver_key(version):
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?", version)
    if not match:
        sys.exit(f"{version!r} is not a semver version")
    major, minor, patch, pre = match.groups()
    release = (int(major), int(minor), int(patch))
    if pre is None:
        return release, (1,)
    parts = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
    return release, (0, *parts)


def pending(index_path):
    live = {}
    if index_path:
        live = {e["id"]: e["version"] for e in json.loads(Path(index_path).read_text())["extensions"]}
    out = []
    for ext in extensions():
        current = live.get(ext["id"])
        if current is None or semver_key(ext["version"]) > semver_key(current):
            out.append(ext)
        elif semver_key(ext["version"]) < semver_key(current):
            print(
                f"warning: {ext['id']} {ext['version']} is older than the published {current}",
                file=sys.stderr,
            )
    return out


def main(argv):
    if argv[:1] == ["check"]:
        return check()
    if argv[:1] == ["list"]:
        print(json.dumps(extensions()))
        return 0
    if argv[:1] == ["pending"]:
        index = argv[2] if argv[1:2] == ["--index"] and len(argv) > 2 else None
        print(json.dumps(pending(index)))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
