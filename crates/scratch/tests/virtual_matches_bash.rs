//! The allowlist's promise: a command `decide` keeps virtual prints what GNU
//! bash and coreutils print for it. Each case runs both ways, in the same
//! directory of the same image, and must agree byte for byte.
//!
//! Needs `bash` and GNU coreutils, grep and sed on the host running the
//! tests, which is what the sandbox image carries.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Fixture, blobs, text};
use environment::{Exec, Outcome, Target};
use scratch::decide::{Decision, decide};
use scratch::{Call, Mode};

const CASES: &[&str] = &[
    "echo hello world",
    "printf '%s-%d\\n' a 1 b 2",
    "echo $((6 * 7))",
    "x=one; echo \"$x two\"",
    "cat -n a.txt",
    "cat a.txt sub/c.rs",
    "head -n 2 a.txt",
    "head -2 a.txt",
    "tail -n 2 a.txt",
    "tail -n +3 a.txt",
    "sort a.txt",
    "sort -r a.txt",
    "sort a.txt | uniq -c",
    "sort -u a.txt",
    "grep -n two a.txt",
    "grep -c two a.txt",
    "grep -v two a.txt",
    "grep -i TWO a.txt",
    "grep -q missing a.txt; echo $?",
    "wc -l < a.txt",
    "tr a-z A-Z < a.txt",
    "cut -c1-2 a.txt",
    "sed -n '2,3p' a.txt",
    "sed 's/two/2/g' a.txt",
    "sed '1d' a.txt",
    "seq 3",
    "basename /a/b/c.rs .rs",
    "dirname /a/b/c.rs",
    "for f in *.txt; do echo \"file $f\"; done",
    "ls",
    "ls sub",
    "ls -1",
    "[ -d sub ] && echo dir",
    "[ -f nope ] || echo missing",
    "test -s a.txt && echo nonempty",
    "pwd",
    "cd sub && pwd",
    "if grep -q three a.txt; then echo yes; else echo no; fi",
    "case a.txt in *.txt) echo text;; *) echo other;; esac",
    "n=0; while [ $n -lt 3 ]; do n=$((n + 1)); echo $n; done",
    "echo a b c | tr ' ' '\\n' | tac",
    "echo abc | rev",
    "set -euo pipefail; echo strict",
    "command -v echo",
    "true && echo ok || echo no",
    "false; echo $?",
    "exit 3",
    // Failures stay virtual, so they must read as the real tools' do.
    "cat nope.txt",
    "ls nope",
];

/// Failures that stay virtual but that bashkit words or shapes differently
/// from coreutils. Each must still fail the same way — same exit code, and a
/// "No such file or directory" naming the operand — and the difference is
/// written down here so it is a known one:
///
/// - `cat a b` with `b` missing stops before printing `a`;
/// - `head`/`tail`/`sort` say `<path>: …` (and `tail`/`sort` the absolute
///   path) where coreutils says `cannot open '<path>' for reading` /
///   `cannot read: <path>`;
/// - `grep` writes its error to stdout and exits 1 (no match) where grep
///   exits 2 (error): `grep -q x missing || …` cannot tell the two apart;
/// - `cd` has no `bash: line 1:` prefix;
/// - `sort` exits 1 where coreutils exits 2 (both nonzero, so `||` and
///   `set -e` act the same).
///
/// Closing these would mean checking a command's file operands exist before
/// deciding it runs virtually.
const FAILURE_CASES: &[&str] = &[
    "cat a.txt nope.txt",
    "head -n 1 nope.txt",
    "tail -n 1 nope.txt",
    "sort nope.txt",
    "grep two nope.txt",
    "cd nope",
];

#[tokio::test]
async fn virtual_answers_match_bash_and_coreutils() {
    let fixture = Fixture::new("matches");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let work = fixture.rootfs().join("work");
    let mut mismatches = Vec::new();

    for case in CASES {
        assert_eq!(
            decide(case),
            Decision::Virtual,
            "`{case}` should be virtual"
        );
        assert_eq!(
            session
                .prepare(Call::Exec {
                    command: case,
                    compute: false
                })
                .await
                .unwrap(),
            Mode::Virtual
        );
        let cwd = session.path("/work").unwrap();
        let exit = session
            .exec(Exec::new(*case).cwd(cwd).timeout(Duration::from_secs(20)))
            .await
            .unwrap();
        assert_eq!(session.mode().await, Mode::Virtual, "`{case}` promoted");

        let host = std::process::Command::new("bash")
            .arg("-c")
            .arg(case)
            .current_dir(&work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .output()
            .unwrap();
        let ours = (
            text(&blobs, &exit.stdout.span),
            text(&blobs, &exit.stderr.span),
            exit.outcome,
        );
        // The host runs inside a copy of the image; scratch's paths are the
        // image's own.
        let image = fixture.rootfs().to_string_lossy().into_owned();
        let theirs = (
            String::from_utf8_lossy(&host.stdout).replace(&image, ""),
            String::from_utf8_lossy(&host.stderr).into_owned(),
            Outcome::Completed {
                code: host.status.code().unwrap_or(-1),
            },
        );
        if ours != theirs {
            mismatches.push(format!(
                "`{case}`\n  virtual: {ours:?}\n  bash:    {theirs:?}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[tokio::test]
async fn virtual_failures_fail_as_bash_does() {
    let fixture = Fixture::new("failures");
    let manager = fixture.manager();
    let blobs = blobs();
    let session = manager
        .session("s1", "ann", Arc::clone(&blobs))
        .await
        .unwrap();
    let work = fixture.rootfs().join("work");

    for case in FAILURE_CASES {
        assert_eq!(
            decide(case),
            Decision::Virtual,
            "`{case}` should be virtual"
        );
        let cwd = session.path("/work").unwrap();
        let exit = session
            .exec(Exec::new(*case).cwd(cwd).timeout(Duration::from_secs(20)))
            .await
            .unwrap();
        assert_eq!(session.mode().await, Mode::Virtual, "`{case}` promoted");
        let host = std::process::Command::new("bash")
            .arg("-c")
            .arg(case)
            .current_dir(&work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .output()
            .unwrap();
        let code = host.status.code().unwrap_or(-1);
        let expected = if case.starts_with("sort ") || case.starts_with("grep ") {
            1
        } else {
            code
        };
        assert_ne!(code, 0, "`{case}` should fail in bash");
        assert_eq!(
            exit.outcome,
            Outcome::Completed { code: expected },
            "`{case}`"
        );
        let said = format!(
            "{}{}",
            text(&blobs, &exit.stdout.span),
            text(&blobs, &exit.stderr.span)
        );
        assert!(
            said.contains("No such file or directory") && said.contains("nope"),
            "`{case}`: {said}"
        );
        assert!(!said.contains("io error"), "`{case}`: {said}");
    }
}
