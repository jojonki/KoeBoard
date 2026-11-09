#!/usr/bin/python3
"""Writes the license notices for one build target.

Usage: generate-licenses.py <rust-target> <output-file>
Run after `cargo build`, so the dependency sources are in the cargo registry.
"""
import collections
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PREFIXES = ("LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE")
MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read().strip()


def shipped_packages(target):
    """Packages linked into the program: normal dependencies only, for this target."""
    output = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", target], cwd=ROOT
    )
    meta = json.loads(output)
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    packages = {p["id"]: p for p in meta["packages"]}
    root = meta["resolve"]["root"]
    seen, stack = set(), [root]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    seen.discard(root)
    return sorted((packages[i] for i in seen), key=lambda p: (p["name"], p["version"]))


def fallback_text(package):
    """Some crates publish no license file; state what their manifest declares."""
    expression = package["license"] or "unspecified"
    text = f"Declared license: {expression}\nSource: {package.get('repository') or 'crates.io'}"
    if "MIT" in expression:
        authors = ", ".join(package["authors"]) or f"The {package['name']} authors"
        text += f"\n\nUsed under the MIT License.\n\nCopyright (c) {authors}\n\n{MIT}"
    return text.strip()


def main(target, out_path):
    by_text = collections.defaultdict(list)
    for package in shipped_packages(target):
        directory = os.path.dirname(package["manifest_path"])
        names = sorted(
            n for n in os.listdir(directory)
            if n.upper().startswith(PREFIXES) and os.path.isfile(os.path.join(directory, n))
        )
        texts = [read(os.path.join(directory, n)) for n in names] or [fallback_text(package)]
        for text in texts:
            by_text[text].append(f"{package['name']} {package['version']}")

    rule = "=" * 78
    parts = [
        "KoeBoard\n\n" + read(os.path.join(ROOT, "LICENSE")),
        "ONNX Runtime\nhttps://github.com/microsoft/onnxruntime\n"
        "Its own third-party notices: "
        "https://github.com/microsoft/onnxruntime/blob/main/ThirdPartyNotices.txt\n\n"
        f"MIT License\n\nCopyright (c) Microsoft Corporation\n\n{MIT}".strip(),
    ]
    if "windows" in target:
        parts.append("Noto Sans JP\n\n" + read(os.path.join(ROOT, "assets/fonts/OFL.txt")))
    for text, users in by_text.items():
        parts.append("Used by: " + ", ".join(users) + "\n\n" + text)

    with open(out_path, "w", encoding="utf-8") as f:
        f.write(f"\n\n{rule}\n\n".join(parts) + "\n")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
