//! HideFolder 后端：单个静态二进制，替代原来的 scripts/*.sh。
//!
//! 文本协议（给 WebUI 用，保持与旧版 cli.sh 一致）：
//!   成功行 `OK|<给人看的消息>`，失败行 `ERR|<原因>`，`list` 直接输出规则原文，
//!   `status` 输出 `rules=N enabled=N hidden=N`。
//!
//! 磁盘格式与旧版完全兼容：rules.txt / active.txt / hidefolder.log 都在模块目录下。

mod mnt;

use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const FALLBACK_MODDIR: &str = "/data/adb/modules/hidefolder";

struct Ctx {
    moddir: PathBuf,
}

impl Ctx {
    fn detect() -> Ctx {
        // 二进制就在模块根目录，父目录即 MODDIR
        let moddir = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from(FALLBACK_MODDIR));
        Ctx { moddir }
    }
    fn empty(&self) -> PathBuf {
        self.moddir.join("empty")
    }
    fn rules(&self) -> PathBuf {
        self.moddir.join("rules.txt")
    }
    fn state(&self) -> PathBuf {
        self.moddir.join("active.txt")
    }
    fn logf(&self) -> PathBuf {
        self.moddir.join("hidefolder.log")
    }
    fn log(&self, msg: &str) {
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.logf())
        {
            let _ = writeln!(f, "[{}] {}", now_str(), msg);
        }
    }
    fn ensure_dirs(&self) {
        let _ = fs::create_dir_all(self.empty());
        for p in [self.rules(), self.state()] {
            if !p.exists() {
                let _ = fs::write(&p, "");
            }
        }
    }
}

fn now_str() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

// ---------------- 路径换算（与旧版 common.sh 语义一致） ----------------

/// 去掉末尾多余的 `/`。
fn norm_path(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        "/".to_string()
    } else {
        t.to_string()
    }
}

/// 用户可见路径 → FUSE/sdcardfs 下层真实路径；非外部存储返回 None。
fn lower_path_of(p: &str) -> Option<String> {
    for root in [
        "/storage/emulated/0",
        "/sdcard",
        "/mnt/sdcard",
        "/storage/self/primary",
    ] {
        if p == root {
            return Some("/data/media/0".to_string());
        }
        if let Some(rest) = p.strip_prefix(&format!("{}/", root)) {
            return Some(format!("/data/media/0/{}", rest));
        }
    }
    if let Some(rest) = p.strip_prefix("/storage/emulated/") {
        let (user, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if !user.is_empty() && user.bytes().all(|b| b.is_ascii_digit()) {
            return Some(format!("/data/media/{}{}", user, tail));
        }
        return None;
    }
    if let Some(rest) = p.strip_prefix("/storage/") {
        let (vol, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let vb = vol.as_bytes();
        if vb.len() == 9 && vb[4] == b'-' {
            return Some(format!("/mnt/media_rw/{}{}", vol, tail));
        }
    }
    None
}

fn is_shared_storage(p: &str) -> bool {
    p.starts_with("/storage/")
        || p == "/sdcard"
        || p.starts_with("/sdcard/")
        || p == "/mnt/sdcard"
        || p.starts_with("/mnt/sdcard/")
        || p.starts_with("/data/media/")
}

// ---------------- state ----------------

fn state_add(ctx: &Ctx, line: &str) {
    let cur = fs::read_to_string(ctx.state()).unwrap_or_default();
    if cur.lines().any(|l| l == line) {
        return;
    }
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ctx.state())
    {
        let _ = writeln!(f, "{}", line);
    }
}

fn state_del(ctx: &Ctx, line: &str) {
    let cur = fs::read_to_string(ctx.state()).unwrap_or_default();
    let kept: Vec<&str> = cur.lines().filter(|l| *l != line).collect();
    let data = if kept.is_empty() {
        String::new()
    } else {
        kept.join("\n") + "\n"
    };
    let _ = fs::write(ctx.state(), data);
}

// ---------------- MediaStore ----------------

/// percent 编码，保留 `/` 与 unreserved 字符（给 file:// URI 用）。
fn uri_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'.' | b'/' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn am_path() -> Option<String> {
    let path = env::var_os("PATH")?;
    for d in env::split_paths(&path) {
        let p = d.join("am");
        if let Ok(m) = fs::metadata(&p) {
            use std::os::unix::fs::PermissionsExt;
            if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// 发一条 MEDIA_SCANNER_SCAN_FILE 广播，不等待结果（后台即发即忘）。
fn am_scan_file(am: &str, file: &str) {
    let uri = format!("file://{}", uri_encode(file));
    let _ = Command::new(am)
        .args([
            "broadcast",
            "-a",
            "android.intent.action.MEDIA_SCANNER_SCAN_FILE",
            "-d",
            &uri,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// 隐藏后：逐文件触发扫描。文件此时已被 bind 隐藏、扫描器看不到，
/// MediaStore 只会删除数据库中的对应行，绝不触碰文件本身。
/// 血的教训：禁止用 `content delete`——在 Android 上它会把文件一起删掉！
fn ms_purge(ctx: &Ctx, path: &str, files: &[String]) {
    match am_path() {
        Some(am) => {
            for f in files {
                am_scan_file(&am, f);
            }
            ctx.log(&format!("ms_purge(scan): {}", path));
        }
        None => ctx.log("ms_purge: skipped (no am)"),
    }
}

/// 恢复后：触发重扫把记录加回来（最多 500 个文件）。
fn ms_rescan(ctx: &Ctx, path: &str) {
    let am = match am_path() {
        Some(a) => a,
        None => return,
    };
    if !is_shared_storage(path) || !Path::new(path).is_dir() {
        return;
    }
    let mut files = Vec::new();
    collect_files(Path::new(path), 500, &mut files);
    for f in &files {
        am_scan_file(&am, f);
    }
    ctx.log(&format!("ms_rescan: {}", path));
}

fn collect_files(dir: &Path, limit: usize, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if out.len() >= limit {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if ft.is_dir() {
            collect_files(&p, limit, out);
        } else if ft.is_file() {
            out.push(p.to_string_lossy().into_owned());
        }
    }
}

// ---------------- 隐藏 / 恢复 ----------------

/// 全局隐藏。至少一处挂载成功才算成功，否则回滚并返回 Err（绝不在没藏住的情况下动媒体库）。
fn do_hide_global(ctx: &Ctx, path: &str) -> Result<(), String> {
    let path = norm_path(path);
    let lower = lower_path_of(&path);
    let empty = ctx.empty().to_string_lossy().into_owned();

    // 先收集文件列表（此时文件还可见，供 MediaStore 扫描用）
    let mut files = Vec::new();
    if am_path().is_some() && is_shared_storage(&path) && Path::new(&path).is_dir() {
        collect_files(Path::new(&path), 3000, &mut files);
    }

    let guard = mnt::NsGuard::new().map_err(|e| format!("取当前命名空间失败: {}", e))?;
    let init_fd = mnt::open_ns(1).map_err(|e| format!("打不开 init 命名空间: {}", e))?;
    guard
        .enter(init_fd)
        .map_err(|e| format!("setns(init) 失败: {}", e))?;

    let mut errs: Vec<String> = Vec::new();
    // FUSE 下层真实路径：在 init 的 ns 里挂载，走 FUSE 的应用一次全覆盖
    if let Some(ref lp) = lower {
        if Path::new(lp).exists() && !mnt::is_mounted_here(lp) {
            match mnt::bind_mount(&empty, lp) {
                Ok(()) => ctx.log(&format!("mount(global): {}", lp)),
                Err(e) => {
                    let m = format!("mount(global) FAILED: {}: {}", lp, e);
                    ctx.log(&m);
                    errs.push(m);
                }
            }
        }
    }
    // 用户路径：在 init 的 ns 里也挂一份
    if Path::new(&path).exists() && !mnt::is_mounted_here(&path) {
        match mnt::bind_mount(&empty, &path) {
            Ok(()) => ctx.log(&format!("mount(global): {}", path)),
            Err(e) => {
                let m = format!("mount(global) FAILED: {}: {}", path, e);
                ctx.log(&m);
                errs.push(m);
            }
        }
    }
    // 验证：init ns 里至少一处挂载成功才算成功
    let verified = mnt::is_mounted_here(&path)
        || lower.as_deref().map(mnt::is_mounted_here).unwrap_or(false);
    let _ = guard.back();
    mnt::close(init_fd);

    // 所有 mount namespace 里补挂用户路径（覆盖直接访问、非 FUSE 路径、已运行应用）
    if verified {
        mnt::for_each_mnt_ns(|_pid, fd| {
            let Ok(g) = mnt::NsGuard::new() else {
                return;
            };
            if g.enter(fd).is_err() {
                return;
            }
            if Path::new(&path).exists() && !mnt::is_mounted_here(&path) {
                let _ = mnt::bind_mount(&empty, &path);
            }
            let _ = g.back();
        });
    }

    if !verified {
        ctx.log(&format!(
            "hide(global) ABORT: mount failed, rolling back: {}",
            path
        ));
        rollback_global(&path, lower.as_deref());
        return Err(errs
            .into_iter()
            .next()
            .unwrap_or_else(|| "挂载失败（无详细信息，见日志）".to_string()));
    }

    state_add(ctx, &format!("global|{}", path));
    ctx.log(&format!(
        "hide(global): {} lower={}",
        path,
        lower.as_deref().unwrap_or("none")
    ));
    if is_shared_storage(&path) {
        ms_purge(ctx, &path, &files);
    }
    Ok(())
}

fn rollback_global(path: &str, lower: Option<&str>) {
    if let Ok(g) = mnt::NsGuard::new() {
        if let Ok(fd) = mnt::open_ns(1) {
            if g.enter(fd).is_ok() {
                mnt::lazy_umount(path);
                if let Some(lp) = lower {
                    mnt::lazy_umount(lp);
                }
                let _ = g.back();
            }
            mnt::close(fd);
        }
    }
    mnt::for_each_mnt_ns(|_pid, fd| {
        if let Ok(g) = mnt::NsGuard::new() {
            if g.enter(fd).is_ok() {
                mnt::lazy_umount(path);
                let _ = g.back();
            }
        }
    });
}

fn do_show_global(ctx: &Ctx, path: &str) {
    let path = norm_path(path);
    let lower = lower_path_of(&path);
    mnt::for_each_mnt_ns(|_pid, fd| {
        if let Ok(g) = mnt::NsGuard::new() {
            if g.enter(fd).is_ok() {
                mnt::lazy_umount(&path);
                let _ = g.back();
            }
        }
    });
    if let Ok(g) = mnt::NsGuard::new() {
        if let Ok(fd) = mnt::open_ns(1) {
            if g.enter(fd).is_ok() {
                if let Some(ref lp) = lower {
                    mnt::lazy_umount(lp);
                }
                mnt::lazy_umount(&path);
                let _ = g.back();
            }
            mnt::close(fd);
        }
    }
    state_del(ctx, &format!("global|{}", path));
    ctx.log(&format!("show(global): {}", path));
    ms_rescan(ctx, &path);
}

/// 包名 → 进程 pid 列表（读 /proc/<pid>/cmdline 首字段精确匹配，comm 截断名兜底）。
fn pids_of(pkg: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let s = e.file_name().to_string_lossy().into_owned();
        let Ok(pid): Result<u32, _> = s.parse() else {
            continue;
        };
        let cmd = fs::read(format!("/proc/{}/cmdline", pid)).unwrap_or_default();
        let first = cmd.split(|b| *b == 0).next().unwrap_or(&[]);
        if first.is_empty() {
            continue; // 内核线程
        }
        if first == pkg.as_bytes() {
            out.push(pid);
            continue;
        }
        // comm 最多 15 字符，包名前缀匹配兜底
        if let Ok(comm) = fs::read_to_string(format!("/proc/{}/comm", pid)) {
            let comm = comm.trim();
            if !comm.is_empty() && pkg.starts_with(comm) {
                out.push(pid);
            }
        }
    }
    out
}

enum AppErr {
    NotRunning,
    Failed(String),
}

/// 按应用隐藏：进入该应用每个进程的 mount namespace 单独 bind。
/// 注意：只屏蔽文件直接访问；走系统 MediaStore 读图的应用仍可能看到（媒体库全局共享）。
fn do_hide_app(ctx: &Ctx, pkg: &str, path: &str) -> Result<(), AppErr> {
    let path = norm_path(path);
    let empty = ctx.empty().to_string_lossy().into_owned();
    let pids = pids_of(pkg);
    if pids.is_empty() {
        return Err(AppErr::NotRunning);
    }
    let mut ok = false;
    let mut last_err = String::new();
    for pid in pids {
        let fd = match mnt::open_ns(pid) {
            Ok(f) => f,
            Err(e) => {
                last_err = format!("pid {}: {}", pid, e);
                continue;
            }
        };
        let g = match mnt::NsGuard::new() {
            Ok(g) => g,
            Err(e) => {
                last_err = e.to_string();
                mnt::close(fd);
                continue;
            }
        };
        if g.enter(fd).is_ok() {
            if mnt::is_mounted_here(&path) {
                ok = true;
            } else if Path::new(&path).exists() {
                match mnt::bind_mount(&empty, &path) {
                    Ok(()) => {
                        ok = true;
                        ctx.log(&format!("hide(app={}): {}", pkg, path));
                    }
                    Err(e) => {
                        last_err = format!("pid {}: {}", pid, e);
                        ctx.log(&format!(
                            "hide(app={}) FAILED pid={}: {}: {}",
                            pkg, pid, path, e
                        ));
                    }
                }
            }
            let _ = g.back();
        } else {
            last_err = format!("pid {} setns 失败", pid);
        }
        mnt::close(fd);
    }
    if ok {
        state_add(ctx, &format!("app|{}|{}", pkg, path));
        ctx.log(&format!("hide(app={}): {}", pkg, path));
        Ok(())
    } else if pids_of(pkg).is_empty() {
        Err(AppErr::NotRunning)
    } else {
        Err(AppErr::Failed(if last_err.is_empty() {
            "挂载失败（见日志）".to_string()
        } else {
            last_err
        }))
    }
}

fn do_show_app(ctx: &Ctx, pkg: &str, path: &str) {
    let path = norm_path(path);
    for pid in pids_of(pkg) {
        let Ok(fd) = mnt::open_ns(pid) else {
            continue;
        };
        if let Ok(g) = mnt::NsGuard::new() {
            if g.enter(fd).is_ok() {
                mnt::lazy_umount(&path);
                let _ = g.back();
            }
        }
        mnt::close(fd);
    }
    state_del(ctx, &format!("app|{}|{}", pkg, path));
    ctx.log(&format!("show(app={}): {}", pkg, path));
}

// ---------------- 规则管理 ----------------

fn clean_arg(s: &str) -> String {
    s.replace(&['\n', '\t', '\r'][..], "")
}

fn valid_scope(s: &str) -> bool {
    if s == "global" {
        return true;
    }
    // 包名：至少两段，每段非空，只含字母数字下划线
    let mut segs = 0;
    for seg in s.split('.') {
        if seg.is_empty()
            || !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return false;
        }
        segs += 1;
    }
    segs >= 2
}

fn valid_path(s: &str) -> bool {
    s.starts_with('/') && !s.contains('|')
}

fn rand_u16() -> u16 {
    let mut b = [0u8; 2];
    if let Ok(v) = fs::read("/dev/urandom") {
        if v.len() >= 2 {
            b.copy_from_slice(&v[..2]);
        }
    }
    u16::from_ne_bytes(b)
}

/// 生成唯一 id：秒级时间戳 + pid + urandom，避免连续调用碰撞。
fn gen_id(rules_text: &str) -> String {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let pid = unsafe { libc::getpid() };
    let mut id = format!("{}{}{:x}", t, pid, rand_u16());
    while rules_text
        .lines()
        .any(|l| l.starts_with(&format!("{}|", id)))
    {
        id = format!("{}_{:x}", id, rand_u16());
    }
    id
}

/// 尝试立即应用一条规则，如实报告结果（绝不谎报成功）。
fn apply_one(ctx: &Ctx, scope: &str, path: &str, verb: &str) {
    if !Path::new(path).exists() {
        println!("OK|已{}，路径当前不存在，出现后自动隐藏", verb);
        return;
    }
    if scope == "global" {
        match do_hide_global(ctx, path) {
            Ok(()) => println!("OK|已{}并隐藏", verb),
            Err(e) => println!("OK|已{}，但本次未能立即隐藏（{}），重启手机后开机自动隐藏", verb, e),
        }
    } else {
        match do_hide_app(ctx, scope, path) {
            Ok(()) => println!("OK|已{}并隐藏", verb),
            Err(AppErr::NotRunning) => println!("OK|已{}，目标应用启动时自动隐藏", verb),
            Err(AppErr::Failed(e)) => {
                println!("OK|已{}，但挂载失败（{}），应用下次启动时重试", verb, e)
            }
        }
    }
}

fn split_rule(line: &str) -> Option<(String, String, String, String)> {
    let mut parts = line.split('|');
    let id = parts.next()?.to_string();
    let enabled = parts.next()?.to_string();
    let scope = parts.next()?.to_string();
    let path = parts.collect::<Vec<_>>().join("|");
    if path.is_empty() {
        return None;
    }
    Some((id, enabled, scope, path))
}

fn cmd_add(ctx: &Ctx, scope: &str, path: &str) {
    let scope = clean_arg(scope);
    let mut path = clean_arg(path);
    if !valid_scope(&scope) {
        println!("ERR|scope 非法");
        std::process::exit(1);
    }
    if !valid_path(&path) {
        println!("ERR|path 非法：需为绝对路径且不含 |");
        std::process::exit(1);
    }
    path = norm_path(&path);
    if path == "/" {
        println!("ERR|不能隐藏根目录");
        std::process::exit(1);
    }
    let rules_text = fs::read_to_string(ctx.rules()).unwrap_or_default();
    // 重复规则精确匹配（scope + path 完全一致才算重复）
    for line in rules_text.lines() {
        if let Some((_, _, sc, p)) = split_rule(line) {
            if sc == scope && p == path {
                println!("ERR|规则已存在");
                std::process::exit(1);
            }
        }
    }
    let id = gen_id(&rules_text);
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ctx.rules())
            .expect("写 rules.txt 失败");
        let _ = writeln!(f, "{}|1|{}|{}", id, scope, path);
    }
    ctx.log(&format!("rule add: {} {}", scope, path));
    apply_one(ctx, &scope, &path, "添加");
}

fn cmd_del(ctx: &Ctx, id: &str) {
    let id = clean_arg(id);
    let text = fs::read_to_string(ctx.rules()).unwrap_or_default();
    let mut found: Option<(String, String)> = None;
    let mut kept: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with(&format!("{}|", id)) {
            if let Some((_, _, scope, path)) = split_rule(line) {
                found = Some((scope, path));
            }
        } else {
            kept.push(line.to_string());
        }
    }
    let (scope, path) = match found {
        Some(v) => v,
        None => {
            println!("ERR|规则不存在");
            std::process::exit(1);
        }
    };
    if scope == "global" {
        do_show_global(ctx, &path);
    } else {
        do_show_app(ctx, &scope, &path);
    }
    let data = if kept.is_empty() {
        String::new()
    } else {
        kept.join("\n") + "\n"
    };
    let _ = fs::write(ctx.rules(), data);
    ctx.log(&format!("rule del: {} {}", scope, path));
    println!("OK|已删除并恢复");
}

fn cmd_set_enabled(ctx: &Ctx, id: &str, want: bool) {
    let id = clean_arg(id);
    let text = fs::read_to_string(ctx.rules()).unwrap_or_default();
    let mut found: Option<(String, String)> = None;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with(&format!("{}|", id)) {
            if let Some((rid, _, scope, path)) = split_rule(line) {
                found = Some((scope.clone(), path.clone()));
                out.push(format!(
                    "{}|{}|{}|{}",
                    rid,
                    if want { "1" } else { "0" },
                    scope,
                    path
                ));
                continue;
            }
        }
        out.push(line.to_string());
    }
    let (scope, path) = match found {
        Some(v) => v,
        None => {
            println!("ERR|规则不存在");
            std::process::exit(1);
        }
    };
    let data = if out.is_empty() {
        String::new()
    } else {
        out.join("\n") + "\n"
    };
    let _ = fs::write(ctx.rules(), data);
    if want {
        ctx.log(&format!("rule enable: {} {}", scope, path));
        apply_one(ctx, &scope, &path, "启用");
    } else {
        if scope == "global" {
            do_show_global(ctx, &path);
        } else {
            do_show_app(ctx, &scope, &path);
        }
        ctx.log(&format!("rule disable: {} {}", scope, path));
        println!("OK|已禁用并恢复");
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    All,
    GlobalOnly,
    AppOnly,
}

/// 按 rules.txt 应用规则（幂等）。返回 true 表示有规则挂载失败。
fn apply_rules(ctx: &Ctx, mode: Mode) -> bool {
    let mut fail = false;
    let text = fs::read_to_string(ctx.rules()).unwrap_or_default();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((_, enabled, scope, path)) = split_rule(line) else {
            continue;
        };
        if enabled != "1" {
            continue;
        }
        match mode {
            Mode::GlobalOnly if scope != "global" => continue,
            Mode::AppOnly if scope == "global" => continue,
            _ => {}
        }
        if !Path::new(&path).exists() {
            ctx.log(&format!("skip not exist: {}", path));
            continue;
        }
        if scope == "global" {
            if do_hide_global(ctx, &path).is_err() {
                fail = true;
            }
        } else {
            // 应用未运行不算失败，后台 daemon 会补上
            if matches!(do_hide_app(ctx, &scope, &path), Err(AppErr::Failed(_))) {
                fail = true;
            }
        }
    }
    if fail {
        ctx.log("apply done with failures");
    }
    fail
}

fn cmd_apply(ctx: &Ctx, mode: Mode) {
    ctx.log(&format!("apply start: {:?}", mode));
    if apply_rules(ctx, mode) {
        println!("OK|已应用，部分规则挂载失败（见日志），重启手机后开机自动重试");
    } else {
        println!("OK|已应用全部启用中的规则");
    }
}

fn cmd_restore(ctx: &Ctx) {
    let text = fs::read_to_string(ctx.state()).unwrap_or_default();
    for line in text.lines() {
        let mut parts = line.split('|');
        match parts.next() {
            Some("global") => {
                if let Some(p) = parts.next() {
                    do_show_global(ctx, p);
                }
            }
            Some("app") => {
                let (a, b) = (parts.next(), parts.next());
                if let (Some(a), Some(b)) = (a, b) {
                    do_show_app(ctx, a, b);
                }
            }
            _ => {}
        }
    }
    let _ = fs::write(ctx.state(), "");
    ctx.log("restore all done");
    println!("OK|已恢复全部");
}

fn cmd_status(ctx: &Ctx) {
    let text = fs::read_to_string(ctx.rules()).unwrap_or_default();
    let mut rules = 0;
    let mut enabled = 0;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        rules += 1;
        if t.split('|').nth(1) == Some("1") {
            enabled += 1;
        }
    }
    let hidden = fs::read_to_string(ctx.state())
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    println!("rules={} enabled={} hidden={}", rules, enabled, hidden);
}

// ---------------- doctor：本机自检 ----------------

/// 在私有 mount ns 里做一次 bind 挂载测试，不影响外部任何挂载。
fn doctor_bind_test(ctx: &Ctx) -> String {
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
        return format!("FAIL(unshare: {})", std::io::Error::last_os_error());
    }
    let base = ctx.moddir.join(".doctor");
    let _ = fs::remove_dir_all(&base);
    let src = base.join("src");
    let dst = base.join("dst");
    if fs::create_dir_all(&src).is_err() || fs::create_dir_all(&dst).is_err() {
        return "FAIL(建测试目录失败)".to_string();
    }
    let src_s = src.to_string_lossy().into_owned();
    let dst_s = dst.to_string_lossy().into_owned();
    let r = (|| -> std::io::Result<bool> {
        mnt::bind_mount(&src_s, &dst_s)?;
        let ok = mnt::is_mounted_here(&dst_s);
        mnt::lazy_umount(&dst_s);
        Ok(ok)
    })();
    let _ = fs::remove_dir_all(&base);
    match r {
        Ok(true) => "OK".to_string(),
        Ok(false) => "FAIL(挂载后 mountinfo 查不到)".to_string(),
        Err(e) => format!("FAIL({})", e),
    }
}

fn cmd_doctor(ctx: &Ctx) {
    let euid = unsafe { libc::geteuid() };
    let uid = unsafe { libc::getuid() };
    println!(
        "[1/7] uid={} euid={} {}",
        uid,
        euid,
        if euid == 0 {
            "OK(root)"
        } else {
            "FAIL(非 root，挂载不可能成功)"
        }
    );
    match mnt::open_ns(1) {
        Ok(fd) => {
            println!("[2/7] 打开 init mount ns: OK");
            match mnt::NsGuard::new() {
                Ok(g) => match g.enter(fd) {
                    Ok(()) => {
                        let _ = g.back();
                        println!("[3/7] setns(init) 并返回: OK");
                    }
                    Err(e) => println!("[3/7] setns(init): FAIL({})", e),
                },
                Err(e) => println!("[3/7] 保存当前 ns: FAIL({})", e),
            }
            mnt::close(fd);
        }
        Err(e) => {
            println!("[2/7] 打开 init mount ns: FAIL({})", e);
            println!("[3/7] setns(init): SKIP");
        }
    }
    println!("[4/7] 私有 ns 内 bind 测试: {}", doctor_bind_test(ctx));
    match am_path() {
        Some(a) => println!("[5/7] am: 找到 {}（相册联动可用）", a),
        None => println!("[5/7] am: 未找到（相册联动不可用，不影响隐藏本身）"),
    }
    let mut n = 0;
    mnt::for_each_mnt_ns(|_, _| n += 1);
    println!("[6/7] mount namespace 数量: {}", n);
    let text = fs::read_to_string(ctx.rules()).unwrap_or_default();
    let mut rules = 0;
    let mut enabled = 0;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        rules += 1;
        if t.split('|').nth(1) == Some("1") {
            enabled += 1;
        }
    }
    let hidden = fs::read_to_string(ctx.state())
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    println!(
        "[7/7] 规则 {} 条（启用 {}），隐藏中 {}",
        rules, enabled, hidden
    );
}

/// 后台守护：开机 20 秒后全量应用一次，之后每 120 秒为新启动的应用补按应用规则。
fn cmd_daemon(ctx: &Ctx) {
    ctx.log("daemon start");
    std::thread::sleep(Duration::from_secs(20));
    apply_rules(ctx, Mode::All);
    loop {
        std::thread::sleep(Duration::from_secs(120));
        apply_rules(ctx, Mode::AppOnly);
    }
}

// ---------------- 入口 ----------------

fn usage() -> ! {
    eprintln!(
        "hidefolder {} — 用 bind mount 隐藏文件夹\n\
         用法: hidefolder <命令> [参数]\n\
         \x20 list | status | doctor | version\n\
         \x20 add <scope> <path> | del <id> | toggle <id> | enable <id> | disable <id>\n\
         \x20 apply [global-only|app-only|all] | restore | daemon | boot-early",
        VERSION
    );
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = env::args_os()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let ctx = Ctx::detect();
    ctx.ensure_dirs();

    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
    match cmd {
        "list" => print!("{}", fs::read_to_string(ctx.rules()).unwrap_or_default()),
        "status" => cmd_status(&ctx),
        "doctor" => cmd_doctor(&ctx),
        "version" => println!("hidefolder {}", VERSION),
        "add" => {
            let (Some(s), Some(p)) = (args.get(2), args.get(3)) else {
                usage()
            };
            cmd_add(&ctx, s, p)
        }
        "del" => {
            let Some(id) = args.get(2) else { usage() };
            cmd_del(&ctx, id)
        }
        "toggle" => {
            let Some(id) = args.get(2) else { usage() };
            // 先读当前状态再翻转
            let text = fs::read_to_string(ctx.rules()).unwrap_or_default();
            let mut want = true;
            let mut known = false;
            for line in text.lines() {
                if line.starts_with(&format!("{}|", clean_arg(id))) {
                    known = true;
                    want = line.split('|').nth(1) != Some("1");
                    break;
                }
            }
            if !known {
                println!("ERR|规则不存在");
                std::process::exit(1);
            }
            cmd_set_enabled(&ctx, id, want)
        }
        "enable" => {
            let Some(id) = args.get(2) else { usage() };
            cmd_set_enabled(&ctx, id, true)
        }
        "disable" => {
            let Some(id) = args.get(2) else { usage() };
            cmd_set_enabled(&ctx, id, false)
        }
        "apply" => {
            let mode = match args.get(2).map(|s| s.as_str()) {
                Some("global-only") => Mode::GlobalOnly,
                Some("app-only") => Mode::AppOnly,
                Some("all") | None => Mode::All,
                _ => usage(),
            };
            cmd_apply(&ctx, mode)
        }
        "boot-early" => {
            ctx.log("boot-early: apply global rules");
            apply_rules(&ctx, Mode::GlobalOnly);
        }
        "restore" => cmd_restore(&ctx),
        "daemon" => cmd_daemon(&ctx),
        _ => usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_strips_slashes() {
        assert_eq!(norm_path("/sdcard/DCIM/"), "/sdcard/DCIM");
        assert_eq!(norm_path("/sdcard/DCIM///"), "/sdcard/DCIM");
        assert_eq!(norm_path("/"), "/");
        assert_eq!(norm_path(""), "/");
    }

    #[test]
    fn lower_mapping() {
        assert_eq!(lower_path_of("/sdcard"), Some("/data/media/0".into()));
        assert_eq!(
            lower_path_of("/storage/emulated/0/DCIM/a"),
            Some("/data/media/0/DCIM/a".into())
        );
        assert_eq!(
            lower_path_of("/sdcard/DCIM"),
            Some("/data/media/0/DCIM".into())
        );
        assert_eq!(
            lower_path_of("/storage/emulated/10/Music"),
            Some("/data/media/10/Music".into())
        );
        assert_eq!(
            lower_path_of("/storage/ABCD-1234/Movies"),
            Some("/mnt/media_rw/ABCD-1234/Movies".into())
        );
        assert_eq!(lower_path_of("/storage/ABCD-1234"), Some("/mnt/media_rw/ABCD-1234".into()));
        assert_eq!(lower_path_of("/data/data/com.x"), None);
        assert_eq!(lower_path_of("/storage/emulated/x/DCIM"), None);
    }

    #[test]
    fn encode_uri() {
        assert_eq!(uri_encode("/sdcard/a b/中"), "/sdcard/a%20b/%E4%B8%AD");
        assert_eq!(uri_encode("a-b_c.d/e~f"), "a-b_c.d/e~f");
    }

    #[test]
    fn scope_path_validation() {
        assert!(valid_scope("global"));
        assert!(valid_scope("com.tencent.mm"));
        assert!(!valid_scope("com..mm"));
        assert!(!valid_scope(".com.mm"));
        assert!(!valid_scope("noperiod"));
        assert!(!valid_scope("a/b"));
        assert!(valid_path("/sdcard/x"));
        assert!(!valid_path("sdcard/x"));
        assert!(!valid_path("/a|b"));
    }

    #[test]
    fn rule_split() {
        let (id, en, sc, p) = split_rule("123|1|global|/sdcard/DCIM").unwrap();
        assert_eq!((id.as_str(), en.as_str(), sc.as_str(), p.as_str()), ("123", "1", "global", "/sdcard/DCIM"));
        assert!(split_rule("badline").is_none());
        assert!(split_rule("1|1|global|").is_none());
    }

    #[test]
    fn shared_storage_check() {
        assert!(is_shared_storage("/storage/emulated/0/DCIM"));
        assert!(is_shared_storage("/sdcard/x"));
        assert!(is_shared_storage("/data/media/0/x"));
        assert!(!is_shared_storage("/data/data/x"));
    }
}
