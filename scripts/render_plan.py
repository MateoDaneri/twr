#!/usr/bin/env python3
"""Render the local project plan. Reads only files within this project."""
import json
import html
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
data = json.loads((ROOT / "docs/plan.json").read_text())
template = (ROOT / "docs/plan.template.html").read_text()
encoded = json.dumps(data, ensure_ascii=False).replace("<", "\\u003c")
inventory = json.loads((ROOT / "Resources/Contracts/setup-inventory.json").read_text())
keymaps = json.loads((ROOT / "Resources/Contracts/keymaps.json").read_text())
progress = json.loads((ROOT / "Resources/Contracts/integration-progress.json").read_text())
replacements = {
    "__PLAN_DATA__": encoded,
    "__PLAN_DATE__": html.escape(data["date"]),
    "__PLAN_REVISION__": str(data["revision"]),
    "__PIN_COUNT__": str(len(inventory["locked_plugins"])),
    "__KEYMAP_COUNT__": str(len(keymaps)),
    "__TEST_COUNT__": str(progress.get("current_source_checks", {}).get("library_tests", progress.get("current_verified_checkpoint", progress["last_verified_checkpoint"])["checks"]["library_tests"])["total"]),
}
for token, value in replacements.items():
    template = template.replace(token, value)
(ROOT / "docs/PLAN.html").write_text(template)
print(ROOT / "docs/PLAN.html")
