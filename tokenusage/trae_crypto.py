from __future__ import annotations

import ctypes
import ctypes.wintypes as wt
import hashlib
import hmac
import json
import os
import re
import struct
import subprocess
from pathlib import Path

PAGE_SIZE = 4096
RESERVE_SIZE = 80
SQLITE_HEADER = b"SQLite format 3\x00"
kernel32 = ctypes.windll.kernel32


class MBI(ctypes.Structure):
    _fields_ = [
        ("BaseAddress", ctypes.c_uint64), ("AllocationBase", ctypes.c_uint64),
        ("AllocationProtect", wt.DWORD), ("_pad1", wt.DWORD),
        ("RegionSize", ctypes.c_uint64), ("State", wt.DWORD),
        ("Protect", wt.DWORD), ("Type", wt.DWORD), ("_pad2", wt.DWORD),
    ]


def _mac_key(key: bytes, salt: bytes) -> bytes:
    return hashlib.pbkdf2_hmac("sha512", key, bytes(value ^ 0x3A for value in salt), 2, dklen=32)


def _valid_key(key: bytes, page: bytes) -> bool:
    digest = hmac.new(_mac_key(key, page[:16]), page[16: PAGE_SIZE - 64], hashlib.sha512)
    digest.update(struct.pack("<I", 1))
    return hmac.compare_digest(digest.digest(), page[PAGE_SIZE - 64: PAGE_SIZE])


def _candidate_pids(process_names: list[str]) -> list[int]:
    found = []
    for name in process_names:
        try:
            output = subprocess.check_output(
                f'tasklist /FI "IMAGENAME eq {name}" /FO CSV /NH', shell=True,
                text=True, errors="replace", timeout=5,
            )
        except Exception:
            continue
        for line in output.splitlines():
            parts = line.strip('"').split('\",\"')
            if len(parts) >= 2:
                try:
                    found.append(int(parts[1]))
                except ValueError:
                    pass
    return found


def _regions(handle) -> list[tuple[int, int]]:
    result, address, mbi = [], 0, MBI()
    readable = {0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80}
    while address < 0x7FFFFFFFFFFF:
        if not kernel32.VirtualQueryEx(handle, ctypes.c_uint64(address), ctypes.byref(mbi), ctypes.sizeof(mbi)):
            break
        if mbi.State == 0x1000 and mbi.Protect in readable and 0 < mbi.RegionSize < 500 * 1024 * 1024:
            result.append((mbi.BaseAddress, mbi.RegionSize))
        next_address = mbi.BaseAddress + mbi.RegionSize
        if next_address <= address:
            break
        address = next_address
    return result


def scan_key(db_path: Path, edition: str) -> str:
    with db_path.open("rb") as handle:
        page = handle.read(PAGE_SIZE)
    salt_hex = page[:16].hex()
    names = ["Trae CN.exe"] if edition == "trae-cn" else ["Trae.exe", "Trae AI.exe", "Trae International.exe"]
    pids = _candidate_pids(names)
    if not pids:
        raise RuntimeError("未找到 Trae 进程，请启动对应版本后重试")
    patterns = [re.compile(rb"x'([0-9a-fA-F]{64,192})'"), re.compile(rb"([0-9a-fA-F]{64})")]
    for pid in pids:
        handle = kernel32.OpenProcess(0x0010 | 0x0400, False, pid)
        if not handle:
            error = kernel32.GetLastError()
            if error == 5:
                raise PermissionError("读取 Trae 进程被拒绝，请在管理员 PowerShell 中运行")
            continue
        try:
            for base, size in _regions(handle):
                buffer = ctypes.create_string_buffer(size)
                read = ctypes.c_size_t()
                if not kernel32.ReadProcessMemory(handle, ctypes.c_uint64(base), buffer, size, ctypes.byref(read)):
                    continue
                data = buffer.raw[:read.value]
                for pattern in patterns:
                    for match in pattern.finditer(data):
                        text = (match.group(1) if match.lastindex else match.group(0)).decode()
                        if len(text) < 64 or (len(text) >= 96 and text[-32:].lower() != salt_hex.lower()):
                            continue
                        try:
                            key = bytes.fromhex(text[:64])
                        except ValueError:
                            continue
                        if _valid_key(key, page):
                            return key.hex()
        finally:
            kernel32.CloseHandle(handle)
    raise RuntimeError("未找到可通过 HMAC 验证的 SQLCipher 密钥")


def decrypt_database(db_path: Path, output: Path, key_hex: str) -> None:
    from Crypto.Cipher import AES

    key = bytes.fromhex(key_hex)
    with db_path.open("rb") as source:
        first_page = source.read(PAGE_SIZE)
    if not _valid_key(key, first_page):
        raise ValueError("密钥 HMAC 验证失败")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    with db_path.open("rb") as source, temporary.open("wb") as target:
        page_number = 1
        while page := source.read(PAGE_SIZE):
            if len(page) < PAGE_SIZE:
                page += bytes(PAGE_SIZE - len(page))
            iv = page[PAGE_SIZE - RESERVE_SIZE: PAGE_SIZE - RESERVE_SIZE + 16]
            start = 16 if page_number == 1 else 0
            decrypted = AES.new(key, AES.MODE_CBC, iv).decrypt(page[start: PAGE_SIZE - RESERVE_SIZE])
            if page_number == 1:
                target.write(SQLITE_HEADER + decrypted + bytes(RESERVE_SIZE))
            else:
                target.write(decrypted + bytes(RESERVE_SIZE))
            page_number += 1
    os.replace(temporary, output)


def prepare_trae(db_path: Path, output: Path, edition: str, key_file: Path | None = None) -> None:
    key_hex = scan_key(db_path, edition)
    if key_file:
        key_file.parent.mkdir(parents=True, exist_ok=True)
        key_file.write_text(json.dumps({"db_path": str(db_path), "enc_key": key_hex}, indent=2), encoding="utf-8")
    decrypt_database(db_path, output, key_hex)
