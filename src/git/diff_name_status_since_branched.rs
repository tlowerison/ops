/// Analyzes the current git diff and only performs clippy on the minimal number of changed packages
use anyhow::Error;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

const REMOTE: &str = "origin";

pub fn git_diff_name_status_since_last_branch() -> Result<String, Error> {
    let mut child = Command::new("git")
        .arg("branch")
        .stdout(Stdio::piped())
        .spawn()?;
    let output = Command::new("grep")
        .arg("*")
        .stdin(child.stdout.take().unwrap())
        .output()?;
    let branch = String::from_utf8_lossy(&output.stdout);
    let branch = &branch.trim()[2..];

    let mut remote_branch = None;

    if branch.len() > REMOTE.len()
        && &branch[..REMOTE.len()] == REMOTE
        && &branch[REMOTE.len()..REMOTE.len() + 1] == "/"
    {
        remote_branch = Some(branch.to_string());
    } else {
        let output = Command::new("git")
            .args(["rev-list", "--first-parent", &format!("{REMOTE}/{branch}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        if output.status.success() {
            remote_branch = Some(format!("{REMOTE}/{branch}"));
        }
    }

    let mut base_commit = None;

    if let Some(remote_branch) = remote_branch.as_ref() {
        let output = Command::new("git")
            .args(["rev-parse", &format!("{remote_branch}~0")])
            .output()?;
        let remote_branch_head = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let output = Command::new("git")
            .args(["merge-base", "--is-ancestor", &remote_branch_head, "HEAD"])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .output()?;
        if output.status.success() {
            base_commit = Some(remote_branch_head);
        }
    }

    if base_commit.is_none() {
        let cur_branch = format!("* {branch}");

        let mut rev_list = Command::new("git")
            .args(["rev-list", "--first-parent", branch])
            .stdout(Stdio::piped())
            .spawn()?;

        let mut branch_contains = Command::new("xargs")
            .args([
                "-n1",
                "-I",
                "{}",
                "sh",
                "-c",
                "git branch --contains {} && echo 'COMMIT: {}'",
            ])
            .stdin(rev_list.stdout.take().unwrap())
            .stdout(Stdio::piped())
            .spawn()?;

        let mut branch_contains_lines =
            BufReader::new(branch_contains.stdout.take().unwrap()).lines();

        for line in branch_contains_lines.by_ref().flatten() {
            let line = line.trim();
            if line.len() >= 8 && &line[..8] == "COMMIT: " {
                continue;
            }
            if line != cur_branch {
                break;
            }
        }

        rev_list.kill().ok();
        branch_contains.kill().ok();

        for line in branch_contains_lines.by_ref().flatten() {
            if line.len() >= 8 && &line[..8] == "COMMIT: " {
                base_commit = Some(line[8..].to_string());
            }
        }
    }

    let base_commit =
        base_commit.ok_or_else(|| Error::msg("unable to find base commit for pre-receive hook"))?;

    let output = Command::new("git")
        .args(["diff", "--name-status", &base_commit])
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Copy, Clone, Debug)]
pub enum GitStatus<'a> {
    Added { file: &'a str },
    Deleted { file: &'a str },
    FileTypeChanged { file: &'a str },
    Modified { file: &'a str },
    Renamed { old: &'a str, new: &'a str },
}

impl GitStatus<'_> {
    pub fn new_file_name(&self) -> Option<&str> {
        match self {
            Self::Added { file } => Some(file),
            Self::Deleted { .. } => None,
            Self::FileTypeChanged { file } => Some(file),
            Self::Modified { file } => Some(file),
            Self::Renamed { new, .. } => Some(new),
        }
    }

    pub fn old_file_name(&self) -> Option<&str> {
        match self {
            Self::Added { .. } => None,
            Self::Deleted { file } => Some(file),
            Self::FileTypeChanged { file } => Some(file),
            Self::Modified { file } => Some(file),
            Self::Renamed { old, .. } => Some(old),
        }
    }
}

pub fn parse_git_statuses(text: &str) -> Result<Vec<GitStatus<'_>>, Error> {
    let lines = text.trim().split('\n');

    let mut git_statuses = vec![];
    for line in lines.into_iter().filter(|x| x.len() >= 3) {
        if let Some(status) = line.split_whitespace().next() {
            if status == "A" {
                git_statuses.push(GitStatus::Added {
                    file: line[2..].trim(),
                });
            } else if status == "D" {
                git_statuses.push(GitStatus::Deleted {
                    file: line[2..].trim(),
                });
            } else if status == "M" || status == "MM" || status == "AM" {
                git_statuses.push(GitStatus::Modified {
                    file: line[2..].trim(),
                });
            } else if &status[0..1] == "R" {
                let mut matched = false;
                for (i, window) in <str as AsRef<[u8]>>::as_ref(line[2..].trim())
                    .windows(4)
                    .enumerate()
                {
                    let indices = if window == b" -> " {
                        Some((2, i + 3, i + 7))
                    } else if &window[0..1] == b"	" {
                        Some((2, i + 3, i + 4))
                    } else {
                        None
                    };
                    let (start1, end1, start2) = match indices {
                        Some(indices) => indices,
                        None => continue,
                    };
                    let old = line[start1..end1].trim();
                    let new = line[start2..].trim();
                    if old == new {
                        git_statuses.push(GitStatus::Modified { file: new });
                    } else {
                        git_statuses.push(GitStatus::Renamed { old, new });
                    }
                    matched = true;
                    break;
                }
                if !matched {
                    println!("{status} {}", &status[0..1]);
                    return Err(Error::msg(format!("unsupported git status: {line}")));
                }
            } else if status == "T" {
                git_statuses.push(GitStatus::FileTypeChanged {
                    file: line[2..].trim(),
                });
            } else {
                return Err(Error::msg(format!("unsupported git status: {line}")));
            }
        }
    }

    Ok(git_statuses)
}
