//! Whether a command runs in the virtual shell or is promoted to the sandbox.
//!
//! Decided before anything runs, from the parsed command alone. The whole
//! command is checked against an allowlist of commands and flags whose
//! virtual behavior was compared against GNU bash and coreutils (see
//! `tests/virtual_matches_bash.rs`); anything off the list promotes the whole
//! command, so nothing of it runs twice.
//!
//! Promoted unconditionally: a command name that is not a literal word, a
//! path being run, `eval`, `source`, a nested shell, a function definition, a
//! background job, process substitution, `coproc`, `select`, `time`, and any
//! file under `/proc`, `/sys` or `/dev` other than `/dev/null` — none of
//! those exist virtually.
//!
//! This is the only decision: a virtual run that fails stays a virtual
//! failure, and is never discarded and run again in the sandbox.

use std::sync::OnceLock;

use bashkit::parser::{
    AssignmentValue, Command, CompoundCommand, ListOperator, Parser, Redirect, RedirectKind,
    SimpleCommand, Word, WordPart,
};

/// Where a command runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Virtual,
    /// Run in the sandbox, for the reason given.
    Promote(String),
}

/// How a command's arguments are checked.
enum Args {
    /// Every word is an operand; the command has no options that matter.
    Any,
    /// GNU-style options: clustered short flags, `--long` flags, and options
    /// that take a value. Anything else that starts with `-` promotes.
    Flags {
        short: &'static str,
        short_value: &'static str,
        long: &'static [&'static str],
        /// At most this many operands.
        max_operands: Option<usize>,
        /// Option values must be non-negative counts (`head -n 3`, `tail -n +2`).
        counts: bool,
    },
    /// Checked by a function of its own.
    Custom(fn(&[Arg]) -> Result<(), String>),
}

/// One argument as far as the decision can see it.
#[derive(Clone, Debug)]
enum Arg {
    /// Fully literal.
    Literal(String),
    /// Starts with literal text that is not an option; the rest expands.
    Operand,
    /// Could expand to anything, an option included.
    Unknown,
}

const fn flags(short: &'static str) -> Args {
    Args::Flags {
        short,
        short_value: "",
        long: &[],
        max_operands: None,
        counts: false,
    }
}

/// The allowlist.
fn rule(name: &str) -> Option<Args> {
    Some(match name {
        "echo" | "true" | "false" | ":" | "exit" => Args::Any,
        "printf" => Args::Custom(printf),
        "test" | "[" => Args::Custom(test),
        "cd" => Args::Flags {
            short: "",
            short_value: "",
            long: &[],
            max_operands: Some(1),
            counts: false,
        },
        "pwd" => Args::Flags {
            short: "",
            short_value: "",
            long: &[],
            max_operands: Some(0),
            counts: false,
        },
        "export" | "unset" | "basename" | "dirname" | "sleep" | "seq" | "nl" | "tac" | "rev"
        | "touch" | "rmdir" | "cp" | "mv" => flags(""),
        "set" => Args::Custom(set),
        "read" => flags("r"),
        "cat" => flags("n"),
        "head" => Args::Flags {
            short: "0123456789",
            short_value: "nc",
            long: &[],
            max_operands: None,
            counts: true,
        },
        "tail" => Args::Flags {
            short: "0123456789",
            short_value: "n",
            long: &[],
            max_operands: None,
            counts: true,
        },
        // Reading stdin only: with file operands bashkit's `wc` prints
        // nothing, where coreutils prints counts and names.
        "wc" => Args::Flags {
            short: "lcwm",
            short_value: "",
            long: &[],
            max_operands: Some(0),
            counts: false,
        },
        // One operand at most: bashkit lists several directories in a
        // different order. `-l` and `-a` are the known exception to matching
        // coreutils; see `listing_output`.
        "ls" => Args::Flags {
            short: "1la",
            short_value: "",
            long: &[],
            max_operands: Some(1),
            counts: false,
        },
        "sort" => Args::Flags {
            short: "rnu",
            short_value: "kt",
            long: &[],
            max_operands: None,
            counts: false,
        },
        "uniq" => flags("cdu"),
        "cut" => Args::Flags {
            short: "",
            short_value: "dfc",
            long: &[],
            max_operands: None,
            counts: false,
        },
        "tr" => flags("ds"),
        "tee" => flags("a"),
        "mkdir" => flags("p"),
        "rm" => Args::Flags {
            short: "rRf",
            short_value: "",
            long: &["--recursive", "--force"],
            max_operands: None,
            counts: false,
        },
        "ln" => Args::Custom(ln),
        // Not `-r`: bashkit prefixes matches with the absolute path.
        "grep" => Args::Flags {
            short: "invclLqsxwEFohH",
            short_value: "e",
            long: &[],
            max_operands: None,
            counts: false,
        },
        "sed" => Args::Custom(sed),
        "command" => Args::Custom(command_v),
        _ => return None,
    })
}

/// Test operators bashkit answers differently from bash in the sandbox:
/// it has no owners, devices, sockets, terminals or link counts, and its
/// `stat` follows symlinks.
const TEST_OPERATORS_PROMOTED: &[&str] = &[
    "-L", "-h", "-O", "-G", "-N", "-S", "-b", "-c", "-p", "-t", "-u", "-g", "-k", "-w", "-ef",
    "-nt", "-ot", "-v", "-R",
];

/// Shell builtins and keywords of bash itself. `command -v` names these
/// alone; for anything else it prints a path, which bashkit only does for
/// names it does not implement itself.
const BASH_BUILTINS: &[&str] = &[
    "alias",
    "bg",
    "bind",
    "break",
    "builtin",
    "caller",
    "cd",
    "command",
    "compgen",
    "complete",
    "compopt",
    "continue",
    "declare",
    "dirs",
    "disown",
    "echo",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "getopts",
    "hash",
    "help",
    "history",
    "jobs",
    "kill",
    "let",
    "local",
    "logout",
    "mapfile",
    "popd",
    "printf",
    "pushd",
    "pwd",
    "read",
    "readarray",
    "readonly",
    "return",
    "set",
    "shift",
    "shopt",
    "source",
    "suspend",
    "test",
    "times",
    "trap",
    "true",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unset",
    "wait",
    "[",
    ".",
    ":",
    "if",
    "then",
    "else",
    "elif",
    "fi",
    "case",
    "esac",
    "for",
    "select",
    "while",
    "until",
    "do",
    "done",
    "in",
    "function",
    "time",
    "{",
    "}",
    "!",
    "[[",
    "]]",
    "coproc",
];

fn bashkit_builtins() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| bashkit::Bash::new().builtin_names())
}

/// Decides where `command` runs.
pub fn decide(command: &str) -> Decision {
    if command
        .chars()
        .any(|ch| matches!(ch, '\0' | '\x01' | '\x02' | '\u{1e}' | '\u{1f}'))
    {
        return Decision::Promote("contains control bytes the virtual shell reserves".into());
    }
    let script = match Parser::new(command).parse() {
        Ok(script) => script,
        Err(_) => return Decision::Promote("the virtual shell cannot parse it".into()),
    };
    match commands(&script.commands).and_then(|()| listings_output(&script.commands, false)) {
        Ok(()) => Decision::Virtual,
        Err(reason) => Decision::Promote(reason),
    }
}

fn commands(list: &[Command]) -> Result<(), String> {
    list.iter().try_for_each(command)
}

fn listings_output(list: &[Command], consumed: bool) -> Result<(), String> {
    list.iter()
        .try_for_each(|command| listing_output(command, consumed))
}

/// `ls -l` and `ls -a` stay virtual although bashkit prints them differently:
/// `-l` has no `total` line, link count or owner and writes dates its own way,
/// and `-a` leaves out `.` and `..`. Someone reading a listing loses nothing
/// that matters, but a program reading one gets the wrong columns, so these
/// promote unless their output is only read or cut short by `head` or `tail`.
/// `consumed` says that something else takes this command's output.
fn listing_output(command: &Command, consumed: bool) -> Result<(), String> {
    match command {
        Command::Simple(simple) => {
            if differing_listing(simple)
                && (consumed || simple.redirects.iter().any(captures_stdout))
            {
                return Err("`ls -l` and `ls -a` print differently virtually, \
                     and something here reads their output"
                    .into());
            }
            Ok(())
        }
        Command::Pipeline(pipeline) => {
            for (at, stage) in pipeline.commands.iter().enumerate() {
                let kept = pipeline.commands[at + 1..].iter().all(keeps_lines);
                listing_output(stage, consumed || !kept)?;
            }
            Ok(())
        }
        Command::List(list) => {
            listing_output(&list.first, consumed)?;
            list.rest
                .iter()
                .try_for_each(|(_, next)| listing_output(next, consumed))
        }
        Command::Compound(compound, redirects) => {
            compound_listings_output(compound, consumed || redirects.iter().any(captures_stdout))
        }
        Command::Function(_) => Ok(()),
    }
}

fn compound_listings_output(compound: &CompoundCommand, consumed: bool) -> Result<(), String> {
    match compound {
        CompoundCommand::If(cmd) => {
            listings_output(&cmd.condition, consumed)?;
            listings_output(&cmd.then_branch, consumed)?;
            for (condition, body) in &cmd.elif_branches {
                listings_output(condition, consumed)?;
                listings_output(body, consumed)?;
            }
            match &cmd.else_branch {
                Some(body) => listings_output(body, consumed),
                None => Ok(()),
            }
        }
        CompoundCommand::For(cmd) => listings_output(&cmd.body, consumed),
        CompoundCommand::ArithmeticFor(cmd) => listings_output(&cmd.body, consumed),
        CompoundCommand::While(cmd) => {
            listings_output(&cmd.condition, consumed)?;
            listings_output(&cmd.body, consumed)
        }
        CompoundCommand::Until(cmd) => {
            listings_output(&cmd.condition, consumed)?;
            listings_output(&cmd.body, consumed)
        }
        CompoundCommand::Case(cmd) => cmd
            .cases
            .iter()
            .try_for_each(|item| listings_output(&item.commands, consumed)),
        CompoundCommand::Subshell(body) | CompoundCommand::BraceGroup(body) => {
            listings_output(body, consumed)
        }
        _ => Ok(()),
    }
}

/// Whether this is an `ls` asking for `-l` or `-a`.
fn differing_listing(simple: &SimpleCommand) -> bool {
    literal(&simple.name).as_deref() == Some("ls")
        && simple
            .args
            .iter()
            .filter_map(literal)
            .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains(['l', 'a']))
}

/// Whether a pipeline stage passes its input's lines on unchanged, only
/// fewer of them, to where the pipeline's output goes.
fn keeps_lines(command: &Command) -> bool {
    let Command::Simple(simple) = command else {
        return false;
    };
    matches!(literal(&simple.name).as_deref(), Some("head" | "tail"))
        && !simple.redirects.iter().any(captures_stdout)
}

/// Whether a redirect sends standard output somewhere other than the result.
fn captures_stdout(redirect: &Redirect) -> bool {
    match redirect.kind {
        RedirectKind::OutputBoth => true,
        RedirectKind::Output
        | RedirectKind::Clobber
        | RedirectKind::Append
        | RedirectKind::DupOutput => matches!(redirect.fd, None | Some(1)),
        _ => false,
    }
}

fn command(command: &Command) -> Result<(), String> {
    match command {
        Command::Simple(simple) => simple_command(simple),
        Command::Pipeline(pipeline) => commands(&pipeline.commands),
        Command::List(list) => {
            self::command(&list.first)?;
            for (operator, next) in &list.rest {
                if *operator == ListOperator::Background {
                    return Err("it runs a command in the background".into());
                }
                self::command(next)?;
            }
            Ok(())
        }
        Command::Compound(compound, redirects) => {
            compound_command(compound)?;
            redirects.iter().try_for_each(redirect)
        }
        Command::Function(_) => Err("it defines a function".into()),
    }
}

fn compound_command(compound: &CompoundCommand) -> Result<(), String> {
    match compound {
        CompoundCommand::If(cmd) => {
            commands(&cmd.condition)?;
            commands(&cmd.then_branch)?;
            for (condition, body) in &cmd.elif_branches {
                commands(condition)?;
                commands(body)?;
            }
            if let Some(body) = &cmd.else_branch {
                commands(body)?;
            }
            Ok(())
        }
        CompoundCommand::For(cmd) => {
            if let Some(words) = &cmd.words {
                words.iter().try_for_each(word)?;
            }
            commands(&cmd.body)
        }
        CompoundCommand::ArithmeticFor(cmd) => {
            expression(&cmd.init)?;
            expression(&cmd.condition)?;
            expression(&cmd.step)?;
            commands(&cmd.body)
        }
        CompoundCommand::While(cmd) => {
            commands(&cmd.condition)?;
            commands(&cmd.body)
        }
        CompoundCommand::Until(cmd) => {
            commands(&cmd.condition)?;
            commands(&cmd.body)
        }
        CompoundCommand::Case(cmd) => {
            word(&cmd.word)?;
            for item in &cmd.cases {
                item.patterns.iter().try_for_each(word)?;
                commands(&item.commands)?;
            }
            Ok(())
        }
        CompoundCommand::Subshell(body) | CompoundCommand::BraceGroup(body) => commands(body),
        CompoundCommand::Arithmetic(expr) => expression(expr),
        CompoundCommand::Conditional(words) => {
            words.iter().try_for_each(word)?;
            for w in words {
                if let Some(text) = literal(w)
                    && TEST_OPERATORS_PROMOTED.contains(&text.as_str())
                {
                    return Err(format!("`[[ {text} ]]` is answered differently virtually"));
                }
            }
            Ok(())
        }
        CompoundCommand::Select(_) => Err("it uses `select`".into()),
        CompoundCommand::Time(_) => Err("it uses `time`".into()),
        CompoundCommand::Coproc(_) => Err("it uses `coproc`".into()),
    }
}

fn simple_command(simple: &SimpleCommand) -> Result<(), String> {
    for assignment in &simple.assignments {
        match &assignment.value {
            AssignmentValue::Scalar(value) => word(value)?,
            AssignmentValue::Array(values) => values.iter().try_for_each(word)?,
        }
    }
    simple.redirects.iter().try_for_each(redirect)?;
    word(&simple.name)?;
    simple.args.iter().try_for_each(word)?;

    let bare = simple.name.parts.is_empty() || literal(&simple.name).as_deref() == Some("");
    if bare && simple.args.is_empty() {
        // A bare assignment.
        return Ok(());
    }
    let Some(name) = literal(&simple.name).filter(|name| !name.is_empty()) else {
        return Err("its command name is not a literal word".into());
    };
    if name.contains('/') {
        return Err(format!("it runs the file `{name}`"));
    }
    let Some(args_rule) = rule(&name) else {
        return Err(format!("`{name}` is not run virtually"));
    };
    let args: Vec<Arg> = simple.args.iter().map(classify).collect();
    for arg in &args {
        if let Arg::Literal(text) = arg {
            special_path(text)?;
        }
    }
    check_args(&name, &args_rule, &args)
}

fn check_args(name: &str, rule: &Args, args: &[Arg]) -> Result<(), String> {
    let (short, short_value, long, max_operands, counts) = match rule {
        Args::Any => return Ok(()),
        Args::Custom(check) => return check(args),
        Args::Flags {
            short,
            short_value,
            long,
            max_operands,
            counts,
        } => (*short, *short_value, *long, *max_operands, *counts),
    };
    let mut operands = 0;
    let mut options_done = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let text = match arg {
            Arg::Literal(text) => text.as_str(),
            Arg::Operand => {
                operands += 1;
                continue;
            }
            Arg::Unknown if options_done => {
                operands += 1;
                continue;
            }
            Arg::Unknown => {
                return Err(format!(
                    "an argument to `{name}` is not literal and could be an option"
                ));
            }
        };
        if options_done || text == "-" || !text.starts_with('-') {
            operands += 1;
            continue;
        }
        if text == "--" {
            options_done = true;
            continue;
        }
        if text.starts_with("--") {
            if long.contains(&text) {
                continue;
            }
            return Err(format!("`{name} {text}` is not run virtually"));
        }
        let cluster: Vec<char> = text[1..].chars().collect();
        let mut at = 0;
        while at < cluster.len() {
            let flag = cluster[at];
            if short_value.contains(flag) {
                let value: String = cluster[at + 1..].iter().collect();
                let value = if value.is_empty() {
                    match iter.next() {
                        Some(Arg::Literal(value)) => value.clone(),
                        _ => {
                            return Err(format!("the value of `{name} -{flag}` is not literal"));
                        }
                    }
                } else {
                    value
                };
                if counts && !is_count(&value) {
                    return Err(format!("`{name} -{flag} {value}` is not run virtually"));
                }
                break;
            }
            if !short.contains(flag) {
                return Err(format!("`{name} -{flag}` is not run virtually"));
            }
            at += 1;
        }
    }
    if let Some(max) = max_operands
        && operands > max
    {
        return Err(format!(
            "`{name}` with more than {max} operand(s) is not run virtually"
        ));
    }
    Ok(())
}

/// A count GNU and bashkit read alike: digits, optionally after `+`.
fn is_count(value: &str) -> bool {
    let digits = value.strip_prefix('+').unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn printf(args: &[Arg]) -> Result<(), String> {
    match args.first() {
        Some(Arg::Literal(first)) if first == "-v" => {
            Err("`printf -v` is not run virtually".into())
        }
        Some(Arg::Unknown) => Err("the format of `printf` is not literal".into()),
        _ => Ok(()),
    }
}

fn test(args: &[Arg]) -> Result<(), String> {
    for arg in args {
        if let Arg::Literal(text) = arg
            && TEST_OPERATORS_PROMOTED.contains(&text.as_str())
        {
            return Err(format!("`test {text}` is answered differently virtually"));
        }
    }
    Ok(())
}

/// `set` only to turn on or off `-e`, `-u`, `-x`-free strictness and
/// `pipefail` — what scripts put on their first line.
fn set(args: &[Arg]) -> Result<(), String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let Arg::Literal(text) = arg else {
            return Err("an argument to `set` is not literal".into());
        };
        let Some(flags) = text.strip_prefix('-').or_else(|| text.strip_prefix('+')) else {
            return Err("`set` with positional arguments is not run virtually".into());
        };
        let mut chars = flags.chars().peekable();
        while let Some(flag) = chars.next() {
            match flag {
                'e' | 'u' => {}
                'o' if chars.peek().is_none() => match iter.next() {
                    Some(Arg::Literal(option))
                        if matches!(option.as_str(), "pipefail" | "errexit" | "nounset") => {}
                    _ => {
                        return Err(
                            "`set -o` is only run virtually for pipefail, errexit and nounset"
                                .into(),
                        );
                    }
                },
                other => return Err(format!("`set -{other}` is not run virtually")),
            }
        }
    }
    Ok(())
}

fn ln(args: &[Arg]) -> Result<(), String> {
    let mut symbolic = false;
    let mut operands = 0;
    for arg in args {
        match arg {
            Arg::Literal(text) if text.starts_with('-') && text.len() > 1 => {
                for flag in text[1..].chars() {
                    match flag {
                        's' => symbolic = true,
                        'f' => {}
                        other => return Err(format!("`ln -{other}` is not run virtually")),
                    }
                }
            }
            Arg::Unknown => return Err("an argument to `ln` is not literal".into()),
            _ => operands += 1,
        }
    }
    if !symbolic {
        return Err("hard links are not run virtually".into());
    }
    if operands != 2 {
        return Err("`ln -s` is only run virtually with a target and a link name".into());
    }
    Ok(())
}

/// `command -v NAME...`, where bash and bashkit print the same thing: bash's
/// own builtins and keywords by name, and anything bashkit does not
/// implement by its path on `PATH`.
fn command_v(args: &[Arg]) -> Result<(), String> {
    let Some(Arg::Literal(first)) = args.first() else {
        return Err("`command` is only run virtually as `command -v`".into());
    };
    if first != "-v" || args.len() < 2 {
        return Err("`command` is only run virtually as `command -v`".into());
    }
    for arg in &args[1..] {
        let Arg::Literal(name) = arg else {
            return Err("a name given to `command -v` is not literal".into());
        };
        if name.starts_with('-') {
            return Err("`command -v` is only run virtually with plain names".into());
        }
        if !BASH_BUILTINS.contains(&name.as_str())
            && bashkit_builtins().iter().any(|builtin| builtin == name)
        {
            return Err(format!(
                "`command -v {name}` names a path in the sandbox and a builtin virtually"
            ));
        }
    }
    Ok(())
}

/// `sed` with scripts from a small, well-trodden subset: line selection with
/// `p`, `d` and `q`, and `s` with the `g`, `p` and `I`-free flags. The
/// regular-expression dialects agree on these; `sed`'s wider language is
/// where they stop agreeing.
fn sed(args: &[Arg]) -> Result<(), String> {
    let mut iter = args.iter();
    let mut scripts = Vec::new();
    let mut have_e = false;
    while let Some(arg) = iter.next() {
        let Arg::Literal(text) = arg else {
            if let Arg::Operand = arg {
                continue;
            }
            return Err("an argument to `sed` is not literal".into());
        };
        match text.as_str() {
            "-n" | "-E" | "-r" | "-nE" | "-En" | "-rn" | "-nr" => {}
            "-e" => match iter.next() {
                Some(Arg::Literal(script)) => {
                    scripts.push(script.clone());
                    have_e = true;
                }
                _ => return Err("a `sed -e` script is not literal".into()),
            },
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("`sed {other}` is not run virtually"));
            }
            other => {
                if !have_e && scripts.is_empty() {
                    scripts.push(other.to_owned());
                }
            }
        }
    }
    if scripts.is_empty() {
        return Err("`sed` without a script is not run virtually".into());
    }
    for script in &scripts {
        if !sed_script_ok(script) {
            return Err(format!("the sed script `{script}` is not run virtually"));
        }
    }
    Ok(())
}

fn sed_script_ok(script: &str) -> bool {
    script.split(';').all(|command| {
        let command = command.trim();
        let rest = skip_address(command);
        let Some(rest) = rest else { return false };
        let rest = rest.trim_start();
        match rest.chars().next() {
            Some('p' | 'd' | 'q') => rest.len() == 1,
            Some('s') => substitution_ok(&rest[1..]),
            _ => false,
        }
    })
}

/// Skips an optional address or address range of line numbers, `$`, or
/// `/regex/`.
fn skip_address(command: &str) -> Option<&str> {
    let mut rest = skip_one_address(command)?;
    if let Some(after) = rest.strip_prefix(',') {
        rest = skip_one_address(after)?;
        if rest.len() == after.len() {
            return None;
        }
    }
    Some(rest)
}

fn skip_one_address(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix('$') {
        return Some(rest);
    }
    if let Some(rest) = text.strip_prefix('/') {
        let end = rest.find('/')?;
        if rest[..end].contains('\\') {
            return None;
        }
        return Some(&rest[end + 1..]);
    }
    Some(text.trim_start_matches(|c: char| c.is_ascii_digit()))
}

fn substitution_ok(body: &str) -> bool {
    let mut chars = body.chars();
    let Some(delimiter) = chars.next() else {
        return false;
    };
    if delimiter.is_alphanumeric() || delimiter == '\\' || delimiter == '\n' {
        return false;
    }
    let parts: Vec<&str> = body[delimiter.len_utf8()..].split(delimiter).collect();
    if parts.len() != 3 {
        return false;
    }
    let flags_ok = parts[2].chars().all(|flag| matches!(flag, 'g' | 'p'));
    // Backreferences and GNU escapes are where the dialects part ways; keep
    // the plain `\(..\)`/`\1` grouping both accept.
    let escapes_ok = |text: &str| {
        let bytes = text.as_bytes();
        bytes.iter().enumerate().all(|(at, byte)| {
            *byte != b'\\'
                || bytes
                    .get(at + 1)
                    .is_some_and(|next| b"()1234567890./*[]^$&\\".contains(next))
        })
    };
    flags_ok && escapes_ok(parts[0]) && escapes_ok(parts[1])
}

fn redirect(redirect: &Redirect) -> Result<(), String> {
    if redirect.fd_var.is_some() {
        return Err("it redirects through a file descriptor variable".into());
    }
    word(&redirect.target)?;
    if matches!(
        redirect.kind,
        RedirectKind::Output
            | RedirectKind::Clobber
            | RedirectKind::Append
            | RedirectKind::Input
            | RedirectKind::OutputBoth
    ) && let Some(target) = literal(&redirect.target)
    {
        special_path(&target)?;
    }
    Ok(())
}

/// Refuses paths into kernel filesystems the virtual shell does not have.
fn special_path(text: &str) -> Result<(), String> {
    let normalized = format!("/{}", text.trim_start_matches('/'));
    for root in ["/proc", "/sys", "/dev"] {
        let under = normalized == root || normalized.starts_with(&format!("{root}/"));
        if text.starts_with('/') && under && normalized != "/dev/null" {
            return Err(format!("`{text}` exists only in the sandbox"));
        }
    }
    Ok(())
}

/// Checks the substitutions inside a word.
fn word(word: &Word) -> Result<(), String> {
    for part in &word.parts {
        match part {
            WordPart::Literal(_) | WordPart::Variable(_) => {}
            WordPart::CommandSubstitution(inner) => {
                commands(inner)?;
                listings_output(inner, true)?;
            }
            WordPart::ProcessSubstitution { .. } => {
                return Err("it uses process substitution".into());
            }
            WordPart::ArithmeticExpansion(expr) => expression(expr)?,
            other => {
                // Operands of `${...}` forms are kept as source text; a
                // substitution inside one is not visible as a command.
                let text = Word {
                    parts: vec![other.clone()],
                    quoted: false,
                    has_unquoted_glob: false,
                    part_quoted: Vec::new(),
                }
                .to_string();
                expression(&text)?;
            }
        }
    }
    Ok(())
}

fn expression(text: &str) -> Result<(), String> {
    if text.contains("$(") || text.contains('`') || text.contains("<(") || text.contains(">(") {
        return Err("it hides a command inside an expansion".into());
    }
    Ok(())
}

fn literal(word: &Word) -> Option<String> {
    let mut text = String::new();
    for part in &word.parts {
        match part {
            WordPart::Literal(piece) => text.push_str(piece),
            _ => return None,
        }
    }
    Some(text)
}

fn classify(word: &Word) -> Arg {
    if let Some(text) = literal(word) {
        return Arg::Literal(text);
    }
    match word.parts.first() {
        Some(WordPart::Literal(head)) if !head.is_empty() && !head.starts_with('-') => Arg::Operand,
        _ => Arg::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn virtual_(command: &str) {
        assert_eq!(decide(command), Decision::Virtual, "{command}");
    }

    fn promoted(command: &str) {
        assert!(
            matches!(decide(command), Decision::Promote(_)),
            "{command} should promote"
        );
    }

    #[test]
    fn everyday_probes_and_file_work_stay_virtual() {
        for command in [
            "ls /work",
            "cat -n /etc/os-release",
            "head -n 20 /src/main.rs && tail -n +2 /src/lib.rs",
            "grep -n -i todo /src/main.rs | sort | uniq -c",
            "mkdir -p /work/a && echo hi > /work/a/b.txt",
            "rm -rf /work/tmp",
            "command -v cargo",
            "command -v echo",
            "[ -d /opt/toolstore ] && echo yes",
            "x=$(cat /a | wc -l); echo \"$x\"",
            "for f in /work/*.rs; do echo \"$f\"; done",
            "set -euo pipefail; cat /a",
            "sed -n '10,20p' /a",
            "sed 's/foo/bar/g' /a",
            "echo hi > /dev/null 2>&1",
            "cd /work && pwd",
            "ln -s /a /b",
            "cat \"/work/$name\"",
        ] {
            virtual_(command);
        }
    }

    #[test]
    fn long_listings_stay_virtual_only_while_nothing_reads_them() {
        for command in [
            "ls -la /work",
            "ls -l",
            "ls -a /work",
            "ls -la /work 2>/dev/null | head -50",
            "ls -l /work | head -n 5 | tail -n 2",
            "echo before; ls -la /work && echo after",
            "if [ -d /work ]; then ls -la /work; fi",
            "ls -1 /work | sort",
            "ls /work | cut -c1-3",
        ] {
            virtual_(command);
        }
        for command in [
            "ls -l /work | cut -c1-10",
            "ls -la | sort",
            "ls -l /work | head -n 2 | cut -c1-3",
            "ls -l /work > /work/listing",
            "ls -l /work >> /work/listing",
            "ls -l /work &> /work/listing",
            "ls -l /work | head -n 2 > /work/listing",
            "x=$(ls -l /work); echo \"$x\"",
            "echo \"$(ls -a | head -n 1)\"",
            "(ls -l /work) | wc -l",
            "{ ls -l /work; } > /work/listing",
            "for d in /a; do ls -l \"$d\"; done | sort",
            "if true; then ls -a; fi | wc -l",
        ] {
            promoted(command);
        }
    }

    #[test]
    fn anything_off_the_list_promotes_the_whole_command() {
        for command in [
            "cargo build",
            "ls /work && cargo test",
            "echo $(python3 -c 'print(1)')",
            "git status",
            "curl https://example.com",
            "./run.sh",
            "/usr/bin/cat /a",
            "eval echo hi",
            "source /env",
            ". /env",
            "bash -c 'echo hi'",
            "$cmd /a",
            "f() { echo; }; f",
            "sleep 10 &",
            "cat <(echo a)",
            "time ls",
            "cat /proc/cpuinfo",
            "echo hi > /dev/stderr",
            "ls /a /b",
            "wc -l /a",
            "head -n -1 /a",
            "tail -f /log",
            "grep -r todo /work",
            "cat $file",
            "command -v cat",
            "command ls",
            "[ -L /bin ]",
            "[[ -w /etc ]]",
            "ln /a /b",
            "sed -i 's/a/b/' /a",
            "sed 'a\\foo' /a",
            "sed 's/a\\+/b/' /a",
            "set -x",
            "printf -v x %s 1",
            "echo ${x:-$(id)}",
            "echo $(( $(id -u) + 1 ))",
            "if true; then",
        ] {
            promoted(command);
        }
    }

    #[test]
    fn counts_and_clustered_flags_are_read_like_getopt() {
        virtual_("head -5 /a");
        virtual_("head -n5 /a");
        virtual_("rm -rf /a");
        virtual_("sort -k2 -t, /a");
        promoted("head -n x /a");
        promoted("rm -ri /a");
        virtual_("cat -- -n");
    }
}
