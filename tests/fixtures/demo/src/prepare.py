"""Drop rows with a missing value."""
import csv
from pathlib import Path

rows = [r for r in csv.DictReader(Path("data/raw.csv").open()) if r["y"] != ""]
with Path("data/prepared.csv").open("w", newline="") as f:
    w = csv.DictWriter(f, fieldnames=["x", "y"])
    w.writeheader()
    w.writerows(rows)
