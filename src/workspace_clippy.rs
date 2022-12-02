/// Analyzes the current git diff and only performs clippy on the minimal number of changed packages.
/// Note that if any changes are made to the workspace level Cargo.toml or Cargo.lock a full workspace
/// level run of cargo clippy is currently required (to capture the case of breaking changes due to
/// changed external dependencies).
use crate::git::diff_name_status_since_branched::*;
use anyhow::Error;
use clap::Parser;
use colored::Colorize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use toml::Value;

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about, long_about = None, trailing_var_arg=true)]
pub struct WorkspaceClippyArgs {
    /// whether to print commands prior to running
    #[clap(short, long)]
    pub verbose: bool,
}

pub fn workspace_clippy(worspace_clippy_args: WorkspaceClippyArgs) -> Result<(), Error> {
    let WorkspaceClippyArgs { verbose } = worspace_clippy_args;
    let text = git_diff_name_status_since_last_branch()?;
    let git_statuses = parse_git_statuses(&text)?;

    let mut package_paths = HashMap::<PathBuf, PathBuf>::default();
    let mut no_package_dirs = HashSet::<PathBuf>::default();
    let mut no_package_paths = HashSet::<PathBuf>::default();

    for git_status in git_statuses {
        let mut existing = None;
        let mut removed = None;
        match git_status {
            GitStatus::Added { file } | GitStatus::FileTypeChanged { file } | GitStatus::Modified { file } => {
                existing = Some(Path::new(file));
            }
            GitStatus::Deleted { file } => {
                removed = Some(Path::new(file));
            }
            GitStatus::Renamed { old, new } => {
                removed = Some(Path::new(old));
                existing = Some(Path::new(new));
            }
        }

        if let Some(existing) = existing {
            let file_name = existing.display().to_string();
            if file_name == "Cargo.toml" || file_name == "Cargo.lock" {
                return workspace_run(verbose);
            }
            get_cargo_package_of_file(existing, &mut package_paths, &mut no_package_dirs, &mut no_package_paths)?;
        }

        if let Some(removed) = removed {
            let file_name = removed.display().to_string();
            if file_name == "Cargo.toml" || file_name == "Cargo.lock" {
                return workspace_run(verbose);
            }
            get_cargo_package_of_file(removed, &mut package_paths, &mut no_package_dirs, &mut no_package_paths)?;
        }
    }

    if !no_package_paths.is_empty() {
        let formatted_paths = no_package_paths.into_iter().map(|x| x.display().to_string()).collect::<Vec<_>>().join("\n - ");
        return Err(Error::msg(format!("cannot run ops-clippy: rust files were found outside of a cargo package:\n - {formatted_paths}")));
    }

    let package_paths = package_paths.into_values().collect::<HashSet<_>>();

    let workspace_cargo = fs::read_to_string("Cargo.toml")?.parse::<Value>()?;
    let workspace_dependencies = workspace_cargo
        .get("workspace")
        .ok_or_else(|| Error::msg("cannot parse workspace Cargo.toml: missing key `workspace`"))?
        .get("dependencies")
        .ok_or_else(|| Error::msg("cannot parse workspace Cargo.toml: missing key `workspace.dependencies`"))?
        .as_table()
        .ok_or_else(|| Error::msg("cannot parse workspace Cargo.toml: key `workspace.dependencies` must be a table"))?;

    let mut internal_crate_path_map = HashMap::<String, PathBuf>::default();

    let mut package_cargos = HashMap::<String, Value>::from_iter(
        package_paths
            .iter()
            .map(|package_path| {
                let package_cargo = fs::read_to_string(package_path.join("Cargo.toml"))?.parse::<Value>()?;
                let package_name = package_cargo
                    .get("package")
                    .ok_or_else(|| Error::msg(format!("cannot parse `{}/Cargo.toml`: missing key `package`", package_path.display())))?
                    .get("name")
                    .ok_or_else(|| Error::msg(format!("cannot parse `{}/Cargo.toml`: missing key `package.name`", package_path.display())))?
                    .as_str()
                    .ok_or_else(|| Error::msg(format!("cannot parse `{}/Cargo.toml`: key `package.name` must be a string", package_path.display())))?
                    .to_string();
                internal_crate_path_map.insert(package_name.clone(), package_path.to_path_buf());
                Ok((package_name, package_cargo))
            })
            .collect::<Result<Vec<_>, Error>>()?
            .into_iter(),
    );

    let changed_package_names = package_cargos.keys().map(String::from).collect::<HashSet<_>>();

    let mut external_crates = HashSet::<&str>::default();

    for (package_name, spec) in workspace_dependencies {
        match spec.get("path") {
            Some(path) => {
                let path = path
                    .as_str()
                    .ok_or_else(|| Error::msg(format!("cannot parse workspace Cargo.toml: key `package.{package_name}.path` must be a string")))?;
                internal_crate_path_map.insert(package_name.clone(), Path::new(path).to_path_buf());
            }
            None => {
                external_crates.insert(package_name);
            }
        };
    }

    let mut top_level_changed_package_names = changed_package_names;

    let mut queue = VecDeque::from_iter(package_cargos.keys().map(String::from));
    let mut analyzed_package_names = HashSet::<String>::default();
    while !queue.is_empty() {
        let package_name = queue.pop_front().unwrap();
        analyzed_package_names.insert(package_name.clone());

        if !package_cargos.contains_key(&package_name) {
            let package_path = internal_crate_path_map
                .get(&package_name)
                .ok_or_else(|| Error::msg(format!("an unexpected error occurred: unable to find path to crate {package_name}")))?;
            let package_cargo = fs::read_to_string(package_path.join("Cargo.toml"))?.parse::<Value>()?;
            package_cargos.insert(package_name.clone(), package_cargo);
        }
        let package_cargo = package_cargos.get(&package_name).unwrap();

        let package_dependencies = match package_cargo.get("dependencies") {
            Some(package_dependencies) => package_dependencies
                .as_table()
                .ok_or_else(|| Error::msg(format!("cannot parse {package_name} Cargo.toml: key `dependencies` must be a table")))?,
            None => continue,
        };

        for (package_dependency_name, _) in package_dependencies {
            top_level_changed_package_names.remove(package_dependency_name);
            if !analyzed_package_names.contains(package_dependency_name) && internal_crate_path_map.contains_key(package_dependency_name) {
                queue.push_back(package_dependency_name.clone());
            }
        }
    }

    if verbose {
        if top_level_changed_package_names.is_empty() {
            println!("{}", "no package changes found".dimmed());
        } else {
            println!("{}", "found changes in these packages (and possibly in their internal dependencies):".dimmed());
            for package_name in top_level_changed_package_names.iter() {
                let package_path = internal_crate_path_map
                    .get(&**package_name)
                    .ok_or_else(|| Error::msg(format!("an unexpected error occurred: unable to find package location with name `{package_name}`")))?;
                println!("{}", format!(" - {package_name} ({})", package_path.display()).dimmed());
            }
            println!();
        }
    }
    for package_name in top_level_changed_package_names {
        let cmd = "cargo";
        let args =
            ["clippy", "--package", &package_name, "--fix", "--allow-dirty", "--allow-staged", "--all-features", "-Zunstable-options", "--", "-D", "warnings"];
        if verbose {
            println!("{}", vec![cmd, &args.join(" ")].join(" ").dimmed());
        }
        let output = Command::new(cmd).args(args).stdout(Stdio::inherit()).stderr(Stdio::inherit()).output()?;

        if !output.status.success() {
            return Err(Error::msg(""));
        }
    }

    Ok(())
}

fn workspace_run(verbose: bool) -> Result<(), Error> {
    if verbose {
        println!("{}", "found changes in workspace Cargo.toml, requires full clippy rerun".dimmed());
    }
    let cmd = "cargo";
    let args = ["clippy", "--fix", "--allow-dirty", "--allow-staged", "--all-features", "-Zunstable-options", "--", "-D", "warnings"];
    if verbose {
        println!("{}", vec![cmd, &args.join(" ")].join(" ").dimmed());
    }
    let output = Command::new(cmd).args(args).stdout(Stdio::inherit()).stderr(Stdio::inherit()).output()?;

    if !output.status.success() {
        return Err(Error::msg(""));
    }
    Ok(())
}

fn get_cargo_package_of_file(
    path: &Path,
    package_paths: &mut HashMap<PathBuf, PathBuf>,
    no_package_dirs: &mut HashSet<PathBuf>,
    no_package_paths: &mut HashSet<PathBuf>,
) -> Result<(), Error> {
    match (&*path.display().to_string(), path.extension().and_then(std::ffi::OsStr::to_str)) {
        ("Cargo.toml", _) | (_, Some("rs")) => {}
        _ => return Ok(()),
    };
    let mut cur_path = path;
    let mut package_sub_dirs = vec![];
    while let Some(parent) = cur_path.parent() {
        package_sub_dirs.push(parent);
        if no_package_dirs.contains(parent) {
            break;
        }

        if let Some(package_path) = package_paths.get(parent).map(|package_path| package_path.to_path_buf()) {
            for dir in package_sub_dirs {
                package_paths.insert(dir.to_path_buf(), package_path.clone());
            }
            return Ok(());
        }
        if parent.join("Cargo.toml").exists() {
            for dir in package_sub_dirs {
                package_paths.insert(dir.to_path_buf(), parent.to_path_buf());
            }
            return Ok(());
        }

        cur_path = parent;
    }

    for dir in package_sub_dirs {
        no_package_dirs.insert(dir.to_path_buf());
    }
    no_package_paths.insert(path.to_path_buf());

    Ok(())
}
