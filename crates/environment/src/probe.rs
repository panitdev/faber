//! What a target is, asked once, through the transport that will run everything else.
//!
//! One implementation for every mode. The alternative — each transport
//! deciding for itself what `os` means — produces manifests that differ in
//! spelling rather than in substance, and the agent reads the difference as
//! information.
//!
//! Probing is the one place the target's own binaries are legitimately in
//! play. Everywhere else, depending on what an image carries is the failure
//! this crate exists to prevent; here, discovering exactly that is the job.
//!
//! Nothing here reads the Faber server's environment. A manifest describes the
//! *target*, and on a shared deployment the server's environment belongs to
//! the operator rather than to any user — so `$SHELL`, `$PATH`, and the host's
//! `std::env::consts` are all off limits even when the target happens to be
//! this same machine.

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::exec::Signal;
use crate::fault::Fault;
use crate::path::{Root, RootedPath};
use crate::spawn::{Run, Spawn};

/// The shell every command string runs through, in every mode.
///
/// Not probed, because it is not a fact about the target: it is Faber's choice
/// of what to invoke, and probing it would mean asking the Faber server what
/// *its* operator's login shell is and then claiming that as the target's.
/// `/bin/sh` is the one answer that is present everywhere, means the same
/// thing everywhere, and does not vary with who deployed the service.
pub const SHELL: &str = "/bin/sh";

/// Long enough for a slow link and a cold container, short enough that binding
/// a dead host fails rather than hangs.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Binaries asked about at bind. Per-binary capability is manifest data, never
/// tool presence — there is no `git` verb on [`Target`](crate::Target), only a
/// `git` line in the manifest.
pub const PROBED_TOOLS: &[&str] = &["git", "rg", "cargo", "node", "python3"];

/// What one probe found.
#[derive(Clone, Debug)]
pub struct Probed {
    pub os: String,
    pub arch: String,
    /// `$HOME` as the target's own login set it, or `None` when it is unset.
    /// Raw: [`home_within`] decides what the agent is told.
    pub home: Option<String>,
    pub tools: BTreeMap<String, String>,
}

/// Asks a target what it is, through the transport that will run everything
/// else against it.
///
/// One round trip: a probe that costs five is five chances for a flaky link to
/// produce a manifest that is partly true, and a partly true manifest is worse
/// than a failed bind.
pub async fn probe(spawn: &dyn Spawn, cwd: String) -> Result<Probed, Fault> {
    // `$HOME` is printed on its own line, after a marker, so an empty value
    // still occupies its line and the tool lines after it stay aligned.
    let mut script = String::from("uname -s\nuname -m\nprintf 'home=%s\\n' \"$HOME\"\n");
    for tool in PROBED_TOOLS {
        // `command -v` is a shell builtin, so presence is decided without
        // depending on `which` being installed.
        script.push_str(&format!(
            "if command -v {tool} >/dev/null 2>&1; then printf '%s\\t' {tool}; \
             {tool} --version 2>/dev/null | head -n 1; fi\n"
        ));
    }

    let mut proc = spawn
        .spawn(Run {
            argv: vec![SHELL.to_owned(), "-c".to_owned(), script],
            cwd,
            env: Vec::new(),
            pty: false,
        })
        .await?;

    // Close stdin: a probe answers no prompts, and leaving it open lets a
    // misbehaving rc file wait on one.
    drop(proc.stdin());
    let mut stdout = proc.stdout();
    let reading = tokio::spawn(async move {
        let mut bytes = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut bytes).await;
        }
        bytes
    });

    match tokio::time::timeout(PROBE_TIMEOUT, proc.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(fault)) => return Err(fault),
        Err(_) => {
            let _ = proc.signal(Signal::Kill).await;
            return Err(Fault::Unreachable(
                "the target did not answer the bind probe".to_owned(),
            ));
        }
    }

    let output = reading.await.unwrap_or_default();
    Ok(parse(&String::from_utf8_lossy(&output)))
}

/// Splits the probe's output into a manifest's worth of facts.
///
/// Missing lines are not an error. A target that answers nothing useful still
/// binds, and the manifest says `unknown` — which is honest, where guessing
/// `linux` would be believed.
fn parse(output: &str) -> Probed {
    let mut lines = output.lines();
    // Lowercased so the same target does not read as two different machines
    // depending on whether `uname` capitalizes.
    let os = lines
        .next()
        .map(|line| line.trim().to_lowercase())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    let arch = lines
        .next()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    let home = lines
        .next()
        .and_then(|line| line.strip_prefix("home="))
        .map(str::trim)
        .filter(|home| !home.is_empty())
        .map(str::to_owned);

    let mut tools = BTreeMap::new();
    for line in lines {
        if let Some((tool, version)) = line.split_once('\t') {
            let version = version.trim();
            if !version.is_empty() {
                tools.insert(tool.to_owned(), version.to_owned());
            }
        }
    }

    Probed {
        os,
        arch,
        home,
        tools,
    }
}

/// The home directory the agent is told about: the target's `$HOME` when it
/// is a path inside the root, and the root itself otherwise.
///
/// Every path the agent sends is refused outside the root, so a home outside
/// it would be a starting point no call can use. It would also say something
/// about the machine the user never registered — for a target on the Faber
/// server itself, the operator's own home directory.
pub fn home_within(root: &Root, home: Option<&str>) -> String {
    home.and_then(|home| RootedPath::new(root, home).ok())
        .map(|path| path.as_str().to_owned())
        .unwrap_or_else(|| root.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_reads_os_arch_and_the_tools_that_answered() {
        let probed =
            parse("Linux\nx86_64\nhome=/home/me\ngit\tgit version 2.43.0\nrg\tripgrep 14.1.0\n");
        assert_eq!(probed.os, "linux");
        assert_eq!(probed.home.as_deref(), Some("/home/me"));
        assert_eq!(probed.arch, "x86_64");
        assert_eq!(probed.tools["git"], "git version 2.43.0");
        assert_eq!(probed.tools["rg"], "ripgrep 14.1.0");
    }

    #[test]
    fn a_target_that_answers_nothing_is_unknown_rather_than_assumed() {
        let probed = parse("");
        assert_eq!(probed.os, "unknown");
        assert_eq!(probed.arch, "unknown");
        assert!(probed.tools.is_empty());
        assert!(probed.home.is_none());
    }

    #[test]
    fn a_home_outside_the_root_is_reported_as_the_root() {
        let root = Root::new("/srv/project").unwrap();
        assert_eq!(home_within(&root, Some("/home/faber")), "/srv/project");
        assert_eq!(home_within(&root, None), "/srv/project");
        assert_eq!(
            home_within(&root, Some("/srv/project/me")),
            "/srv/project/me"
        );
        let whole = Root::new("/").unwrap();
        assert_eq!(home_within(&whole, Some("/home/me")), "/home/me");
        // Not a path at all: nothing to start from but the root.
        assert_eq!(home_within(&whole, Some("~")), "/");
    }

    #[test]
    fn the_probe_asks_for_home_on_its_own_line() {
        // A target that prints no home still keeps the tool lines in place.
        let probed = parse("Linux\nx86_64\nhome=\nrg\tripgrep 14.1.0\n");
        assert_eq!(probed.tools["rg"], "ripgrep 14.1.0");
    }

    #[test]
    fn a_tool_that_printed_no_version_is_absent_rather_than_blank() {
        let probed = parse("Linux\naarch64\nhome=\ngit\t\n");
        assert!(probed.home.is_none());
        assert!(!probed.tools.contains_key("git"));
    }
}
