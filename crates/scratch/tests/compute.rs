//! Compute mode against a real `runsc`.
//!
//! Ignored by default; run with `--include-ignored` and the environment
//! naming the pieces (a run that cannot find them fails rather than passing
//! without testing anything):
//!
//! - `FABER_TEST_RUNSC`: the runsc binary (its `gvisor-bin` sidecars next to
//!   it, as release tarballs ship them);
//! - `FABER_TEST_HELPER`: a statically linked `scratch-helper`, built with
//!   `RUSTFLAGS="-C target-feature=+crt-static" cargo build -p scratch
//!   --bin scratch-helper --target x86_64-unknown-linux-gnu`;
//! - `FABER_TEST_BUSYBOX`: a static busybox, which becomes the image's
//!   userland.
//!
//! runsc runs rootless unless the tests run as root.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{Fixture, MemoryMount, add_busybox, blobs, text};
use environment::{Blob, Cursor, Edit, Exec, Outcome, Replace, Signal, Target};
use scratch::Mode;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.exists())
}

/// A fixture whose image busybox can run in.
fn compute(name: &str) -> Fixture {
    let (Some(runsc), Some(helper), Some(busybox)) = (
        env_path("FABER_TEST_RUNSC"),
        env_path("FABER_TEST_HELPER"),
        env_path("FABER_TEST_BUSYBOX"),
    ) else {
        panic!("set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX");
    };
    let mut fixture = Fixture::new(name);
    add_busybox(&fixture.rootfs(), &busybox);
    fixture.config.runsc = Some(runsc);
    fixture.config.helper = Some(helper);
    fixture.config.shell = "/bin/sh".to_owned();
    // Safety: geteuid has no preconditions.
    fixture.config.rootless = unsafe { libc::geteuid() } != 0;
    fixture
}

fn exec(command: &str) -> Exec {
    Exec::new(command).timeout(Duration::from_secs(60))
}

#[tokio::test]
#[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
async fn a_deletion_survives_every_crossing_between_modes() {
    let fixture = compute("crossing");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();

    // Virtual: write one file, delete one from the base.
    let (exit, mode) = session
        .run(exec("echo virt > /work/v.txt && rm /etc/motd"), false)
        .await
        .unwrap();
    assert_eq!(
        (mode, exit.outcome),
        (Mode::Virtual, Outcome::Completed { code: 0 })
    );

    // Compute sees both.
    let (exit, mode) = session
        .run(
            exec("uname -s; cat /work/v.txt; ls /etc; cat /etc/motd"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Compute);
    assert_eq!(text(&blobs, &exit.stdout.span), "Linux\nvirt\nos-release\n");
    assert!(text(&blobs, &exit.stderr.span).contains("/etc/motd"));

    // Compute changes things, including deleting from the base.
    let (exit, _) = session
        .run(
            exec(
                "echo comp > /work/c.txt && rm /work/a.txt && mkdir /work/d && echo in > /work/d/f",
            ),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        exit.outcome,
        Outcome::Completed { code: 0 },
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    // `read` and `patch` reach the sandbox's files.
    let c = session.path("/work/c.txt").unwrap();
    assert_eq!(
        text(&blobs, &session.read(&c, None).await.unwrap()),
        "comp\n"
    );
    session
        .edit(&Edit::Replace(Replace::new(c.clone(), "comp", "patched")))
        .await
        .unwrap();
    let listing = session
        .list(&session.path("/work").unwrap(), None)
        .await
        .unwrap();
    let names: Vec<String> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect();
    assert!(
        names.contains(&"/work/d".to_owned()) && !names.contains(&"/work/a.txt".to_owned()),
        "{names:?}"
    );

    // Back to virtual over the archived layer.
    session.teardown().await.unwrap();
    assert_eq!(session.mode().await, Mode::Virtual);
    let (exit, mode) = session
        .run(
            exec("cat /work/c.txt /work/v.txt /work/d/f; ls /work; [ -e /etc/motd ] || echo gone"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "patched\nvirt\nin\nc.txt\nd\nsub\nv.txt\ngone\n"
    );

    // And into compute again: still deleted, still there.
    let (exit, mode) = session
        .run(exec("ls /work; ls /etc; cat /work/d/f"), true)
        .await
        .unwrap();
    assert_eq!(mode, Mode::Compute);
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "c.txt\nd\nsub\nv.txt\nos-release\nin\n"
    );

    // The base was never written.
    let rootfs = fixture.rootfs();
    assert!(rootfs.join("etc/motd").exists());
    assert!(rootfs.join("work/a.txt").exists());
    assert!(!rootfs.join("work/c.txt").exists());
    manager.delete_session("s1").await;
}

#[tokio::test]
#[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
async fn processes_run_in_compute_and_idle_teardown_stops_them() {
    let fixture = compute("processes");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();

    let echo = session.start(exec("cat")).await.unwrap();
    assert_eq!(session.mode().await, Mode::Compute);
    session
        .stdin(echo, &Blob(b"hello\n".to_vec()))
        .await
        .unwrap();
    session.close_stdin(echo).await.unwrap();
    let mut chunk = session.output(echo, Cursor::default()).await.unwrap();
    for _ in 0..100 {
        if chunk.outcome.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        chunk = session.output(echo, Cursor::default()).await.unwrap();
    }
    assert_eq!(chunk.outcome, Some(Outcome::Completed { code: 0 }));
    assert_eq!(text(&blobs, &chunk.stdout), "hello\n");

    // A signal reaches the process inside the sandbox.
    let sleeper = session.start(exec("sleep 1000")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    session.signal(sleeper, Signal::Term).await.unwrap();
    let mut outcome = None;
    for _ in 0..100 {
        outcome = session
            .output(sleeper, Cursor::default())
            .await
            .unwrap()
            .outcome;
        if outcome.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        outcome,
        Some(Outcome::Signaled {
            signal: Signal::Term
        })
    );

    // A timeout kills, inside the sandbox, and comes back promptly.
    let started = std::time::Instant::now();
    let exit = session
        .exec(Exec::new("sleep 30").timeout(Duration::from_secs(1)))
        .await
        .unwrap();
    assert_eq!(exit.outcome, Outcome::TimedOut);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );

    // A running process keeps the sandbox up however idle the session is.
    let running = session.start(exec("sleep 1000")).await.unwrap();
    assert!(!session.teardown_idle(Duration::ZERO).await);
    assert_eq!(session.mode().await, Mode::Compute);

    // Stopped anyway, a later call on it says why; finished output survives.
    session.teardown().await.unwrap();
    let refused = session
        .output(running, Cursor::default())
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("stopped by idle teardown"),
        "{refused}"
    );
    let chunk = session.output(echo, Cursor::default()).await.unwrap();
    assert_eq!(text(&blobs, &chunk.stdout), "hello\n");

    // What an `exec` left running does not keep an idle sandbox up: idle
    // teardown stops it, and says so on the next call.
    let (_, mode) = session.run(exec("true"), true).await.unwrap();
    assert_eq!(mode, Mode::Compute);
    let leftover = session.begin(exec("sleep 1000")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(session.teardown_idle(Duration::ZERO).await);
    assert_eq!(session.mode().await, Mode::Virtual);
    let refused = session
        .output(leftover, Cursor::default())
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("stopped by idle teardown"),
        "{refused}"
    );
    manager.delete_session("s1").await;
}

#[tokio::test]
#[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
async fn a_directory_replaced_whole_stays_replaced_across_modes() {
    let fixture = compute("opaque");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let listing = async |command: &str, compute: bool| {
        let (exit, mode) = session.run(exec(command), compute).await.unwrap();
        assert_eq!(exit.outcome, Outcome::Completed { code: 0 }, "`{command}`");
        (mode, text(&blobs, &exit.stdout.span))
    };

    // Replaced virtually: compute sees only the new contents.
    let (mode, _) = listing("rm -rf /work && mkdir /work && echo n > /work/new", false).await;
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(
        listing("ls /work", true).await,
        (Mode::Compute, "new\n".to_owned())
    );

    // Replaced in compute: back in virtual, the base's contents stay gone.
    listing("rm -rf /etc && mkdir /etc && echo c > /etc/only", true).await;
    assert!(session.teardown_idle(Duration::ZERO).await);
    assert_eq!(
        listing("ls /etc", false).await,
        (Mode::Virtual, "only\n".to_owned())
    );
    assert_eq!(
        listing("ls /work", false).await,
        (Mode::Virtual, "new\n".to_owned())
    );
    manager.delete_session("s1").await;
}

#[tokio::test]
#[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
async fn one_fuse_connection_serves_every_mount_including_new_ones() {
    let fixture = compute("fuse");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let notes = MemoryMount::with(&[("/a.md", "alpha\n"), ("/dir/b.md", "beta\n")]);
    session
        .mount("notes", "main", false, notes.clone())
        .unwrap();

    let (exit, mode) = session
        .run(
            exec("cat /mnt/notes/main/a.md /mnt/notes/main/dir/b.md; ls /mnt/notes/main"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Compute);
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "alpha\nbeta\na.md\ndir\n",
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    // Mounted while the sandbox runs.
    let docs = MemoryMount::with(&[("/readme", "read me\n")]);
    session.mount("docs", "ro", true, docs.clone()).unwrap();
    let (exit, _) = session
        .run(exec("cat /mnt/docs/ro/readme; ls /mnt"), false)
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "read me\ndocs\nnotes\n");

    // Writes are buffered and put on close.
    let (exit, _) = session
        .run(
            exec("echo one > /mnt/notes/main/new.md && echo two >> /mnt/notes/main/new.md"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        exit.outcome,
        Outcome::Completed { code: 0 },
        "{}",
        text(&blobs, &exit.stderr.span)
    );
    assert_eq!(notes.get("/new.md").unwrap(), "one\ntwo\n");

    // A conflict is the close's errno, and its reason is in /mnt/.errors:
    // the file changes between the open and the close.
    let writer = session
        .start(exec(
            "exec 3> /mnt/notes/main/a.md; sleep 2; echo mine >&3; exec 3>&-",
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    notes.set("/a.md", "edited elsewhere\n");
    let mut chunk = session.output(writer, Cursor::default()).await.unwrap();
    for _ in 0..100 {
        if chunk.outcome.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        chunk = session.output(writer, Cursor::default()).await.unwrap();
    }
    // Shells ignore what `close` returns, so the log is where this shows.
    assert!(chunk.outcome.is_some());
    assert_eq!(notes.get("/a.md").unwrap(), "edited elsewhere\n");
    let (exit, _) = session.run(exec("cat /mnt/.errors"), false).await.unwrap();
    assert!(
        text(&blobs, &exit.stdout.span).contains("put /mnt/notes/main/a.md: conflict"),
        "{}",
        text(&blobs, &exit.stdout.span)
    );

    // Read-only is refused before the provider sees it.
    let (exit, _) = session
        .run(exec("echo x > /mnt/docs/ro/readme"), false)
        .await
        .unwrap();
    assert_ne!(exit.outcome, Outcome::Completed { code: 0 });
    assert_eq!(docs.get("/readme").unwrap(), "read me\n");

    // `read` in compute goes through the same mount.
    let path = session.path("/mnt/notes/main/dir/b.md").unwrap();
    assert_eq!(
        text(&blobs, &session.read(&path, None).await.unwrap()),
        "beta\n"
    );

    // A file grown past what scratch holds in memory is refused, and the
    // server is still there to say so.
    let (exit, _) = session
        .run(
            exec("truncate -s 1T /mnt/notes/main/a.md; echo $?; dd if=/dev/zero of=/mnt/notes/main/a.md bs=1 count=1 seek=1099511627776 2>/dev/null; echo $?; cat /mnt/notes/main/a.md"),
            true,
        )
        .await
        .unwrap();
    let said = text(&blobs, &exit.stdout.span);
    let codes: Vec<&str> = said.lines().take(2).collect();
    assert!(codes.iter().all(|code| *code != "0"), "{said}");
    assert_eq!(said.lines().nth(2), Some("edited elsewhere"), "{said}");
    // Created, written and closed: one put, with the contents.
    let (exit, _) = session
        .run(
            exec("echo made > /mnt/notes/main/new.md && cat /mnt/notes/main/new.md"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "made\n");
    assert_eq!(notes.get("/new.md").unwrap(), "made\n");

    session.unmount("docs", "ro").unwrap();
    let (exit, _) = session.run(exec("ls /mnt"), false).await.unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "notes\n");
    manager.delete_session("s1").await;
}

#[tokio::test]
#[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
async fn compute_is_capped_by_the_quota_and_sees_the_toolstore_read_only() {
    let mut fixture = compute("cap");
    let toolstore = fixture.dir.0.join("toolstore");
    std::fs::create_dir_all(&toolstore).unwrap();
    std::fs::write(toolstore.join("tool.txt"), "a tool\n").unwrap();
    fixture.config.toolstore = Some(toolstore);
    fixture.config.quota = Some(256 * 1024);
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();

    let (exit, _) = session
        .run(exec("cat /opt/toolstore/tool.txt"), false)
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "a tool\n");

    let (exit, mode) = session
        .run(
            exec("cat /opt/toolstore/tool.txt; echo x > /opt/toolstore/new"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Compute);
    assert_eq!(text(&blobs, &exit.stdout.span), "a tool\n");
    assert_ne!(exit.outcome, Outcome::Completed { code: 0 });

    let (exit, _) = session
        .run(
            exec("dd if=/dev/zero of=/work/big bs=1024 count=1024"),
            false,
        )
        .await
        .unwrap();
    assert_ne!(exit.outcome, Outcome::Completed { code: 0 });
    assert!(
        text(&blobs, &exit.stderr.span).contains("No space left"),
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    session.teardown().await.unwrap();
    assert!(session.bytes() <= 256 * 1024, "{}", session.bytes());
    manager.delete_session("s1").await;
}

#[tokio::test]
#[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
async fn idle_sandboxes_are_archived_and_a_restart_recovers_running_ones() {
    let mut fixture = compute("idle");
    fixture.config.idle_timeout = Duration::ZERO;
    let blobs = blobs();
    {
        let manager = fixture.manager();
        let session = manager
            .session("s1", "ann", Arc::clone(&blobs))
            .await
            .unwrap();
        let (_, mode) = session
            .run(exec("echo kept > /root/notes"), true)
            .await
            .unwrap();
        assert_eq!(mode, Mode::Compute);
        manager.teardown_idle().await;
        assert_eq!(session.mode().await, Mode::Virtual);
        let (exit, mode) = session.run(exec("cat /root/notes"), false).await.unwrap();
        assert_eq!(
            (mode, text(&blobs, &exit.stdout.span)),
            (Mode::Virtual, "kept\n".to_owned())
        );

        // Running again when the process goes away.
        session
            .run(exec("echo after > /root/later"), true)
            .await
            .unwrap();
    }

    // A new process finds the sandbox still running and archives it.
    let manager = fixture.manager();
    manager.recover().await;
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let (exit, mode) = session
        .run(exec("cat /root/notes /root/later"), false)
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(text(&blobs, &exit.stdout.span), "kept\nafter\n");
    manager.delete_session("s1").await;
}
