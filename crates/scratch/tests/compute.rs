//! Scratch against a real tenantd.
//!
//! Ignored by default; run with `--include-ignored` and the environment
//! naming the pieces (a run that cannot find them fails rather than passing
//! without testing anything):
//!
//! - `FABER_TEST_TENANTD`: tenantd's gRPC socket, or instead
//!   `FABER_TEST_TENANTD_ENDPOINT` with `FABER_TEST_TENANTD_TLS_CA`, `_CERT`
//!   and `_KEY` for its mTLS listener. Its storage need not be on this
//!   host;
//! - `FABER_TEST_IMAGE`: the scratch image (`ops/scratch-default`) as a
//!   registry reference the profile admits;
//! - `FABER_TEST_PROFILE`: the profile, `scratch` unless set. It must allow
//!   exec and declare the `workspace` slot at `/workspace`, the `tmp` slot at
//!   `/tmp`, and a FUSE slot at `/mnt`.
//!
//! The process running the tests must be one of tenantd's callers.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{MemoryMount, blobs, text};
use environment::{
    Blob, Cursor, Denial, Edit, EntryKind, Exec, Fault, Outcome, Patch, PatchOp, Signal, Target,
};
use scratch::{ScratchConfig, ScratchManager, TenantdConfig};

const IGNORED: &str = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE";

/// Ids as Faber gives them: UUIDs, distinct per test run.
fn id(n: u8) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "0199{:08x}-0000-7000-8000-{:012x}",
        nanos as u32,
        // Twelve digits, as in a UUID: longer names overflow the socket
        // path gVisor makes for a sandbox.
        ((nanos as u64) << 8 | u64::from(n)) & 0xffff_ffff_ffff
    )
}

/// A manager over the tenantd the environment names.
fn manager() -> ScratchManager {
    common::trace();
    let (Some(endpoint), Some(image)) = (
        scratch::compute::test_endpoint(),
        std::env::var("FABER_TEST_IMAGE").ok(),
    ) else {
        panic!("{IGNORED}");
    };
    let mut tenantd = TenantdConfig::new(endpoint, image);
    if let Ok(profile) = std::env::var("FABER_TEST_PROFILE") {
        tenantd.profile = profile;
    }
    tenantd.storage_bytes = 1 << 30;
    let mut config = ScratchConfig::new(tenantd);
    config.shell = "/bin/sh".to_owned();
    ScratchManager::new(config)
}

fn exec(command: &str) -> Exec {
    Exec::new(command).timeout(Duration::from_secs(60))
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn the_volumes_outlive_the_container_and_nothing_else_does() {
    let manager = manager();
    let blobs = blobs();
    let (user, project, s1, s2) = (id(1), id(2), id(3), id(4));
    let one = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();
    assert!(!one.running().await, "opening a session starts nothing");

    let exit = one
        .exec(exec("echo kept > /workspace/a && echo shared > /tmp/shared && echo mine > \"$TMPDIR/own\" && echo gone > /root/eph; echo $TMPDIR"))
        .await
        .unwrap();
    assert_eq!(
        exit.outcome,
        Outcome::Completed { code: 0 },
        "{}",
        text(&blobs, &exit.stderr.span)
    );
    assert_eq!(text(&blobs, &exit.stdout.span), format!("/tmp/s-{s1}\n"));
    assert!(one.running().await);

    // Another session of the project shares the workspace and /tmp, not
    // $TMPDIR.
    let two = manager
        .session(&s2, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();
    let exit = two
        .exec(exec("cat /workspace/a /tmp/shared; ls \"$TMPDIR\""))
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "kept\nshared\n");

    // Idle drops the container's root and $TMPDIR; the volumes stay.
    one.teardown().await;
    assert!(!one.running().await);
    let exit = one
        .exec(exec(
            "cat /workspace/a; [ -e /root/eph ] && echo eph; ls \"$TMPDIR\"; cat /tmp/shared",
        ))
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "kept\nshared\n");

    // `read` and `write` start the container too, and reach its own root.
    one.teardown().await;
    let path = one.path("/root/written").unwrap();
    one.write(&path, &Blob(b"in the container\n".to_vec()))
        .await
        .unwrap();
    assert!(one.running().await);
    let span = one.read(&path, None).await.unwrap();
    assert_eq!(text(&blobs, &span), "in the container\n");

    manager.delete_session(&s1).await;
    manager.delete_session(&s2).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn files_in_compute_are_the_containers_own() {
    let manager = manager();
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    let session = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();

    // What a process leaves in the container's root, outside both volumes,
    // is what `read` finds, through a symlink and with a space in the name.
    let exit = session
        .exec(exec(
            "mkdir -p /root/d/sub && printf hi > '/root/d/a b' && ln -s '/root/d/a b' /root/link",
        ))
        .await
        .unwrap();
    assert_eq!(
        exit.outcome,
        Outcome::Completed { code: 0 },
        "{}",
        text(&blobs, &exit.stderr.span)
    );
    let link = session.path("/root/link").unwrap();
    let span = session.read(&link, None).await.unwrap();
    assert_eq!(text(&blobs, &span), "hi");

    // A write makes the parents it needs, and a process sees the result.
    let deep = session.path("/root/new/deep/file").unwrap();
    let stat = session.write(&deep, &Blob(b"body".to_vec())).await.unwrap();
    assert_eq!(stat.size, 4);
    assert!(stat.modified.is_some());
    let exit = session.exec(exec("cat /root/new/deep/file")).await.unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "body");

    // So does a move, and the file is gone from where it was.
    let moved = session.path("/root/moved/here/file").unwrap();
    let stats = session
        .edit(&Edit::Patch(Patch::new(vec![PatchOp::Move {
            from: deep.clone(),
            to: moved.clone(),
        }])))
        .await
        .unwrap();
    assert_eq!(stats.iter().map(|stat| stat.size).collect::<Vec<_>>(), [4]);
    let span = session.read(&moved, None).await.unwrap();
    assert_eq!(text(&blobs, &span), "body");
    assert!(matches!(
        session.read(&deep, None).await,
        Err(Fault::Denied(Denial::NotFound { .. }))
    ));

    // A listing names each entry with its kind, the link included.
    let root = session.path("/root").unwrap();
    let listing = session.list(&root, None).await.unwrap();
    let kind = |path: &str| {
        listing
            .entries
            .iter()
            .find(|entry| entry.path.as_str() == path)
            .map(|entry| entry.kind)
    };
    assert_eq!(kind("/root/d"), Some(EntryKind::Dir));
    assert_eq!(kind("/root/link"), Some(EntryKind::Symlink));

    // A directory has no bytes to read: a failure, never empty contents.
    let dir = session.path("/root/d").unwrap();
    assert!(session.read(&dir, None).await.is_err());

    manager.delete_session(&s1).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn processes_run_in_compute_and_idle_teardown_stops_them() {
    let manager = manager();
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    let session = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();

    // stdin given with the call reaches the process.
    session.exec(exec("true")).await.unwrap();
    let exit = session
        .exec(exec("cat").stdin(b"hello\n".to_vec()))
        .await
        .unwrap();
    assert_eq!(exit.outcome, Outcome::Completed { code: 0 });
    assert_eq!(text(&blobs, &exit.stdout.span), "hello\n");

    // A signal reaches the process inside the container.
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

    // A timeout kills, inside the container, and comes back promptly.
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

    // A running process keeps the container up however idle the session is.
    let running = session.start(exec("sleep 1000")).await.unwrap();
    assert!(!session.teardown_idle(Duration::ZERO).await);
    assert!(session.running().await);

    // Stopped anyway, a later call on it says why.
    session.teardown().await;
    let refused = session
        .output(running, Cursor::default())
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("stopped by idle teardown"),
        "{refused}"
    );

    // What an `exec` left running does not keep an idle container up.
    session.exec(exec("true")).await.unwrap();
    let leftover = session.begin(exec("sleep 1000")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(session.teardown_idle(Duration::ZERO).await);
    assert!(!session.running().await);
    let refused = session
        .output(leftover, Cursor::default())
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("stopped by idle teardown"),
        "{refused}"
    );
    manager.delete_session(&s1).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn the_fuse_connection_tenantd_hands_over_serves_every_mount() {
    let manager = manager();
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    let session = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();
    let notes = MemoryMount::with(&[("/a.md", "alpha\n"), ("/dir/b.md", "beta\n")]);
    session
        .mount("notes", "main", false, notes.clone())
        .unwrap();

    let exit = session
        .exec(exec(
            "cat /mnt/notes/main/a.md /mnt/notes/main/dir/b.md; ls /mnt/notes/main",
        ))
        .await
        .unwrap();
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "alpha\nbeta\na.md\ndir\n",
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    // Mounted while the container runs: no remount.
    let docs = MemoryMount::with(&[("/readme", "read me\n")]);
    session.mount("docs", "ro", true, docs.clone()).unwrap();
    let exit = session
        .exec(exec("cat /mnt/docs/ro/readme; ls /mnt"))
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "read me\ndocs\nnotes\n");

    let exit = session
        .exec(exec(
            "echo one > /mnt/notes/main/new.md && echo two >> /mnt/notes/main/new.md",
        ))
        .await
        .unwrap();
    assert_eq!(
        exit.outcome,
        Outcome::Completed { code: 0 },
        "{}",
        text(&blobs, &exit.stderr.span)
    );
    assert_eq!(notes.get("/new.md").unwrap(), "one\ntwo\n");
    manager.delete_session(&s1).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn a_restart_deletes_what_the_last_process_left_running() {
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    {
        let manager = manager();
        let session = manager
            .session(&s1, &user, &project, Arc::clone(&blobs))
            .await
            .unwrap();
        session
            .exec(exec("echo kept > /workspace/k"))
            .await
            .unwrap();
        // The process ends here with its container still running.
    }
    let manager = manager();
    manager.recover().await;
    let session = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();
    // A fresh container, over the same workspace.
    let exit = session.exec(exec("cat /workspace/k")).await.unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "kept\n");
    manager.delete_session(&s1).await;
}

/// Runs `call` as an agent run does: on a thread of its own with a
/// current-thread runtime, dropped when the call returns, with every task
/// spawned on it.
fn in_a_run<F, Fut>(call: F) -> Fut::Output
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future,
    Fut::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(call())
            })
            .join()
            .unwrap()
    })
}

/// The incident behind faber-aa3: the run whose call started the container
/// ended, and with its runtime went the task serving `/mnt`. The next run's
/// first `/mnt` access then waited forever, freezing the sandbox. A
/// background process started in one run must likewise keep its output
/// collected after that run.
#[test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
fn what_a_run_starts_outlives_the_run() {
    // The API's runtime, which outlives every run.
    let api = tokio::runtime::Runtime::new().unwrap();
    let manager = {
        let _on = api.enter();
        manager()
    };
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    let session = api
        .block_on(manager.session(&s1, &user, &project, Arc::clone(&blobs)))
        .unwrap();
    let notes = MemoryMount::with(&[("/a.md", "alpha\n")]);
    session
        .mount("notes", "main", false, notes.clone())
        .unwrap();

    // The first run starts the container, and a process that speaks only
    // once the run is over.
    let late = in_a_run(|| async {
        let exit = session.exec(exec("true")).await.unwrap();
        assert_eq!(exit.outcome, Outcome::Completed { code: 0 });
        session.start(exec("sleep 3; echo late")).await.unwrap()
    });

    // The next run reaches /mnt, and hears the process.
    in_a_run(|| async {
        let exit = tokio::time::timeout(
            Duration::from_secs(60),
            session.exec(exec("ls -la / > /dev/null && cat /mnt/notes/main/a.md")),
        )
        .await
        .expect("/mnt hung after the run that started the container ended")
        .unwrap();
        assert_eq!(
            text(&blobs, &exit.stdout.span),
            "alpha\n",
            "{}",
            text(&blobs, &exit.stderr.span)
        );

        let mut heard = String::new();
        for _ in 0..100 {
            let chunk = session.output(late, Cursor::default()).await.unwrap();
            heard = text(&blobs, &chunk.stdout);
            if chunk.outcome.is_some() && !heard.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(heard, "late\n");
    });

    api.block_on(manager.delete_session(&s1));
}
