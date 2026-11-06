"""Add a scaled copy of x."""
import csv
from pathlib import Path

import params

scale = params.load()["features"]["scale"]
rows = list(csv.DictReader(Path("data/prepared.csv").open()))
with Path("data/features.csv").open("w", newline="") as f:
    w = csv.DictWriter(f, fieldnames=["x", "y", "x_scaled"])
    w.writeheader()
    for r in rows:
        w.writerow({**r, "x_scaled": float(r["x"]) * scale})
