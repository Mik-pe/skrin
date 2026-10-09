"""Regenerate paired resident-conversion results; reject incomplete evidence."""
import json
from pathlib import Path
import statistics

ROOT = Path(__file__).resolve().parent
metadata = json.loads((ROOT / "metadata.json").read_text())
samples = []
for run in metadata["runs"]:
    output = (ROOT / run["output"]).read_text().splitlines()
    values = {}
    for line in output:
        if not line.startswith("conversion,"):
            continue
        row = dict(field.split("=", 1) for field in line.split(",")[1:])
        mode = row["mode"]
        assert mode in ("single", "catalog") and mode not in values
        assert int(row.get("rows", row.get("entities"))) == run["rows"]
        if mode == "catalog":
            assert int(row["items"]) == run["rows"]
            assert int(row["postings"]) == 2 * run["rows"]
            assert row["exact_rows_indexes_and_retry"] == "verified"
        else:
            assert row["exact_state"] == "verified"
        values[mode] = {
            "elapsed_ms": float(row["elapsed_ms"]),
            "accounted_bytes": int(row["accounted_bytes"]),
        }
    assert set(values) == {"single", "catalog"}
    samples.append({**run, "values": values})

summary = []
for rows in metadata["sizes"]:
    for mode in ("single", "catalog"):
        groups = {}
        for revision in ("before", "after"):
            group = [r for r in samples if r["rows"] == rows and r["revision"] == revision]
            assert sorted(r["pair"] for r in group) == list(range(1, metadata["pairs"] + 1))
            times = [r["values"][mode]["elapsed_ms"] for r in group]
            assert all(t > 0 for t in times)
            groups[revision] = {"times_ms": times, "median_ms": statistics.median(times), "min_ms": min(times), "max_ms": max(times)}
        accounted = {r["values"][mode]["accounted_bytes"] for r in samples if r["rows"] == rows}
        assert len(accounted) == 1
        summary.append({"rows": rows, "mode": mode, "accounted_bytes": accounted.pop(), **groups, "median_speed_ratio": groups["before"]["median_ms"] / groups["after"]["median_ms"]})
(ROOT / "summary.json").write_text(json.dumps({"samples": samples, "summary": summary}, indent=2) + "\n")
print(json.dumps(summary, indent=2))
