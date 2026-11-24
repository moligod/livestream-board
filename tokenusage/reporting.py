from __future__ import annotations

import csv
import json
from collections import defaultdict
from datetime import datetime
from pathlib import Path

from .models import UsageRecord


def summarize(records: list[UsageRecord], group_by: str = "source") -> list[dict]:
    groups: dict[str, dict] = defaultdict(lambda: {
        "sessions": 0, "input_tokens": 0, "output_tokens": 0,
        "reasoning_tokens": 0, "cache_read_tokens": 0,
        "cache_write_tokens": 0, "total_tokens": 0, "cost": 0.0,
    })
    for record in records:
        if group_by == "date":
            key = datetime.fromtimestamp(record.timestamp).strftime("%Y-%m-%d") if record.timestamp else "unknown"
        else:
            key = str(getattr(record, group_by, "unknown") or "unknown")
        item = groups[key]
        item["sessions"] += 1
        for field in ("input_tokens", "output_tokens", "reasoning_tokens", "cache_read_tokens", "cache_write_tokens", "total_tokens"):
            item[field] += getattr(record, field)
        item["cost"] += record.cost
    return [{group_by: key, **value} for key, value in sorted(groups.items(), key=lambda item: -item[1]["total_tokens"])]


def print_table(rows: list[dict], group_by: str) -> None:
    print(f"{'分组':<28} {'记录':>7} {'Input':>14} {'Output':>12} {'Reasoning':>12} {'Cache Read':>14} {'Total':>15}")
    print("-" * 110)
    for row in rows:
        print(
            f"{str(row[group_by])[:27]:<28} {row['sessions']:>7,} {row['input_tokens']:>14,} "
            f"{row['output_tokens']:>12,} {row['reasoning_tokens']:>12,} "
            f"{row['cache_read_tokens']:>14,} {row['total_tokens']:>15,}"
        )


def export_records(records: list[UsageRecord], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    rows = [record.to_dict() for record in records]
    if path.suffix.lower() == ".json":
        path.write_text(json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
        return
    with path.open("w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]) if rows else list(UsageRecord.__dataclass_fields__))
        writer.writeheader()
        writer.writerows(rows)
