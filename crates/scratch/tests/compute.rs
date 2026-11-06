//! Compute mode against a real tenantd.
//!
//! Ignored by default; run with `--include-ignored` and the environment
//! naming the pieces (a run that cannot find them fails rather than passing
//! without testing anything):
//!
//! - `FABER_TEST_TENANTD`: tenantd's gRPC socket, or instead
//!   `FABER_TEST_TENANTD_ENDPOINT` with `FABER_TEST_TENANTD_TLS_CA`, `_CERT`
//!   and `_KEY` for its mTLS listener. tenantd's volumes must be visible at
//!   the paths it reports either way, as for a deployment;
//! - `FABER_TEST_IMAGE`: the scratch image (`ops/scratch-image`, which
//!   carries `scratch-helper`) as a registry reference the profile admits;
//! - `FABER_TEST_PROFILE`: the profile, `scratch` unless set. It must allow
//!   exec and declare the `workspace` slot at `/workspace`, the `tmp` slot at
//!   `/tmp`, and a FUSE slot at `/mnt`.
//!
//! The process running the tests must be one of tenantd's callers.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Fixture, MemoryMount, blobs, text};
use environment::{Cursor, Exec, Outcome, Signal, Target};
use scratch::{Mode, ScratchManager, TenantdConfig};

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
        (nanos as u64) << 8 | u64::from(n)
    )
}

/// A fixture whose compute is tenantd.
fn compute(name: &str) -> Fixture {
    let (Some(endpoint), Some(image)) = (
        scratch::compute::test_endpoint(),
        std::env::var("FABER_TEST_IMAGE").ok(),
    ) else {
        panic!("{IGNORED}");
    };
    let mut fixture = Fixture::new(name);
    let mut tenantd = TenantdConfig::new(endpoint, image);
    if let Ok(profile) = std::env::var("FABER_TEST_PROFILE") {
        tenantd.profile = profile;
    }
    tenantd.storage_bytes = 1 << 30;
    fixture.config.tenantd = Some(tenantd);
    fixture.config.shell = "/bin/sh".to_owned();
    fixture
}

fn exec(command: &str) -> Exec {
    Exec::new(command).timeout(Duration::from_secs(60))
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn the_volumes_cross_between_modes_and_nothing_else_does() {
    let fixture = compute("volumes");
    let manager = fixture.manager();
    let blobs = blobs();
    let (user, project, s1, s2) = (id(1), id(2), id(3), id(4));
    let one = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();

    let (exit, mode) = one
        .run(
            exec("echo kept > /workspace/a && echo shared > /tmp/shared && echo mine > \"$TMPDIR/own\" && echo gone > /root/eph"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(
        exit.outcome,
        Outcome::Completed { code: 0 },
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    // Promoted, the session sees the volumes and not the virtual layer.
    let (exit, mode) = one
        .run(
            exec("cat /workspace/a /tmp/shared \"$TMPDIR/own\"; [ -e /root/eph ] && echo eph; echo $TMPDIR"),
            true,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Compute);
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        format!("kept\nshared\nmine\n/tmp/s-{s1}\n"),
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    // Another session of the project shares the workspace and /tmp, not
    // $TMPDIR.
    let two = manager
        .session(&s2, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();
    let (exit, _) = one
        .run(
            exec("echo from-compute > /workspace/b && echo root > /root/c"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(exit.outcome, Outcome::Completed { code: 0 });
    let (exit, mode) = two
        .run(exec("cat /workspace/b /tmp/shared; ls \"$TMPDIR\""), false)
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(text(&blobs, &exit.stdout.span), "from-compute\nshared\n");

    // Idle drops the container's root and $TMPDIR; the volumes stay.
    one.teardown().await;
    assert_eq!(one.mode().await, Mode::Virtual);
    let (exit, _) = one
        .run(
            exec("cat /workspace/b; [ -e /root/c ] && echo root; ls \"$TMPDIR\"; cat /tmp/shared"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "from-compute\nshared\n");

    // `read` and `write` in compute go through the helper.
    one.run(exec("true"), true).await.unwrap();
    let path = one.path("/root/written").unwrap();
    one.write(&path, &environment::Blob(b"by the helper\n".to_vec()))
        .await
        .unwrap();
    let span = one.read(&path, None).await.unwrap();
    assert_eq!(text(&blobs, &span), "by the helper\n");

    manager.delete_session(&s1).await;
    manager.delete_session(&s2).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn processes_run_in_compute_and_idle_teardown_stops_them() {
    let fixture = compute("processes");
    let manager = fixture.manager();
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    let session = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();

    // stdin given with the call reaches the process.
    session.run(exec("true"), true).await.unwrap();
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
    assert_eq!(session.mode().await, Mode::Compute);

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
    manager.delete_session(&s1).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn the_fuse_connection_tenantd_hands_over_serves_every_mount() {
    let fixture = compute("fuse");
    let manager = fixture.manager();
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

    // Mounted while the container runs: no remount.
    let docs = MemoryMount::with(&[("/readme", "read me\n")]);
    session.mount("docs", "ro", true, docs.clone()).unwrap();
    let (exit, _) = session
        .run(exec("cat /mnt/docs/ro/readme; ls /mnt"), false)
        .await
        .unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "read me\ndocs\nnotes\n");

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
    manager.delete_session(&s1).await;
}

#[tokio::test]
#[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
async fn a_restart_deletes_what_the_last_process_left_running() {
    let fixture = compute("recover");
    let blobs = blobs();
    let (user, project, s1) = (id(1), id(2), id(3));
    {
        let manager = fixture.manager();
        let session = manager
            .session(&s1, &user, &project, Arc::clone(&blobs))
            .await
            .unwrap();
        session
            .run(exec("echo kept > /workspace/k"), true)
            .await
            .unwrap();
        // The process ends here with its container still running.
    }
    let manager = ScratchManager::new(fixture.config.clone()).unwrap();
    manager.recover().await;
    let session = manager
        .session(&s1, &user, &project, Arc::clone(&blobs))
        .await
        .unwrap();
    // A fresh container, over the same workspace.
    let (exit, mode) = session.run(exec("cat /workspace/k"), true).await.unwrap();
    assert_eq!(mode, Mode::Compute);
    assert_eq!(text(&blobs, &exit.stdout.span), "kept\n");
    manager.delete_session(&s1).await;
}
