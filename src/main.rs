use clap::{Parser, Subcommand};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about = "Review and compare saved English net use reports")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "List connections in stable drive and remote-path order")]
    List {
        #[arg(help = "Saved English net use report (UTF-8)")]
        input: PathBuf,
        #[arg(long, value_parser = parse_drive, help = "Only show this drive, ignoring case (for example, Z:)")]
        drive: Option<String>,
        #[arg(long, help = "Only show this exact status, ignoring case")]
        status: Option<String>,
    },
    #[command(about = "Show added, removed, retargeted and status-changed connections")]
    Compare {
        #[arg(help = "Earlier saved report (UTF-8)")]
        before: PathBuf,
        #[arg(help = "Later saved report (UTF-8)")]
        after: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Connection {
    status: String,
    drive: String,
    remote: String,
    network: String,
}

impl Connection {
    fn key(&self) -> (u8, String) {
        if self.drive.is_empty() {
            (1, self.remote.to_lowercase())
        } else {
            (0, self.drive.to_ascii_uppercase())
        }
    }

    fn matches(&self, drive: Option<&str>, status: Option<&str>) -> bool {
        drive.is_none_or(|value| self.drive.eq_ignore_ascii_case(value))
            && status.is_none_or(|value| self.status.eq_ignore_ascii_case(value))
    }
}

fn parse_drive(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Ok(value.to_ascii_uppercase())
    } else {
        Err("expected a drive letter followed by a colon (for example, Z:)".into())
    }
}

fn valid_remote(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("\\\\") else {
        return false;
    };
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    !server.is_empty()
        && !server.chars().any(char::is_whitespace)
        && !matches!(server, "." | ".." | "?")
        && !share.trim().is_empty()
        && !matches!(share, "." | "..")
        && !value.contains('/')
}

fn header_columns(line: &str) -> Option<[usize; 4]> {
    if line.split_whitespace().collect::<Vec<_>>() != ["Status", "Local", "Remote", "Network"] {
        return None;
    }
    let mut columns = [0; 4];
    for (index, name) in ["Status", "Local", "Remote", "Network"].iter().enumerate() {
        columns[index] = line[..line.find(name)?].chars().count();
    }
    Some(columns)
}

fn field(chars: &[char], start: usize, end: usize) -> String {
    chars
        .get(start..end.min(chars.len()))
        .unwrap_or_default()
        .iter()
        .collect::<String>()
        .trim()
        .into()
}

fn parse_report(input: &str) -> Result<Vec<Connection>, String> {
    let mut columns = None;
    let mut complete = false;
    let mut empty = false;
    let mut connections = BTreeMap::new();
    let mut drives = BTreeSet::new();
    for (index, line) in input.trim_start_matches('\u{feff}').lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();
        let fail = |message: &str| format!("line {number}: {message}");
        if trimmed.is_empty() {
            continue;
        }
        if line.chars().any(|c| c.is_control()) {
            return Err(fail(
                "unexpected control character; use a fixed-width report",
            ));
        }
        if complete {
            return Err(fail("unexpected text after the completion message"));
        }
        if trimmed == "The command completed successfully." {
            if columns.is_none() && !empty {
                return Err(fail("completion message without a report header"));
            }
            complete = true;
            continue;
        }
        if columns.is_none() && !empty {
            if matches!(
                trimmed,
                "New connections will be remembered." | "New connections will not be remembered."
            ) {
                continue;
            }
            if trimmed == "There are no entries in the list." {
                empty = true;
                continue;
            }
            if let Some(found) = header_columns(line) {
                columns = Some(found);
                continue;
            }
            return Err(fail(
                "expected the Status, Local, Remote and Network header",
            ));
        }
        let Some([status_start, local_start, remote_start, network_start]) = columns else {
            return Err(fail("unexpected text after the empty report message"));
        };
        if trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-') {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        if chars.iter().take(status_start).any(|c| !c.is_whitespace()) {
            return Err(fail("unexpected text before the Status column"));
        }
        let status = field(&chars, status_start, local_start);
        let local = field(&chars, local_start, remote_start);
        let remote = field(&chars, remote_start, network_start);
        let network = field(&chars, network_start, chars.len());
        let drive = if local.is_empty() {
            String::new()
        } else {
            parse_drive(&local).map_err(|message| fail(&message))?
        };
        if !valid_remote(&remote) {
            return Err(fail(
                "remote path must contain a UNC server and share (for example, \\\\files\\team)",
            ));
        }
        if network.is_empty() {
            return Err(fail("missing network provider"));
        }
        if !drive.is_empty() && !drives.insert(drive.clone()) {
            return Err(fail(&format!("duplicate drive {drive}")));
        }
        let connection = Connection {
            status,
            drive,
            remote,
            network,
        };
        if connections.insert(connection.key(), connection).is_some() {
            return Err(fail("duplicate connection without a drive letter"));
        }
    }
    if columns.is_none() && !empty {
        return Err("missing Status, Local, Remote and Network header".into());
    }
    Ok(connections.into_values().collect())
}

fn read_report(path: &Path) -> Result<Vec<Connection>, String> {
    let input = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    parse_report(&input).map_err(|error| format!("{}: {error}", path.display()))
}

fn listing(connections: &[Connection], drive: Option<&str>, status: Option<&str>) -> String {
    let mut output = String::from("Local\tRemote\tStatus\tNetwork\n");
    for connection in connections.iter().filter(|c| c.matches(drive, status)) {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            connection.drive, connection.remote, connection.status, connection.network
        ));
    }
    output
}

fn comparison(before: &[Connection], after: &[Connection]) -> String {
    let before: BTreeMap<_, _> = before.iter().map(|c| (c.key(), c)).collect();
    let after: BTreeMap<_, _> = after.iter().map(|c| (c.key(), c)).collect();
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut output =
        String::from("Change\tLocal\tBefore remote\tAfter remote\tBefore status\tAfter status\n");
    for key in keys {
        let old = before.get(key).copied();
        let new = after.get(key).copied();
        let drive = new.or(old).map_or("", |c| c.drive.as_str());
        let mut changes = Vec::new();
        match (old, new) {
            (None, Some(_)) => changes.push("added"),
            (Some(_), None) => changes.push("removed"),
            (Some(old), Some(new)) => {
                if old.remote.to_lowercase() != new.remote.to_lowercase() {
                    changes.push("retargeted");
                }
                if !old.status.eq_ignore_ascii_case(&new.status) {
                    changes.push("status-changed");
                }
            }
            (None, None) => unreachable!(),
        }
        for change in changes {
            output.push_str(&format!(
                "{change}\t{drive}\t{}\t{}\t{}\t{}\n",
                old.map_or("", |c| c.remote.as_str()),
                new.map_or("", |c| c.remote.as_str()),
                old.map_or("", |c| c.status.as_str()),
                new.map_or("", |c| c.status.as_str())
            ));
        }
    }
    output
}

fn run(cli: Cli) -> Result<(), String> {
    let output = match cli.command {
        Command::List {
            input,
            drive,
            status,
        } => listing(&read_report(&input)?, drive.as_deref(), status.as_deref()),
        Command::Compare { before, after } => {
            comparison(&read_report(&before)?, &read_report(&after)?)
        }
    };
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(|error| format!("cannot write output: {error}"))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(rows: &[(&str, &str, &str, &str)]) -> String {
        let mut text = format!("{:<13}{:<10}{:<32}Network\n", "Status", "Local", "Remote");
        text.push_str(
            "-------------------------------------------------------------------------------\n",
        );
        for (status, drive, remote, network) in rows {
            text.push_str(&format!("{status:<13}{drive:<10}{remote:<32}{network}\n"));
        }
        text.push_str("The command completed successfully.\n");
        text
    }

    #[test]
    fn preserves_spaces_and_sorts_and_filters() {
        let text = report(&[
            (
                "OK",
                "z:",
                "\\\\files\\team work",
                "Microsoft Windows Network",
            ),
            (
                "Disconnected",
                "Y:",
                "\\\\archive\\backup",
                "Example Network Provider",
            ),
            (
                "",
                "",
                "\\\\files\\shared space",
                "Microsoft Windows Network",
            ),
        ]);
        let rows = parse_report(&text).unwrap();
        assert_eq!(rows[0].drive, "Y:");
        assert_eq!(rows[1].remote, "\\\\files\\team work");
        assert_eq!(rows[2].drive, "");
        assert_eq!(rows[0].network, "Example Network Provider");
        let output = listing(&rows, Some("y:"), Some("disCONNECTED"));
        assert!(output.contains("Y:\t\\\\archive\\backup\tDisconnected"));
        assert_eq!(output.lines().count(), 2);
    }

    #[test]
    fn rejects_invalid_unc_and_duplicate_drives_with_line_numbers() {
        let invalid = report(&[("OK", "Z:", "\\\\files", "Microsoft Windows Network")]);
        assert!(
            parse_report(&invalid)
                .unwrap_err()
                .starts_with("line 3: remote path")
        );
        let duplicate = report(&[
            ("OK", "Z:", "\\\\files\\one", "Provider"),
            ("OK", "z:", "\\\\files\\two", "Provider"),
        ]);
        assert_eq!(
            parse_report(&duplicate).unwrap_err(),
            "line 4: duplicate drive Z:"
        );
    }

    #[test]
    fn compares_all_changes_and_ignores_path_and_status_case() {
        let before = parse_report(&report(&[
            ("OK", "A:", "\\\\files\\old", "Provider"),
            ("OK", "B:", "\\\\files\\removed", "Provider"),
            ("OK", "", "\\\\FILES\\same", "Provider"),
        ]))
        .unwrap();
        let after = parse_report(&report(&[
            ("Disconnected", "a:", "\\\\files\\new", "Provider"),
            ("OK", "C:", "\\\\files\\added", "Provider"),
            ("ok", "", "\\\\files\\SAME", "Provider"),
        ]))
        .unwrap();
        let changes = comparison(&before, &after);
        let labels: Vec<_> = changes
            .lines()
            .skip(1)
            .map(|line| line.split('\t').next().unwrap())
            .collect();
        assert_eq!(labels, ["retargeted", "status-changed", "removed", "added"]);
    }

    #[test]
    fn handles_empty_reports_and_rejects_unrecognised_text() {
        assert!(
            parse_report(
                "New connections will not be remembered.\n\nThere are no entries in the list.\n"
            )
            .unwrap()
            .is_empty()
        );
        assert!(parse_report("").is_err());
        assert!(
            parse_report("unexpected\n")
                .unwrap_err()
                .starts_with("line 1:")
        );
        assert!(parse_report(&(report(&[]) + "unexpected\n")).is_err());
    }
}
