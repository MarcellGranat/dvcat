"""Pretend to train: a \\r-rewritten progress bar on stdout, a note on stderr, deterministic metrics."""
import json
import sys
import time
from pathlib import Path

import params

p = params.load()["train"]
epochs = int(p["epochs"])
print("note: using cpu", file=sys.stderr, flush=True)
for epoch in range(1, epochs + 1):
    time.sleep(0.4)
    bar = "#" * epoch + "." * (epochs - epoch)
    sys.stdout.write(f"\repoch {epoch}/{epochs} [{bar}]")
    sys.stdout.flush()
print()
print("training done", flush=True)
Path("model.txt").write_text(f"lr={p['lr']} epochs={epochs}\n")
effort = p["lr"] * epochs * 10  # grows with lr and epochs, saturating towards 0.99
auc = round(0.5 + 0.49 * effort / (1 + effort), 4)
Path("metrics.json").write_text(json.dumps({"auc": auc, "loss": round(1 - auc, 4)}, indent=2))
