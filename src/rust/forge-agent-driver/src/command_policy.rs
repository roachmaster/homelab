//! Translate a small command grammar to fixed argv vectors for
//! fixture tests (command_policy.py).
//!
//! The tokenizer mirrors shlex with punctuation_chars="|;&<>()" in
//! POSIX mode. Commands containing $, ` or \ are rejected before
//! lexing, exactly as upstream, so the lexer's escape paths are
//! unreachable in practice.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError(pub String);

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CommandError {}

fn error(message: &str) -> CommandError {
    CommandError(message.to_string())
}

const PUNCTUATION: &str = "|;&<>()";

fn tokenize(command: &str) -> Result<Vec<String>, CommandError> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_punct: Option<bool> = None;
    let mut chars = command.chars();
    let flush = |tokens: &mut Vec<String>, current: &mut String, kind: &mut Option<bool>| {
        if kind.is_some() {
            tokens.push(std::mem::take(current));
            *kind = None;
        }
    };
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            flush(&mut tokens, &mut current, &mut current_punct);
            continue;
        }
        if c == '\'' || c == '"' {
            if current_punct == Some(true) {
                flush(&mut tokens, &mut current, &mut current_punct);
            }
            current_punct = Some(false);
            let mut closed = false;
            for inner in chars.by_ref() {
                if inner == c {
                    closed = true;
                    break;
                }
                current.push(inner);
            }
            if !closed {
                return Err(error("Invalid command quoting"));
            }
            continue;
        }
        if PUNCTUATION.contains(c) {
            if current_punct == Some(false) {
                flush(&mut tokens, &mut current, &mut current_punct);
            }
            current_punct = Some(true);
            current.push(c);
            continue;
        }
        if current_punct == Some(true) {
            flush(&mut tokens, &mut current, &mut current_punct);
        }
        current_punct = Some(false);
        current.push(c);
    }
    flush(&mut tokens, &mut current, &mut current_punct);
    Ok(tokens)
}

pub fn parse_command(command: &str, files: &[String]) -> Result<Vec<Vec<String>>, CommandError> {
    if command.is_empty()
        || command.chars().count() > 512
        || command.chars().any(|c| matches!(c, '\r' | '\n' | '\0'))
    {
        return Err(error("Expected one command line"));
    }
    if command.chars().any(|c| matches!(c, '$' | '`' | '\\')) {
        return Err(error(
            "Shell expansions and escapes are outside the fixture grammar",
        ));
    }
    let tokens = tokenize(command)?;
    if tokens.iter().filter(|token| token.as_str() == "|").count() > 1 {
        return Err(error("Only one pipe is supported"));
    }
    if let Some(split) = tokens.iter().position(|token| token == "|") {
        if tokens[split + 1..] != ["uniq".to_string()] {
            return Err(error("The supported pipe target is uniq"));
        }
        return Ok(vec![
            file_command(&tokens[..split], files)?,
            vec!["/usr/bin/uniq".to_string()],
        ]);
    }
    Ok(vec![file_command(&tokens, files)?])
}

fn is_grep_flag(flag: &str) -> bool {
    flag.len() >= 2 && flag.starts_with('-') && flag[1..].chars().all(|c| matches!(c, 'F' | 'n'))
}

fn is_grep_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && pattern.chars().count() <= 80
        && pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | ':' | '-'))
}

fn cut_arguments(joined: &str) -> Option<String> {
    // -d ?(,) -f ?([1-9]) over the space-joined options.
    let rest = joined.strip_prefix("-d")?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let rest = rest.strip_prefix(',')?;
    let rest = rest.strip_prefix(" -f")?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    if rest.len() == 1 && rest.chars().all(|c| c.is_ascii_digit() && c != '0') {
        Some(rest.to_string())
    } else {
        None
    }
}

pub fn file_command(tokens: &[String], files: &[String]) -> Result<Vec<String>, CommandError> {
    if tokens.len() < 2 || !files.contains(tokens.last().unwrap()) {
        return Err(error("Expected one fixture filename"));
    }
    let name = tokens[0].as_str();
    let options = &tokens[1..tokens.len() - 1];
    let filename = tokens.last().unwrap().clone();
    if name == "wc" && (options == ["-l".to_string()] || options == ["--lines".to_string()]) {
        return Ok(vec![
            "/usr/bin/wc".to_string(),
            "-l".to_string(),
            "--".to_string(),
            filename,
        ]);
    }
    if name == "sort"
        && (options.is_empty()
            || options == ["-u".to_string()]
            || options == ["--unique".to_string()])
    {
        let mut argv = vec!["/usr/bin/sort".to_string()];
        if !options.is_empty() {
            argv.push("-u".to_string());
        }
        argv.push("--".to_string());
        argv.push(filename);
        return Ok(argv);
    }
    if name == "head" && options.len() == 2 && options[0] == "-n" && {
        let value = &options[1];
        (value.len() == 1 || value.len() == 2)
            && value.chars().all(|c| c.is_ascii_digit())
            && !value.starts_with('0')
    } {
        return Ok(vec![
            "/usr/bin/head".to_string(),
            "-n".to_string(),
            options[1].clone(),
            "--".to_string(),
            filename,
        ]);
    }
    if name == "grep" && !options.is_empty() {
        let (flags, pattern) = options.split_at(options.len() - 1);
        let mut flags: Vec<String> = flags.to_vec();
        if flags.last().map(String::as_str) == Some("--") {
            flags.pop();
        }
        if flags.iter().all(|flag| is_grep_flag(flag)) && is_grep_pattern(&pattern[0]) {
            let mut argv = vec!["/usr/bin/grep".to_string()];
            argv.extend(flags);
            argv.push("--".to_string());
            argv.push(pattern[0].clone());
            argv.push(filename);
            return Ok(argv);
        }
    }
    if name == "cut" {
        if let Some(field) = cut_arguments(&options.join(" ")) {
            return Ok(vec![
                "/usr/bin/cut".to_string(),
                "-d".to_string(),
                ",".to_string(),
                "-f".to_string(),
                field,
                "--".to_string(),
                filename,
            ]);
        }
    }
    Err(error("Unsupported command or arguments"))
}

#[derive(Debug)]
pub enum ExecuteError {
    Timeout,
    Io(std::io::Error),
}

/// Run the argv pipeline as (uid, gid) in directory, feeding each
/// stage the previous stage's stdout (execute in command_policy.py).
/// Supplementary groups are not cleared (CommandExt::groups is
/// unstable in this toolchain) — the recorded divergence shared with
/// the S6 process factory.
pub fn execute(
    commands: &[Vec<String>],
    directory: &Path,
    uid: u32,
    gid: u32,
) -> Result<(i32, String, String), ExecuteError> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    let mut previous: Vec<u8> = Vec::new();
    for argv in commands {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .envs([
                ("PATH", "/usr/bin:/bin"),
                ("LC_ALL", "C"),
                ("HOME", "/nonexistent"),
            ])
            .uid(uid)
            .gid(gid);
        let mut child = command.spawn().map_err(ExecuteError::Io)?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&previous);
            drop(stdin);
        }
        let mut stdout_pipe = child.stdout.take();
        let out_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut data = Vec::new();
            if let Some(pipe) = stdout_pipe.as_mut() {
                let _ = pipe.read_to_end(&mut data);
            }
            data
        });
        let mut stderr_pipe = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut data = Vec::new();
            if let Some(pipe) = stderr_pipe.as_mut() {
                let _ = pipe.read_to_end(&mut data);
            }
            data
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(ExecuteError::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(ExecuteError::Io(error)),
            }
        };
        let stdout = out_thread.join().unwrap_or_default();
        let stderr = err_thread.join().unwrap_or_default();
        let code = status
            .code()
            .unwrap_or_else(|| status.signal().map(|s| -s).unwrap_or(-1));
        if code != 0 {
            return Ok((
                code,
                String::from_utf8_lossy(&stdout).into_owned(),
                String::from_utf8_lossy(&stderr).into_owned(),
            ));
        }
        previous = stdout;
    }
    Ok((
        0,
        String::from_utf8_lossy(&previous).into_owned(),
        String::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        vec!["notes.txt".to_string(), "names.txt".to_string()]
    }

    #[test]
    fn grammar_translations() {
        assert_eq!(
            parse_command("wc -l notes.txt", &files()).unwrap(),
            vec![vec!["/usr/bin/wc", "-l", "--", "notes.txt"]]
        );
        assert_eq!(
            parse_command("sort -u names.txt | uniq", &files()).unwrap(),
            vec![
                vec!["/usr/bin/sort", "-u", "--", "names.txt"],
                vec!["/usr/bin/uniq"]
            ]
        );
        assert_eq!(
            parse_command("grep -n 'beta: 1' names.txt", &files()).unwrap(),
            vec![vec!["/usr/bin/grep", "-n", "--", "beta: 1", "names.txt"]]
        );
        assert_eq!(
            parse_command("cut -d, -f2 names.txt", &files()).unwrap(),
            vec![vec![
                "/usr/bin/cut",
                "-d",
                ",",
                "-f",
                "2",
                "--",
                "names.txt"
            ]]
        );
        assert_eq!(
            parse_command("head -n 12 notes.txt", &files()).unwrap(),
            vec![vec!["/usr/bin/head", "-n", "12", "--", "notes.txt"]]
        );
    }

    #[test]
    fn grammar_rejections() {
        assert!(parse_command("wc -l notes.txt; rm x", &files()).is_err());
        assert!(parse_command("wc -l $HOME", &files()).is_err());
        assert!(parse_command("wc -l other.txt", &files()).is_err());
        assert!(parse_command("sort names.txt | sort", &files()).is_err());
        assert!(parse_command("head -n 0 notes.txt", &files()).is_err());
        assert!(parse_command("grep -x beta names.txt", &files()).is_err());
    }

    #[test]
    fn execute_runs_pipeline() {
        let directory =
            std::env::temp_dir().join(format!("forge-driver-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("names.txt"), "beta\nalpha\nbeta\n").unwrap();
        let commands = parse_command("sort names.txt | uniq", &files()).unwrap();
        let (code, stdout, _) = execute(
            &commands,
            &directory,
            nix::unistd::geteuid().as_raw(),
            nix::unistd::getegid().as_raw(),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(stdout, "alpha\nbeta\n");
        std::fs::remove_dir_all(&directory).ok();
    }
}
