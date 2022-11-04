/// Analyzes the current git diff and only performs eslint on the minimal number of changed packages

#[macro_use]
extern crate lazy_static;

use anyhow::Error;
use clap::Parser;
use colored::Colorize;
use fancy_regex::Regex;
use ops::git::diff_name_status_since_branched::*;
use serde_yaml::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PRE_COMMIT_CONFIG_FILE_NAME: &str = ".pre-commit-config.yaml";

lazy_static! {
    static ref REPLACE_NEWLINES_REGEX: Regex = Regex::new(r"\n *").unwrap();
}

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about, long_about = None, trailing_var_arg=true)]
struct Args {
    /// path to .pre-commit-config.yaml
    pre_commit_config_path: Option<PathBuf>,
    /// whether to print commands prior to running
    #[clap(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Error> {
    let Args {
        pre_commit_config_path,
        verbose,
    } = Args::parse();

    let file_regex = get_eslint_file_regex(pre_commit_config_path)?;

    if verbose {
        println!(
            "{}",
            format!("matching files with regex: {file_regex}").dimmed()
        );
    }

    let text = git_diff_name_status_since_last_branch()?;
    let git_statuses = parse_git_statuses(&text)?;
    let js_file_names = git_statuses
        .iter()
        .filter_map(GitStatus::new_file_name)
        .filter_map(|file_name| {
            file_regex
                .is_match(file_name)
                .ok()
                .and_then(|is_match| match is_match {
                    true => Some(file_name),
                    _ => None,
                })
        })
        .collect::<Vec<_>>();

    if js_file_names.is_empty() {
        println!("{}", "no files to lint".dimmed());
        return Ok(());
    }

    if verbose {
        println!(
            "{}",
            format!("eslint --fix {}", js_file_names.join(" ")).dimmed()
        );
    }

    let output = Command::new("eslint")
        .arg("--fix")
        .args(js_file_names)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()?;

    if !output.status.success() {
        return Err(Error::msg(
            output
                .status
                .code()
                .map(|code| format!("eslint failed with status {code}"))
                .unwrap_or_else(|| String::from("eslint failed")),
        ));
    }

    Ok(())
}

fn get_eslint_file_regex(pre_commit_config_path: Option<PathBuf>) -> Result<Regex, Error> {
    let pre_commit_config_path = match pre_commit_config_path {
        Some(pre_commit_config_path) => Path::new(&pre_commit_config_path).to_path_buf(),
        None => {
            let default_path = Path::new(PRE_COMMIT_CONFIG_FILE_NAME).to_path_buf();
            if !Path::exists(&default_path) {
                return Err(Error::msg(format!("unable to find pre-commit config file, try passing in a path with the --pre-commit-config-path flag")));
            }
            default_path
        }
    };
    let pre_commit_config_path_display = pre_commit_config_path.display();

    let pre_commit_config_text = fs::read_to_string(&pre_commit_config_path)?;
    let pre_commit_config: Value =
        serde_yaml::from_str(&pre_commit_config_text).map_err(|err| {
            Error::msg(format!(
                "unable to parse `{pre_commit_config_path_display}`: {err}"
            ))
        })?;

    let eslint_hook_config = pre_commit_config
        .get("repos")
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: no value `repos` found")))?
        .as_sequence()
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: expected `repos` to be a sequence")))?
        .iter()
        .find(|repo| repo.get("repo").map(|repo| repo == "local").unwrap_or_default())
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: no repo found with repo \"local\"")))?
        .get("hooks")
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: no value `repos[repo == \"local\"].hooks` found")))?
        .as_sequence()
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: expected `repos[repo == \"local\"].hooks` to be a sequence")))?
        .iter()
        .find(|repo| repo.get("id").map(|id| id == "eslint").unwrap_or_default())
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: no local hook found with id \"eslint\"")))?
        .as_mapping()
        .ok_or_else(|| {
            Error::msg(format!(
                "unable to parse `{pre_commit_config_path_display}`: expected `repos[repo == \"local\"].hooks[id == \"eslint\"]` to be a mapping"
            ))
        })?;

    let eslint_hook_config_files = eslint_hook_config
        .get("files")
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: no value `repos[repo == \"local\"].hooks[id == \"eslint\"].files` found, set a file filter for the javascript file extensions to lint (should be a valid regex)")))?
        .as_str()
        .ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: expected `repos[repo == \"local\"].hooks[id == \"eslint\"].files` to be a string, set a file filter for the javascript file extensions to lint (should be a valid regex)")))?;

    let eslint_hook_config_files = REPLACE_NEWLINES_REGEX.replace_all(eslint_hook_config_files, "");

    let eslint_hook_config_exclude = eslint_hook_config
        .get("exclude")
        .map(|exclude| exclude.as_str().ok_or_else(|| Error::msg(format!("unable to parse `{pre_commit_config_path_display}`: expected `repos[repo == \"local\"].hooks[id == \"eslint\"].exclude` to be a string, `exclude` should be a filter for the javascript file extensions to *not* lint (should be a valid regex non-capturing group i.e. looks like `(?!regex-here)`)"))))
        .transpose()?;

    let eslint_hook_config_files = Regex::new(&eslint_hook_config_files)
        .map_err(|err| Error::msg(format!("unable to parse `repos[repo == \"local\"].hooks[id == \"eslint\"].files` as a valid regex: {err}")))?;

    let eslint_hook_config_exclude = eslint_hook_config_exclude
        .map(|eslint_hook_config_exclude| {
            let eslint_hook_config_exclude = REPLACE_NEWLINES_REGEX.replace_all(eslint_hook_config_exclude, "");
            Regex::new(&eslint_hook_config_exclude)
                .map_err(|err| Error::msg(format!("unable to parse `repos[repo == \"local\"].hooks[id == \"eslint\"].exclude` as a valid regex: {err}")))
        })
        .transpose()?;

    Ok(match eslint_hook_config_exclude {
        Some(eslint_hook_config_exclude) => {
            Regex::new(&format!("{}{}", eslint_hook_config_exclude.as_str(), eslint_hook_config_files.as_str())).map_err(|err| {
                Error::msg(format!("an unexpected error ocurred: unable to combine the files regex and exclude regex into a combined regex: {err}"))
            })?
        }
        None => eslint_hook_config_files,
    })
}
