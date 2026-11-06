"""Tiny params.yaml reader (two-level, flat values) so the fixture needs no packages."""
from pathlib import Path


def load(path: str = "params.yaml") -> dict[str, dict[str, float]]:
    out: dict[str, dict[str, float]] = {}
    section = ""
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        if not line.startswith(" "):
            section = line.rstrip(":")
            out[section] = {}
        else:
            key, value = line.strip().split(": ")
            out[section][key] = float(value)
    return out
