//! One test body per promise, run against every mode this environment can bind.
//!
//! The agent is told that only two things differ between targets: what the
//! manifest says, and the posture line. Path shape, failure classes, exec
//! semantics, truncation behavior, and cwd handling are supposed to be
//! identical. Everywhere else in the crate that claim is kept by construction
//! — the behavior lives in [`Machine`] and the transports only supply
//! plumbing. This file is where the claim is *checked*, because "by
//! construction" is an argument and an argument is not a test.
//!
//! Every assertion names the mode it failed for, so a transport that reaches
//! into policy and changes an answer says which transport did it.
//!
//! Modes that need something this machine may not have — a docker daemon, an
//! SSH host — are bound when they are available and skipped when they are
//! not. A skipped mode is reported rather than silently passing: a
//! conformance suite that quietly checks one mode is worse than none, because
//! it reads as coverage.

use std::sync::Arc;
use std::time::Duration;

use crate::docker::{Daemon, DockerTarget, LocalSocket};
use crate::exec::{Cursor, Exec, Outcome, Signal};
use crate::fault::{Denial, Fault};
use crate::file::{Edit, Patch, PatchOp, Replace, Window};
use crate::local::LocalTarget;
use crate::local::tests::scratch;
use crate::machine::Machine;
use crate::path::Root;
use crate::ssh::{HostKey, SshCredential, SshForwarded, SshSession, SshTarget};
use crate::store::{Blobs, MemoryBlobs, Span};
use crate::target::Target;

/// One bound target, plus the store its spans redeem against.
struct Mode {
    name: &'static str,
    target: Machine,
    blobs: Arc<MemoryBlobs>,
}

impl Mode {
    fn text(&self, span: &Span) -> String {
        String::from_utf8(self.blobs.get(&span.blob).unwrap()).unwrap()
    }
}

/// Every mode bindable here. Adding a transport is one push.
///
/// A mode needing something this machine may not have is bound only when it is
/// configured, and says so on the way past when it is not. Reading these two
/// variables is not the ambient resolution the crate refuses — nothing under
/// `src/` reads them, and a test fixture choosing what to test against is not
/// a user's connection being routed somewhere they did not ask for.
async fn modes() -> Vec<Mode> {
    let mut modes = Vec::new();

    let blobs = Arc::new(MemoryBlobs::new());
    let root = Root::new(scratch().to_string_lossy()).unwrap();
    modes.push(Mode {
        name: "local+direct",
        target: LocalTarget::bind("conformance", root, blobs.clone() as Arc<dyn Blobs>)
            .await
            .unwrap(),
        blobs,
    });

    match (
        std::env::var("FABER_TEST_DOCKER"),
        std::env::var("FABER_TEST_CONTAINER"),
    ) {
        (Ok(endpoint), Ok(container)) => {
            let blobs = Arc::new(MemoryBlobs::new());
            let daemon = Arc::new(LocalSocket::new(endpoint).unwrap()) as Arc<dyn Daemon>;
            modes.push(Mode {
                name: "local+docker",
                target: DockerTarget::bind(
                    "conformance",
                    daemon,
                    container,
                    Root::new("/work").unwrap(),
                    blobs.clone() as Arc<dyn Blobs>,
                )
                .await
                .expect("could not bind the configured test container"),
                blobs,
            });
        }
        _ => eprintln!(
            "conformance: docker modes skipped — set FABER_TEST_DOCKER and \
             FABER_TEST_CONTAINER to include them"
        ),
    }

    match (
        std::env::var("FABER_TEST_SSH"),
        std::env::var("FABER_TEST_SSH_KEY"),
        std::env::var("FABER_TEST_SSH_ROOT"),
    ) {
        (Ok(address), Ok(key_path), Ok(root)) => {
            let (user, address) = address
                .split_once('@')
                .expect("FABER_TEST_SSH is user@host:port");
            let credential = SshCredential {
                user: user.to_owned(),
                private_key: std::fs::read_to_string(key_path)
                    .expect("could not read the test key"),
                passphrase: None,
            };

            let blobs = Arc::new(MemoryBlobs::new());
            let (target, fingerprint) = SshTarget::bind(
                "conformance",
                address,
                &credential,
                // First contact against a host under test. A caller storing
                // the fingerprint would pass Verify from here on, which is the
                // shape this returns it for.
                HostKey::AcceptNew,
                Root::new(root).unwrap(),
                blobs.clone() as Arc<dyn Blobs>,
            )
            .await
            .expect("could not bind the configured test host");
            assert!(
                fingerprint.starts_with("SHA256:"),
                "a bind reports the host key it saw, so the caller can store it"
            );

            modes.push(Mode {
                name: "ssh+direct",
                target,
                blobs,
            });

            // The fourth cell: the same container, reached by forwarding to
            // the daemon's socket over the session just opened. Loopback here,
            // but every piece is the real one — direct-streamlocal, then the
            // Engine API over an SSH channel.
            if let (Ok(socket), Ok(container)) = (
                std::env::var("FABER_TEST_SSH_DOCKER"),
                std::env::var("FABER_TEST_CONTAINER"),
            ) {
                let (session, _) = SshSession::connect(address, &credential, HostKey::AcceptNew)
                    .await
                    .expect("could not open a session for the forward");
                let daemon =
                    Arc::new(SshForwarded::new(Arc::new(session), socket)) as Arc<dyn Daemon>;

                let blobs = Arc::new(MemoryBlobs::new());
                modes.push(Mode {
                    name: "ssh+docker",
                    target: DockerTarget::bind(
                        "conformance",
                        daemon,
                        container,
                        Root::new("/work").unwrap(),
                        blobs.clone() as Arc<dyn Blobs>,
                    )
                    .await
                    .expect("could not bind the container through the forward"),
                    blobs,
                });
            }
        }
        _ => eprintln!(
            "conformance: ssh modes skipped — set FABER_TEST_SSH (user@host:port), \
             FABER_TEST_SSH_KEY and FABER_TEST_SSH_ROOT to include them"
        ),
    }

    // What ran is part of the result. A suite that checked one mode and a
    // suite that checked four both print "ok".
    eprintln!(
        "conformance: {} mode(s) bound — {}",
        modes.len(),
        modes
            .iter()
            .map(|mode| mode.name)
            .collect::<Vec<_>>()
            .join(", ")
    );

    modes
}

#[tokio::test]
async fn a_nonzero_exit_is_a_result_in_every_mode() {
    for mode in modes().await {
        let exit = mode
            .target
            .exec(Exec::new("exit 3"))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{}: a nonzero exit must not be a fault, got {error}",
                    mode.name
                )
            });
        assert_eq!(
            exit.outcome,
            Outcome::Completed { code: 3 },
            "{}: a nonzero exit is a result carrying the code",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_timeout_is_an_outcome_in_every_mode() {
    for mode in modes().await {
        let exit = mode
            .target
            .exec(Exec::new("sleep 30").timeout(Duration::from_millis(150)))
            .await
            .unwrap_or_else(|error| {
                panic!("{}: a timeout must not be a fault, got {error}", mode.name)
            });
        assert_eq!(
            exit.outcome,
            Outcome::TimedOut,
            "{}: the command ran, it just did not stop",
            mode.name
        );
    }
}

#[tokio::test]
async fn every_exit_echoes_its_target_and_resolved_cwd_in_every_mode() {
    for mode in modes().await {
        let exit = mode.target.exec(Exec::new("true")).await.unwrap();
        assert_eq!(
            exit.target.as_str(),
            "conformance",
            "{}: the exit names the target it ran on",
            mode.name
        );
        assert_eq!(
            exit.cwd.as_str(),
            mode.target.root().as_str(),
            "{}: the exit carries the configured cwd",
            mode.name
        );
    }
}

#[tokio::test]
async fn cwd_does_not_carry_between_calls_in_any_mode() {
    for mode in modes().await {
        mode.target
            .write(&mode.target.root().join("sub/marker").unwrap(), &"x".into())
            .await
            .unwrap();
        mode.target.exec(Exec::new(&format!("cd {}/sub", mode.target.root()))).await.unwrap();

        let exit = mode.target.exec(Exec::new("pwd")).await.unwrap();
        assert_eq!(
            mode.text(&exit.stdout.span).trim(),
            mode.target.root().as_str(),
            "{}: cwd is per-call and never persistent",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_path_leaving_the_root_is_refused_in_every_mode() {
    for mode in modes().await {
        // Lexical, so it is refused before any transport sees it — which is
        // the point: the refusal cannot differ by mode.
        for escape in ["/../etc/passwd", "~/.ssh/id_rsa", "relative/path"] {
            let denial = mode.target.path(escape).unwrap_err();
            assert!(
                matches!(denial, Fault::Denied(Denial::PathEscape { .. })),
                "{}: `{escape}` leaves the root and is refused, got {denial:?}",
                mode.name
            );
        }
    }
}

#[tokio::test]
async fn a_missing_file_is_not_an_empty_read_in_any_mode() {
    for mode in modes().await {
        let fault = mode
            .target
            .read(&mode.target.root().join("nope.txt").unwrap(), None)
            .await
            .unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::NotFound { .. })),
            "{}: a missing path is not an empty file, got {fault:?}",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_window_past_the_end_is_out_of_range_in_every_mode() {
    for mode in modes().await {
        let path = mode.target.root().join("lines.txt").unwrap();
        mode.target
            .write(&path, &"a\nb\nc\nd\n".into())
            .await
            .unwrap();

        let span = mode
            .target
            .read(&path, Some(Window::new(1, 2)))
            .await
            .unwrap();
        assert_eq!(
            mode.text(&span),
            "b\nc",
            "{}: the window selects lines",
            mode.name
        );
        assert!(
            span.truncated,
            "{}: a window that stopped short of the end is flagged",
            mode.name
        );

        let fault = mode
            .target
            .read(&path, Some(Window::new(99, 2)))
            .await
            .unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::OutOfRange { .. })),
            "{}: past the end is not an empty read, got {fault:?}",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_rejected_glob_is_never_an_empty_listing_in_any_mode() {
    for mode in modes().await {
        let root = mode.target.root();

        let fault = mode.target.list(&root, Some("[abc")).await.unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::BadPattern { .. })),
            "{}: a rejected pattern is a refusal, not a settled negative answer, got {fault:?}",
            mode.name
        );

        // The other half: a real negative answer stays distinguishable.
        let listing = mode.target.list(&root, Some("*.nothing")).await.unwrap();
        assert!(
            listing.entries.is_empty() && !listing.truncated,
            "{}: an empty match set is empty and not truncated",
            mode.name
        );
    }
}

#[tokio::test]
async fn an_ambiguous_edit_is_refused_in_every_mode() {
    for mode in modes().await {
        let path = mode.target.root().join("dup.txt").unwrap();
        mode.target.write(&path, &"x\nx\n".into()).await.unwrap();

        let fault = mode
            .target
            .edit(&Edit::Replace(Replace::new(path.clone(), "x", "y")))
            .await
            .unwrap_err();
        assert!(
            matches!(fault, Fault::Denied(Denial::EditRefused { .. })),
            "{}: two matches without `all` is refused rather than guessed at, got {fault:?}",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_patch_set_adds_edits_moves_and_deletes_in_every_mode() {
    for mode in modes().await {
        let first = mode.target.root().join("patch/a.txt").unwrap();
        let second = mode.target.root().join("patch/b.txt").unwrap();
        let moved = mode.target.root().join("patch/nested/c.txt").unwrap();

        mode.target.write(&second, &"old\n".into()).await.unwrap();
        let patch = Patch::new(vec![
            PatchOp::Add {
                path: first.clone(),
                body: b"one\n".to_vec(),
            },
            PatchOp::Update(Replace::new(second.clone(), "old", "new")),
            PatchOp::Move {
                from: second.clone(),
                to: moved.clone(),
            },
        ]);

        let stats = mode.target.edit(&Edit::Patch(patch)).await.unwrap();
        assert_eq!(
            stats.len(),
            3,
            "{}: every op that wrote reports a stat",
            mode.name
        );
        assert_eq!(
            mode.text(&mode.target.read(&moved, None).await.unwrap()),
            "new\n",
            "{}: the edit landed and then moved with the file",
            mode.name
        );
        assert!(
            matches!(
                mode.target.read(&second, None).await.unwrap_err(),
                Fault::Denied(Denial::NotFound { .. })
            ),
            "{}: what moved is gone from where it was",
            mode.name
        );

        mode.target
            .edit(&Edit::Patch(Patch::new(vec![PatchOp::Delete {
                path: first.clone(),
            }])))
            .await
            .unwrap();
        assert!(
            matches!(
                mode.target.read(&first, None).await.unwrap_err(),
                Fault::Denied(Denial::NotFound { .. })
            ),
            "{}: a deleted file is gone",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_listing_stays_in_one_directory_in_every_mode() {
    for mode in modes().await {
        for name in ["/list/keep.rs", "/list/skip.md", "/list/deep/also.rs"] {
            mode.target
                .write(&mode.target.root().join(name).unwrap(), &"".into())
                .await
                .unwrap();
        }

        let listing = mode
            .target
            .list(&mode.target.root().join("list").unwrap(), Some("*.rs"))
            .await
            .unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            names,
            vec!["/list/keep.rs"],
            "{}: a glob matches one directory's names and does not recurse",
            mode.name
        );
    }
}

#[tokio::test]
async fn a_background_process_reads_forward_from_a_cursor_in_every_mode() {
    for mode in modes().await {
        let id = mode
            .target
            .start(Exec::new("while read line; do echo \"got $line\"; done"))
            .await
            .unwrap();

        mode.target.stdin(id, &"one\n".into()).await.unwrap();

        let mut cursor = Cursor::START;
        let mut waited = 0;
        let first = loop {
            let chunk = mode.target.output(id, cursor).await.unwrap();
            cursor = chunk.next;
            if chunk.stdout.len > 0 {
                break mode.text(&chunk.stdout);
            }
            waited += 1;
            assert!(
                waited < 200,
                "{}: the process never answered stdin",
                mode.name
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(
            first.trim(),
            "got one",
            "{}: stdin reached a running process and its output came back",
            mode.name
        );

        mode.target
            .signal(id, Signal::Term)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{}: a running process can be signalled, got {error}",
                    mode.name
                )
            });
    }
}

#[tokio::test]
async fn large_output_spills_to_a_path_inside_the_target_in_every_mode() {
    for mode in modes().await {
        let exit = mode
            .target
            .exec(Exec::new("head -c 200000 /dev/zero | tr '\\0' 'a'"))
            .await
            .unwrap();

        let spill = exit
            .stdout
            .spill
            .unwrap_or_else(|| panic!("{}: large output spills to a path", mode.name));
        let span = mode.target.read(&spill, None).await.unwrap();
        assert_eq!(
            span.len, 200_000,
            "{}: the spill file holds everything the span could not",
            mode.name
        );
    }
}

#[tokio::test]
async fn every_mode_publishes_what_it_can_do() {
    for mode in modes().await {
        let manifest = mode.target.manifest();
        assert!(
            !manifest.capabilities.is_empty(),
            "{}: a target that answers nothing is not a target",
            mode.name
        );
        assert_eq!(
            manifest.label.as_str(),
            "conformance",
            "{}: the manifest names the label it was bound under",
            mode.name
        );
        assert_eq!(
            manifest.shell,
            crate::probe::SHELL,
            "{}: every mode runs command strings through the same shell",
            mode.name
        );
        // The probe went out over this mode's own transport and came back with
        // something. `unknown` is the honest answer when it did not, and it is
        // the answer a transport that silently returns nothing would give.
        assert_ne!(
            manifest.os, "unknown",
            "{}: the bind probe reached the target",
            mode.name
        );
        assert_ne!(
            manifest.arch, "unknown",
            "{}: the bind probe reached the target",
            mode.name
        );
    }
}
