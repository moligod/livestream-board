from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

from .collectors import collect_trae, default_trae_encrypted
from .models import UsageRecord
from .reporting import export_records, print_table, summarize
from .sources import NATIVE_SOURCES, SOURCES
from .trae_crypto import prepare_trae


ROOT = Path(__file__).resolve().parents[1]
DATA = Path(os.environ.get("TOKENUSAGE_DATA_DIR", Path(os.environ.get("TEMP", ".")) / "TokenUsage"))
DATA.mkdir(parents=True, exist_ok=True)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="tokenusage", description="Agent 本地 Token 用量查询")
    sub = parser.add_subparsers(dest="command", required=True)

    prepare = sub.add_parser("prepare-trae", help="从运行中的 Trae 提取密钥并解密数据库")
    prepare.add_argument("--edition", choices=["trae-cn", "trae"], required=True)
    prepare.add_argument("--db", type=Path)
    prepare.add_argument("--output", type=Path)
    prepare.add_argument("--save-key", action="store_true", help="保存密钥文件（默认不保存）")

    report = sub.add_parser("report", help="汇总一个或全部数据源")
    report.add_argument("--source", choices=["all", *SOURCES], default="all")
    report.add_argument("--group-by", choices=["source", "project", "model", "date"], default="source")
    report.add_argument("--trae-cn-db", type=Path, default=DATA / "trae-cn.db")
    report.add_argument("--trae-db", type=Path, default=DATA / "trae.db")
    report.add_argument("--opencode-db", type=Path)
    report.add_argument("--codex-root", type=Path)
    report.add_argument("--export", type=Path, help="导出明细到 CSV 或 JSON")
    return parser


def _native_records(sources: list[str], codex_root: Path | None, opencode_db: Path | None) -> list[UsageRecord]:
    target = ROOT / "src-tauri" / "target"
    exe_name = "dashboard-json.exe" if os.name == "nt" else "dashboard-json"
    override = os.environ.get("TOKENUSAGE_COLLECTOR")
    executable = Path(override) if override else next((p for p in (target / "release" / exe_name, target / "debug" / exe_name) if p.exists()), None)
    if executable is None:
        raise RuntimeError("未找到 Rust 采集器，请先运行 start-web.cmd 构建项目")
    command = [str(executable), "--sources", ",".join(sources)]
    if codex_root is not None:
        command.extend(["--codex-root", str(codex_root)])
    if opencode_db is not None:
        command.extend(["--opencode-db", str(opencode_db)])
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=900, check=False)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "采集器运行失败")
    payload = json.loads(result.stdout)
    for source in sources:
        status = payload.get("source_status", {}).get(source)
        if status in ("error", "needs-permission"):
            raise RuntimeError(f"{source} 采集失败：{payload.get('source_errors', {}).get(source) or status}")
        if payload.get("source_errors", {}).get(source):
            print(f"{source}：{payload['source_errors'][source]}", file=sys.stderr)
    return [UsageRecord(**record) for record in payload.get("records", []) if record.get("source") in sources]


def main(argv: list[str] | None = None) -> int:
    os.environ.setdefault("PYTHONIOENCODING", "utf-8")
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    args = _parser().parse_args(argv)
    if args.command == "prepare-trae":
        encrypted = args.db or default_trae_encrypted(args.edition)
        output = args.output or DATA / f"{args.edition}.db"
        key_file = DATA / f"{args.edition}-key.json" if args.save_key else None
        prepare_trae(encrypted, output, args.edition, key_file)
        print(f"解密完成：{output}")
        if key_file:
            print(f"密钥已保存：{key_file}（请勿上传或分享）")
        return 0

    records = []
    native_sources = [source for source in NATIVE_SOURCES if args.source in ("all", source)]
    if native_sources:
        try:
            records.extend(_native_records(native_sources, args.codex_root, args.opencode_db))
        except (RuntimeError, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
            print(f"采集失败：{error}", file=sys.stderr)
            return 1
    if args.source in ("all", "trae-cn"):
        if not args.trae_cn_db.exists():
            print(f"提示：未找到 Trae CN 解密库：{args.trae_cn_db}")
        records.extend(collect_trae(args.trae_cn_db, "trae-cn"))
    if args.source in ("all", "trae"):
        if not args.trae_db.exists():
            print(f"提示：未找到 Trae 国际版解密库：{args.trae_db}")
        records.extend(collect_trae(args.trae_db, "trae"))
    print_table(summarize(records, args.group_by), args.group_by)
    print(f"\n共读取 {len(records):,} 条用量记录")
    if args.export:
        export_records(records, args.export)
        print(f"已导出：{args.export}")
    return 0
