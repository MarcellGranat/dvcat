"""Pretend to train: sleeps per epoch, prints progress, writes deterministic metrics."""
import json
import sys
import time
from pathlib import Path

import params

p = params.load()["train"]
epochs = int(p["epochs"])
for epoch in range(1, epochs + 1):
    time.sleep(0.4)
    print(f"epoch {epoch}/{epochs}", flush=True)
    sys.stdout.write(f"\rprogress {epoch * 100 // epochs}%")
    sys.stdout.flush()
print()
Path("model.txt").write_text(f"lr={p['lr']} epochs={epochs}\n")
auc = round(min(0.99, 0.5 + p["lr"] * epochs * 8), 4)
Path("metrics.json").write_text(json.dumps({"auc": auc, "loss": round(1 - auc, 4)}, indent=2))
