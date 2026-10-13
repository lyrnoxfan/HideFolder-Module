//! 底层 mount / mount namespace 操作（只依赖 libc，无第三方 crate）。
//!
//! 约定：所有进入其他 mount namespace 的操作都通过 [`NsGuard`] 进行，
//! `enter(fd)` 切过去，`back()` 切回来，Drop 时兜底切回原 namespace。

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::io::RawFd;

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

pub fn close(fd: RawFd) {
    unsafe {
        libc::close(fd);
    }
}

/// 打开 pid 的 mount namespace，返回 fd。
pub fn open_ns(pid: u32) -> io::Result<RawFd> {
    let fd = unsafe {
        libc::open(
            cstr(&format!("/proc/{}/ns/mnt", pid)).as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

fn self_ns() -> io::Result<RawFd> {
    let fd = unsafe { libc::open(cstr("/proc/self/ns/mnt").as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

pub fn setns(fd: RawFd) -> io::Result<()> {
    if unsafe { libc::setns(fd, libc::CLONE_NEWNS) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// 命名空间守卫：创建时记住当前 ns；enter 切入目标 ns；back/Drop 切回。
pub struct NsGuard {
    orig: RawFd,
}

impl NsGuard {
    pub fn new() -> io::Result<NsGuard> {
        Ok(NsGuard { orig: self_ns()? })
    }
    pub fn enter(&self, fd: RawFd) -> io::Result<()> {
        setns(fd)
    }
    pub fn back(&self) -> io::Result<()> {
        setns(self.orig)
    }
}

impl Drop for NsGuard {
    fn drop(&mut self) {
        let _ = setns(self.orig);
        close(self.orig);
    }
}

/// mount(2) MS_BIND。
pub fn bind_mount(src: &str, dst: &str) -> io::Result<()> {
    let r = unsafe {
        libc::mount(
            cstr(src).as_ptr(),
            cstr(dst).as_ptr(),
            std::ptr::null(),
            libc::MS_BIND as libc::c_ulong,
            std::ptr::null(),
        )
    };
    if r != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// 懒卸载，忽略错误（恢复场景下挂载点可能已经没了）。
pub fn lazy_umount(target: &str) {
    unsafe {
        libc::umount2(cstr(target).as_ptr(), libc::MNT_DETACH);
    }
}

/// 还原 mountinfo 里 `\040` 这类八进制转义。
fn unescape_mnt(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_digit()
            && b[i + 3].is_ascii_digit()
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v as char);
            i += 4;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// 纯函数：mountinfo 文本里有没有把 path 当挂载点。
/// mountinfo 第 5 列（" - " 之前的部分）是挂载点，位置固定。
pub fn mountinfo_has(text: &str, path: &str) -> bool {
    for line in text.lines() {
        let left = match line.find(" - ") {
            Some(i) => &line[..i],
            None => continue,
        };
        if let Some(mp) = left.split(' ').nth(4) {
            if unescape_mnt(mp) == path {
                return true;
            }
        }
    }
    false
}

/// 当前线程所在的 mount namespace 里，path 是否为挂载点。
pub fn is_mounted_here(path: &str) -> bool {
    match fs::read_to_string("/proc/self/mountinfo") {
        Ok(t) => mountinfo_has(&t, path),
        Err(_) => false,
    }
}

/// 遍历所有不重复的 mount namespace，对每个调用 f(pid, ns_fd)。
/// fd 在 f 返回后关闭，f 内部不要长期持有。
pub fn for_each_mnt_ns(mut f: impl FnMut(u32, RawFd)) {
    let mut seen: Vec<(u64, u64)> = Vec::new();
    let Ok(rd) = fs::read_dir("/proc") else {
        return;
    };
    for e in rd.flatten() {
        let s = e.file_name().to_string_lossy().into_owned();
        let Ok(pid): Result<u32, _> = s.parse() else {
            continue;
        };
        let Ok(fd) = open_ns(pid) else {
            continue;
        };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } != 0 {
            close(fd);
            continue;
        }
        let key = (st.st_dev as u64, st.st_ino as u64);
        if seen.contains(&key) {
            close(fd);
            continue;
        }
        seen.push(key);
        f(pid, fd);
        close(fd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_octal() {
        assert_eq!(unescape_mnt("/sdcard/My\\040Docs"), "/sdcard/My Docs");
        assert_eq!(unescape_mnt("/plain/path"), "/plain/path");
        assert_eq!(unescape_mnt("a\\134b"), "a\\b");
    }

    #[test]
    fn mountinfo_match() {
        let text = "36 35 98:0 / / rw,noatime master:1 - ext4 /dev/root rw\n\
                    40 35 0:35 /data/media/0/DCIM /storage/emulated/0/DCIM rw,nosuid,nodev - fuse - rw\n";
        assert!(mountinfo_has(text, "/storage/emulated/0/DCIM"));
        assert!(!mountinfo_has(text, "/storage/emulated/0/Download"));
        // 转义路径也能匹配
        let text2 = "41 35 0:35 /data/media/0/My\\040Docs /storage/emulated/0/My\\040Docs rw - fuse - rw\n";
        assert!(mountinfo_has(text2, "/storage/emulated/0/My Docs"));
    }
}
