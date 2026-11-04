//! The one program of Faber's own that runs inside a scratch container.
//!
//! The scratch image carries it at `/usr/local/libexec/faber/scratch-helper`
//! (`ops/scratch-image`); tenantd binds nothing of the caller's into a
//! container. It is linked statically, so it runs against whatever libc the
//! image has, or none.
//!
//! `stat`, `cat`, `put`, `rm`, `mv`, `ls` move bytes for `read` and `patch`
//! while scratch is in compute, when the session's files live in the
//! container's root rather than on the host. They do exactly one system
//! call's worth each, so what they report is the container's own answer and
//! never an image's coreutils.
//!
//! tenantd gives a process no stdin yet, so `put PATH FROM` takes the body
//! from the file `FROM`, which the caller staged in the session's private
//! `/tmp` directory, and removes it.
//!
//! Failures print `errno N` on stderr and exit 1; usage errors exit 2.

use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match refs.as_slice() {
        ["stat", path] => stat(path),
        ["cat", path] => cat(path),
        ["put", path, from] => put(path, from),
        ["rm", path] => fs::remove_file(path),
        ["mv", from, to] => mv(from, to),
        ["ls", dir] => ls(dir),
        _ => {
            eprintln!("usage: scratch-helper stat|cat|put|rm|mv|ls ARGS");
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

/// Writes the body staged at `from` to `path` whole, creating missing
/// parents, then reports it as `stat` does. The staged file is removed
/// whatever happens.
fn put(path: &str, from: &str) -> io::Result<()> {
    let body = fs::read(from);
    let _ = fs::remove_file(from);
    let body = body?;
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
