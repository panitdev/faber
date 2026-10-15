//! Scratch without a sandbox runtime: everything virtual mode answers on its
//! own, and the refusal everything else gets.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Fixture, MemoryMount, blobs, text};
use environment::{Blob, Edit, Exec, Fault, Outcome, Patch, PatchOp, Replace, Target};
use scratch::{Call, Mode};

fn exec(command: &str) -> Exec {
    Exec::new(command).timeout(Duration::from_secs(20))
}

#[tokio::test]
async fn probes_answer_from_the_image_without_promoting() {
    let fixture = Fixture::new("probes");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();

    let mode = session
        .prepare(Call::Exec {
            command: "cat /etc/os-release",
            compute: false,
        })
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);

    let (exit, mode) = session
        .run(exec("cat /etc/os-release && echo \"$PATH\" && command -v cargo && [ -d /bin ] && echo dir"), false)
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(exit.outcome, Outcome::Completed { code: 0 });
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "PRETTY_NAME=\"Scratch Test\"\nID=test\n\
         /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
         /usr/bin/cargo\ndir\n"
    );
    assert_eq!(session.manifest().home, "/root");
    assert_eq!(session.manifest().root.as_str(), "/");
}

#[tokio::test]
async fn writes_land_in_the_layer_and_never_in_the_base() {
    let fixture = Fixture::new("writes");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();

    let (exit, _) = session
        .run(
            exec("mkdir -p /work/new && echo hi > /work/new/f && rm /etc/motd && echo more >> /work/a.txt"),
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

    // The base is untouched.
    let rootfs = fixture.rootfs();
    assert_eq!(
        std::fs::read_to_string(rootfs.join("etc/motd")).unwrap(),
        "base motd\n"
    );
    assert_eq!(
        std::fs::read_to_string(rootfs.join("work/a.txt")).unwrap(),
        "one\ntwo\nthree\ntwo\n"
    );
    assert!(!rootfs.join("work/new").exists());

    // Scratch sees the change.
    let path = session.path("/work/new/f").unwrap();
    let span = session.read(&path, None).await.unwrap();
    assert_eq!(text(&blobs, &span), "hi\n");
    let motd = session.path("/etc/motd").unwrap();
    assert!(matches!(
        session.read(&motd, None).await,
        Err(Fault::Denied(_))
    ));
    let (exit, _) = session.run(exec("cat /work/a.txt"), false).await.unwrap();
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "one\ntwo\nthree\ntwo\nmore\n"
    );

    // Edits and patches go through the same layer.
    let a = session.path("/work/a.txt").unwrap();
    session
        .edit(&Edit::Replace(Replace::new(a.clone(), "three", "THREE")))
        .await
        .unwrap();
    session
        .edit(&Edit::Patch(Patch::new(vec![
            PatchOp::Add {
                path: session.path("/work/added.txt").unwrap(),
                body: b"added\n".to_vec(),
            },
            PatchOp::Move {
                from: session.path("/work/sub/c.rs").unwrap(),
                to: session.path("/work/moved/c.rs").unwrap(),
            },
        ])))
        .await
        .unwrap();
    let (exit, _) = session
        .run(
            exec("cat /work/a.txt /work/added.txt /work/moved/c.rs; ls /work/sub"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "one\ntwo\nTHREE\ntwo\nmore\nadded\nfn main() {}\n"
    );
    assert!(rootfs.join("work/sub/c.rs").exists());
}

#[tokio::test]
async fn what_needs_compute_is_refused_before_it_runs_without_a_runtime() {
    let fixture = Fixture::new("refused");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();

    // `touch` is virtual; `uname` is not, so the whole command is refused and
    // the file is never made.
    let refused = session
        .run(exec("touch /work/made && uname -a"), false)
        .await;
    let Err(Fault::Denied(denial)) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert!(
        denial.to_string().contains("no sandbox runtime"),
        "{denial}"
    );
    let (exit, _) = session.run(exec("ls /work"), false).await.unwrap();
    assert!(!text(&blobs, &exit.stdout.span).contains("made"));

    assert!(
        session
            .prepare(Call::Exec {
                command: "ls",
                compute: true
            })
            .await
            .is_err()
    );
    assert!(session.start(exec("sleep 1")).await.is_err());

    // A doubtful virtual answer is rerun in compute when there is one, and
    // kept when there is not.
    let (exit, mode) = session.run(exec("ls /nope"), false).await.unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_ne!(exit.outcome, Outcome::Completed { code: 0 });
}

#[tokio::test]
async fn mounts_are_served_in_virtual_mode_with_conflicts_logged() {
    let fixture = Fixture::new("mounts");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let notes = MemoryMount::with(&[("/a.md", "alpha\n"), ("/dir/b.md", "beta\n")]);
    let docs = MemoryMount::with(&[("/readme", "read me\n")]);
    assert_eq!(
        session
            .mount("notes", "main", false, notes.clone())
            .unwrap(),
        "/mnt/notes/main"
    );
    session.mount("docs", "ro", true, docs.clone()).unwrap();
    assert_eq!(session.mounts(), ["/mnt/docs/ro", "/mnt/notes/main"]);

    let (exit, _) = session
        .run(
            exec("ls /mnt; ls /mnt/notes/main; cat /mnt/notes/main/a.md /mnt/docs/ro/readme"),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        text(&blobs, &exit.stdout.span),
        "docs\nnotes\na.md\ndir\nalpha\nread me\n"
    );

    // Read, change behind scratch's back, then write: a conflict, not a
    // silent overwrite, and the reason is in /mnt/.errors.
    session
        .run(exec("cat /mnt/notes/main/a.md"), false)
        .await
        .unwrap();
    notes.set("/a.md", "edited elsewhere\n");
    let (exit, _) = session
        .run(exec("echo mine > /mnt/notes/main/a.md"), false)
        .await
        .unwrap();
    assert_ne!(exit.outcome, Outcome::Completed { code: 0 });
    assert_eq!(notes.get("/a.md").unwrap(), "edited elsewhere\n");
    let (exit, _) = session.run(exec("cat /mnt/.errors"), false).await.unwrap();
    let log = text(&blobs, &exit.stdout.span);
    assert!(log.contains("put /mnt/notes/main/a.md: conflict"), "{log}");

    // A read-only mount refuses before the provider is asked.
    let puts = docs.puts.load(std::sync::atomic::Ordering::Relaxed);
    let (exit, _) = session
        .run(exec("echo x > /mnt/docs/ro/readme"), false)
        .await
        .unwrap();
    assert_ne!(exit.outcome, Outcome::Completed { code: 0 });
    assert_eq!(docs.puts.load(std::sync::atomic::Ordering::Relaxed), puts);

    // Writing a new file goes through.
    let path = session.path("/mnt/notes/main/new.md").unwrap();
    session
        .write(&path, &Blob(b"new\n".to_vec()))
        .await
        .unwrap();
    assert_eq!(notes.get("/new.md").unwrap(), "new\n");

    session.unmount("notes", "main").unwrap();
    let (exit, _) = session.run(exec("ls /mnt"), false).await.unwrap();
    assert_eq!(text(&blobs, &exit.stdout.span), "docs\n");
}

#[tokio::test]
async fn the_quota_spans_a_users_sessions() {
    let mut fixture = Fixture::new("quota");
    fixture.config.quota = Some(1000);
    let manager = fixture.manager();
    let blobs = blobs();
    let one = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let two = manager
        .session("s2", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let other = manager
        .session("s3", "bob", Arc::clone(&blobs))
        .await
        .unwrap();

    let path = one.path("/work/big").unwrap();
    one.write(&path, &Blob(vec![b'x'; 700])).await.unwrap();
    let path = two.path("/work/big").unwrap();
    let refused = two.write(&path, &Blob(vec![b'x'; 400])).await;
    assert!(refused.is_err(), "{refused:?}");
    let (exit, _) = two
        .run(exec("printf %0400d 0 > /work/shell"), false)
        .await
        .unwrap();
    assert!(
        text(&blobs, &exit.stderr.span).contains("quota"),
        "{}",
        text(&blobs, &exit.stderr.span)
    );

    // Another user is unaffected.
    let path = other.path("/work/big").unwrap();
    other.write(&path, &Blob(vec![b'x'; 900])).await.unwrap();

    // Past the limit, promotion is refused with the reason, before anything
    // runs.
    let path = one.path("/work/more").unwrap();
    one.write(&path, &Blob(vec![b'x'; 300])).await.unwrap();
    let refused = two.run(exec("uname"), false).await;
    let Err(Fault::Denied(denial)) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert!(denial.to_string().contains("quota exceeded"), "{denial}");

    // Freeing space works virtually, and counts.
    one.run(exec("rm /work/big"), false).await.unwrap();
    assert_eq!(manager.quota().used("ann"), 300);

    // The count survives a restart.
    drop((one, two, other));
    let again = fixture.manager();
    assert_eq!(again.quota().used("ann"), 300);
    assert_eq!(again.quota().used("bob"), 900);

    manager.delete_session("s1").await;
    assert_eq!(manager.quota().used("ann"), 0);
    assert!(!fixture.config.root.join("sessions/s1").exists());
}

#[tokio::test]
async fn session_ids_never_name_a_path() {
    let fixture = Fixture::new("ids");
    let manager = fixture.manager();
    assert!(manager.session("../etc", "ann", blobs()).await.is_err());
    assert!(manager.session("a/b", "ann", blobs()).await.is_err());
    assert!(manager.session("", "ann", blobs()).await.is_err());
}

#[tokio::test]
async fn a_timeout_stops_a_virtual_run() {
    let fixture = Fixture::new("timeout");
    let manager = fixture.manager();
    let session = manager.session("s1", "ann", blobs()).await.unwrap();
    let started = std::time::Instant::now();
    let (exit, mode) = session
        .run(
            Exec::new("sleep 5").timeout(Duration::from_millis(300)),
            false,
        )
        .await
        .unwrap();
    assert_eq!(mode, Mode::Virtual);
    assert_eq!(exit.outcome, Outcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10));
}
