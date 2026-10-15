//! The one program Faber runs inside a scratch sandbox that the image did not
//! bring.
//!
//! The sandbox bind-mounts it read-only from the Faber host, so nothing is
//! installed in the image and nothing in the session can replace it. It must
//! be linked statically: it runs against whatever libc the image has, or none.
//!
//! Two jobs:
//!
//! - `relay MOUNTPOINT` serves `/mnt` under compute. gVisor implements
//!   `/dev/fuse` inside the sandbox and offers no way to hand its FUSE
//!   connection to a server outside it, so this mounts FUSE from the inside
//!   and copies every message, unchanged, between `/dev/fuse` and descriptor
//!   3 — a host `SOCK_SEQPACKET` socket passed in with `--pass-fd`, whose
//!   message boundaries are the FUSE ones. The FUSE server is Faber's,
//!   outside the sandbox.
//! - `stat`, `cat`, `put`, `rm`, `mv`, `ls` move bytes for `read` and `patch`
//!   while scratch is in compute, when the session's files live in the
//!   sandbox's overlay rather than on the host. They do exactly one system
//!   call's worth each, so what they report is the sandbox's own answer and
//!   never an image's coreutils.
//!
//! Failures print `errno N` on stderr and exit 1; usage errors exit 2.

use std::ffi::CString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match refs.as_slice() {
        ["relay", mountpoint] => relay(mountpoint),
        ["stat", path] => stat(path),
        ["cat", path] => cat(path),
        ["put", path] => put(path),
        ["rm", path] => fs::remove_file(path),
        ["mv", from, to] => mv(from, to),
        ["ls", dir] => ls(dir),
        _ => {
            eprintln!("usage: scratch-helper relay|stat|cat|put|rm|mv|ls ARGS");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "errno {} {error}",
                error.raw_os_error().unwrap_or(libc::EIO)
            );
            ExitCode::from(1)
        }
    }
}

fn kind(meta: &fs::Metadata) -> &'static str {
    let kind = meta.file_type();
    if kind.is_dir() {
        "dir"
    } else if kind.is_file() {
        "file"
    } else if kind.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

/// `KIND SIZE MTIME REALPATH`, following symlinks. The real path is how
/// `confine` learns where a link led.
fn stat(path: &str) -> io::Result<()> {
    let meta = fs::metadata(path)?;
    let real = fs::canonicalize(path)?;
    let mut out = io::stdout().lock();
    write!(out, "{} {} {} ", kind(&meta), meta.len(), meta.mtime())?;
    out.write_all(real.as_os_str().as_bytes())?;
    out.write_all(b"\n")
}

fn cat(path: &str) -> io::Result<()> {
    let mut file = fs::File::open(path)?;
    if file.metadata()?.is_dir() {
        return Err(io::Error::from_raw_os_error(libc::EISDIR));
    }
    io::copy(&mut file, &mut io::stdout().lock())?;
    Ok(())
}

/// Writes stdin to `path` whole, creating missing parents, then reports it as
/// `stat` does.
fn put(path: &str) -> io::Result<()> {
    let mut body = Vec::new();
    io::stdin().lock().read_to_end(&mut body)?;
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, &body)?;
    stat(path)
}

fn mv(from: &str, to: &str) -> io::Result<()> {
    if let Some(parent) = Path::new(to).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)?;
    stat(to)
}

/// One record per entry, `KIND\0SIZE\0NAME\0`: names may hold newlines.
fn ls(dir: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        write!(out, "{}\0{}\0", kind(&meta), meta.len())?;
        out.write_all(entry.file_name().as_bytes())?;
        out.write_all(b"\0")?;
    }
    Ok(())
}

/// Room for any FUSE message either way. The server caps writes at 1 MiB
/// (`MAX_WRITE` in `scratch::fuse`); FUSE refuses a read into a buffer
/// smaller than that plus the request headers, so this leaves ample room.
const MESSAGE: usize = (1 << 20) + (64 << 10);

/// The host end of the relay, passed in by the caller.
const HOST_FD: i32 = 3;

fn relay(mountpoint: &str) -> io::Result<()> {
    let device = CString::new("/dev/fuse").expect("no nul");
    // Safety: plain syscalls on descriptors this process owns.
    let fuse = unsafe { libc::open(device.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fuse < 0 {
        return Err(io::Error::last_os_error());
    }
    let target =
        CString::new(mountpoint).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let source = CString::new("faber").expect("no nul");
    let fstype = CString::new("fuse").expect("no nul");
    let options = CString::new(format!(
        "fd={fuse},rootmode=40000,user_id=0,group_id=0,allow_other"
    ))
    .expect("no nul");
    let mounted = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            options.as_ptr().cast(),
        )
    };
    if mounted != 0 {
        return Err(io::Error::last_os_error());
    }
    // The caller waits for this line before it lets anything use the mount.
    {
        let mut out = io::stdout().lock();
        out.write_all(b"ready\n")?;
        out.flush()?;
    }

    let to_host = std::thread::spawn(move || pump(fuse, HOST_FD));
    let from_host = pump(HOST_FD, fuse);
    // Either side closing ends the relay; the mount goes with the sandbox.
    let _ = to_host.join();
    from_host
}

/// Copies one message per read from `from` to `to` until either side closes.
/// Both descriptors keep message boundaries, so a read is always one whole
/// message and a write delivers it whole.
fn pump(from: i32, to: i32) -> io::Result<()> {
    let mut buffer = vec![0u8; MESSAGE];
    loop {
        let read = unsafe { libc::read(from, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                // FUSE: the request was interrupted before it was read.
                Some(libc::ENOENT) if from != HOST_FD => continue,
                _ => {
                    eprintln!("relay: read failed: {error}");
                    return Err(error);
                }
            }
        }
        if read == 0 {
            return Ok(());
        }
        let written = unsafe { libc::write(to, buffer.as_ptr().cast(), read as usize) };
        if written < 0 {
            let error = io::Error::last_os_error();
            // FUSE refuses a reply to a request the kernel already gave up
            // on; that ends the request, not the relay.
            if error.raw_os_error() == Some(libc::ENOENT) && to != HOST_FD {
                continue;
            }
            return Err(error);
        }
    }
}
