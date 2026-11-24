use aes::Aes256;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::Sha512;
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const PAGE_SIZE: usize = 4096;
const RESERVE_SIZE: usize = 80;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const HMAC_SIZE: usize = 64;

type HmacSha512 = Hmac<Sha512>;
type Aes256Cbc = cbc::Decryptor<Aes256>;

fn hmac_key(key: &[u8], salt: &[u8]) -> [u8; 32] {
    let transformed_salt: Vec<u8> = salt.iter().map(|value| value ^ 0x3a).collect();
    let mut derived = [0_u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(key, &transformed_salt, 2, &mut derived);
    derived
}

fn valid_key(key: &[u8; 32], page: &[u8]) -> bool {
    if page.len() < PAGE_SIZE {
        return false;
    }
    let derived = hmac_key(key, &page[..16]);
    let Ok(mut mac) = HmacSha512::new_from_slice(&derived) else {
        return false;
    };
    mac.update(&page[16..PAGE_SIZE - HMAC_SIZE]);
    mac.update(&1_u32.to_le_bytes());
    mac.verify_slice(&page[PAGE_SIZE - HMAC_SIZE..]).is_ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn decode_key(value: &[u8]) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut key = [0_u8; 32];
    for (index, pair) in value.chunks_exact(2).enumerate() {
        key[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(key)
}

fn candidate_keys(data: &[u8], salt: &[u8; 16]) -> Vec<[u8; 32]> {
    let mut candidates = Vec::new();
    let mut start = 0;
    while start < data.len() {
        while start < data.len() && hex_value(data[start]).is_none() {
            start += 1;
        }
        let run_start = start;
        while start < data.len() && hex_value(data[start]).is_some() {
            start += 1;
        }
        let run = &data[run_start..start];
        if run.len() < 64 || run.len() > 256 {
            continue;
        }
        let max_offset = run.len() - 64;
        for offset in 0..=max_offset {
            if run.len() >= 96
                && run[run.len() - 32..].eq_ignore_ascii_case(&hex_bytes(salt))
                && offset != 0
            {
                continue;
            }
            if let Some(key) = decode_key(&run[offset..offset + 64]) {
                if !candidates.iter().any(|existing| existing == &key) {
                    candidates.push(key);
                }
            }
        }
    }
    candidates
}

fn hex_bytes(value: &[u8; 16]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = Vec::with_capacity(32);
    for byte in value {
        output.push(HEX[(byte >> 4) as usize]);
        output.push(HEX[(byte & 0x0f) as usize]);
    }
    output
}

#[cfg(windows)]
fn process_ids(names: &[&str]) -> Vec<u32> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut ids = Vec::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return ids;
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        let length = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        let executable = String::from_utf16_lossy(&entry.szExeFile[..length]);
        if names
            .iter()
            .any(|name| executable.eq_ignore_ascii_case(name))
        {
            ids.push(entry.th32ProcessID);
        }
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { windows_sys::Win32::Foundation::CloseHandle(snapshot) };
    ids
}

#[cfg(not(windows))]
fn process_ids(_names: &[&str]) -> Vec<u32> {
    Vec::new()
}

#[cfg(windows)]
fn scan_memory_for_key(page: &[u8; PAGE_SIZE], names: &[&str]) -> Result<[u8; 32], String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED},
        System::{
            Diagnostics::Debug::ReadProcessMemory,
            Memory::{
                VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE,
                PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD,
                PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
            },
            Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
        },
    };

    let ids = process_ids(names);
    if ids.is_empty() {
        return Err("未找到对应的 Trae 进程，请先启动 Trae 后重试".into());
    }
    let salt = page[..16].try_into().expect("salt length");
    let mut access_denied = false;
    for pid in ids {
        let handle = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
        if handle.is_null() {
            access_denied |= unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
            continue;
        }
        let mut address = 0_usize;
        loop {
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let queried = unsafe {
                VirtualQueryEx(
                    handle,
                    address as *const core::ffi::c_void,
                    &mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if queried == 0 {
                break;
            }
            let next = (info.BaseAddress as usize).saturating_add(info.RegionSize);
            let protection = info.Protect;
            let readable = matches!(
                protection,
                PAGE_READONLY
                    | PAGE_READWRITE
                    | PAGE_WRITECOPY
                    | PAGE_EXECUTE
                    | PAGE_EXECUTE_READ
                    | PAGE_EXECUTE_READWRITE
                    | PAGE_EXECUTE_WRITECOPY
            ) && protection & (PAGE_GUARD | PAGE_NOACCESS) == 0;
            if info.State == MEM_COMMIT
                && readable
                && info.RegionSize > 0
                && info.RegionSize < 500 * 1024 * 1024
            {
                let mut buffer = vec![0_u8; info.RegionSize];
                let mut read = 0_usize;
                let ok = unsafe {
                    ReadProcessMemory(
                        handle,
                        info.BaseAddress as *const core::ffi::c_void,
                        buffer.as_mut_ptr() as *mut core::ffi::c_void,
                        buffer.len(),
                        &mut read,
                    )
                };
                if ok != 0 {
                    for key in candidate_keys(&buffer[..read], &salt) {
                        if valid_key(&key, page) {
                            unsafe { CloseHandle(handle) };
                            return Ok(key);
                        }
                    }
                }
            }
            if next <= address {
                break;
            }
            address = next;
        }
        unsafe { CloseHandle(handle) };
    }
    if access_denied {
        Err(
            "读取 Trae 进程被拒绝，请关闭客户端后以管理员身份运行 TokenUsage，并保持 Trae 运行"
                .into(),
        )
    } else {
        Err("未找到可通过 HMAC 验证的 Trae SQLCipher 密钥，请保持对应版本 Trae 运行后重试".into())
    }
}

#[cfg(not(windows))]
fn scan_memory_for_key(_page: &[u8; PAGE_SIZE], _names: &[&str]) -> Result<[u8; 32], String> {
    Err("Trae 原始数据库解密目前仅支持 Windows".into())
}

fn edition_process_names(edition: &str) -> &'static [&'static str] {
    match edition {
        "trae-cn" => &["Trae CN.exe", "Trae.exe"],
        _ => &["Trae.exe", "Trae AI.exe", "Trae International.exe"],
    }
}

fn stamp_path(output: &Path) -> PathBuf {
    output.with_extension("stamp")
}

fn key_cache_path(edition: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("TokenUsage"))
        .join("TokenUsage")
        .join("keys")
        .join(format!("{edition}.dpapi"))
}

#[cfg(windows)]
fn load_cached_key(edition: &str) -> Option<[u8; 32]> {
    use std::ptr::null_mut;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB},
    };

    let bytes = fs::read(key_cache_path(edition)).ok()?;
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.cbData != 32 || output.pbData.is_null() {
        return None;
    }
    let decrypted = unsafe { std::slice::from_raw_parts(output.pbData, 32) };
    let mut key = [0_u8; 32];
    key.copy_from_slice(decrypted);
    unsafe { LocalFree(output.pbData.cast()) };
    Some(key)
}

#[cfg(not(windows))]
fn load_cached_key(_edition: &str) -> Option<[u8; 32]> {
    None
}

#[cfg(windows)]
fn save_cached_key(edition: &str, key: &[u8; 32]) {
    use std::ptr::null_mut;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB},
    };

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: key.len() as u32,
        pbData: key.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.cbData == 0 || output.pbData.is_null() {
        return;
    }
    let encrypted = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let path = key_cache_path(edition);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, encrypted);
    unsafe { LocalFree(output.pbData.cast()) };
}

#[cfg(not(windows))]
fn save_cached_key(_edition: &str, _key: &[u8; 32]) {}

fn seed_key_cache_if_available(path: &Path, edition: &str) {
    if load_cached_key(edition).is_some() || process_ids(edition_process_names(edition)).is_empty()
    {
        return;
    }
    let Ok(mut source) = File::open(path).map(BufReader::new) else {
        return;
    };
    let mut first_page = [0_u8; PAGE_SIZE];
    if source.read_exact(&mut first_page).is_err() {
        return;
    }
    if let Ok(key) = scan_memory_for_key(&first_page, edition_process_names(edition)) {
        save_cached_key(edition, &key);
    }
}

fn source_stamp(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos())
        .unwrap_or_default();
    Ok(format!("{}:{}", metadata.len(), modified))
}

fn is_sqlite(path: &Path) -> Result<bool, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut header = [0_u8; 16];
    let read = file.read(&mut header).map_err(|error| error.to_string())?;
    Ok(read == SQLITE_HEADER.len() && &header == SQLITE_HEADER)
}

fn decrypt_database(input: &Path, output: &Path, key: &[u8; 32]) -> Result<(), String> {
    let mut source = BufReader::new(File::open(input).map_err(|error| error.to_string())?);
    let temporary = output.with_extension("db.tmp");
    let mut target = BufWriter::new(File::create(&temporary).map_err(|error| error.to_string())?);
    let mut page_number = 1_u64;
    loop {
        let mut page = [0_u8; PAGE_SIZE];
        let mut read = 0_usize;
        while read < PAGE_SIZE {
            let count = source
                .read(&mut page[read..])
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read == 0 {
            break;
        }
        let iv = &page[PAGE_SIZE - RESERVE_SIZE..PAGE_SIZE - RESERVE_SIZE + 16];
        let start = if page_number == 1 { 16 } else { 0 };
        let mut encrypted = page[start..PAGE_SIZE - RESERVE_SIZE].to_vec();
        let decrypted = Aes256Cbc::new_from_slices(key, iv)
            .map_err(|error| error.to_string())?
            .decrypt_padded_mut::<NoPadding>(&mut encrypted)
            .map_err(|error| error.to_string())?;
        if page_number == 1 {
            target
                .write_all(SQLITE_HEADER)
                .map_err(|error| error.to_string())?;
        }
        target
            .write_all(decrypted)
            .map_err(|error| error.to_string())?;
        target
            .write_all(&[0_u8; RESERVE_SIZE])
            .map_err(|error| error.to_string())?;
        page_number += 1;
        if read < PAGE_SIZE {
            break;
        }
    }
    target.flush().map_err(|error| error.to_string())?;
    drop(target);
    if output.exists() {
        fs::remove_file(output).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, output).map_err(|error| error.to_string())
}

pub fn ensure_decrypted(path: &Path, edition: &str) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err("数据库文件不存在".into());
    }
    if is_sqlite(path)? {
        return Ok(path.to_path_buf());
    }
    let output_dir = std::env::temp_dir().join("TokenUsage");
    fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let output = output_dir.join(format!("{}-decrypted.db", edition));
    let stamp = source_stamp(path)?;
    let stamp_file = stamp_path(&output);
    if output.exists()
        && stamp_file.exists()
        && fs::read_to_string(&stamp_file).ok().as_deref() == Some(stamp.as_str())
        && is_sqlite(&output).unwrap_or(false)
    {
        seed_key_cache_if_available(path, edition);
        return Ok(output);
    }
    let mut source = BufReader::new(File::open(path).map_err(|error| error.to_string())?);
    let mut first_page = [0_u8; PAGE_SIZE];
    source
        .read_exact(&mut first_page)
        .map_err(|error| error.to_string())?;
    let key = match load_cached_key(edition) {
        Some(cached) if valid_key(&cached, &first_page) => cached,
        _ => scan_memory_for_key(&first_page, edition_process_names(edition))?,
    };
    decrypt_database(path, &output, &key)?;
    save_cached_key(edition, &key);
    fs::write(stamp_file, stamp).map_err(|error| error.to_string())?;
    Ok(output)
}
