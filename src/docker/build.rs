use anyhow::Error;
use clap::Parser;
use colored::Colorize;
use std::fs::{read_to_string, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, ffi::OsStr, io::Write};

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about, long_about = None, trailing_var_arg=true)]
pub struct DockerBuildArgs {
    /// Dockerfile path
    /// - defaults to a file named `Dockerfile` in the current working directory
    /// - relative paths are relative to current working directory
    #[clap(short, long)]
    pub file: Option<PathBuf>,

    /// Dockerfile contents, overrides any --file flag passed in
    #[clap(long)]
    pub file_text: Option<String>,

    /// .dockerignore file override
    /// - defaults to a file named `.dockerignore` in the current working directory or if a Dockerfile
    ///   is specified in it looks for a `.dockerignore` in the Dockerfile's directory with a corresponding name
    /// - relative paths are relative to current working directory
    ///
    /// Ex: `ops-docker-build --file path/to/Dockerfile -- ...` will look for dockerignore files in the following order:
    /// - path/to/.dockerignore
    ///
    /// Ex: `ops-docker-build --file path/to/foobar.txt -- ...` will look for dockerignore files in the following order:
    /// - path/to/.dockerignore
    ///
    /// Ex: `ops-docker-build --file path/to/foo.Dockerfile -- ...` will look for dockerignore files in the following order:
    /// - path/to/.foo.dockerignore
    /// - path/to/foo.dockerignore
    /// - path/to/.dockerignore
    ///
    /// Ex: `ops-docker-build --file path/to/.foo.Dockerfile -- ...` will look for dockerignore files in the following order:
    /// - path/to/.foo.dockerignore
    /// - path/to/foo.dockerignore
    /// - path/to/.dockerignore
    ///
    /// Ex: `ops-docker-build --file path/to/Dockerfile.foo -- ...` will look for dockerignore files in the following order:
    /// - path/to/.Dockerfile.foo.dockerignore
    /// - path/to/Dockerfile.foo.dockerignore
    /// - path/to/.dockerignore
    ///
    /// Ex: `ops-docker-build --file path/to/.Dockerfile.foo -- ...` will look for dockerignore files in the following order:
    /// - path/to/.Dockerfile.foo.dockerignore
    /// - path/to/Dockerfile.foo.dockerignore
    /// - path/to/.dockerignore
    #[clap(short, long)]
    pub ignore_file: Option<PathBuf>,

    /// log commands prior to running them
    #[clap(short, long)]
    pub verbose: bool,

    /// docker build args
    #[clap(value_parser)]
    pub docker_args: Vec<String>,
}

pub fn docker_build(docker_build_args: DockerBuildArgs) -> Result<(), Error> {
    let DockerBuildArgs {
        docker_args,
        file: docker_file,
        file_text,
        ignore_file,
        verbose,
    } = docker_build_args;

    let cwd = env::current_dir()?;

    let cwd = Path::new(&cwd);

    let DockerConfig {
        docker_file,
        ignore_file,
    } = get_docker_file_and_docker_ignore_file(cwd, file_text, docker_file, ignore_file, verbose)?;

    // NOTE: tmp_dir and all of its contents are deleted on drop, only need
    let tmp_dir = tempfile::tempdir()?;
    let tmp_dir = tmp_dir.path();

    if verbose {
        println!(
            "{}",
            format!(
                "created temporary directory for Dockerfile and .dockerignore at path: {}",
                tmp_dir.display()
            )
            .dimmed()
        );
    }
    let tmp_docker_file_path = tmp_dir.join("Dockerfile.tmp");
    let tmp_ignore_file_path = tmp_dir.join("Dockerfile.tmp.dockerignore");

    if verbose {
        println!(
            "{}",
            format!("creating Dockerfile at: {}", tmp_docker_file_path.display()).dimmed()
        );
    }
    let mut docker_file_file = File::create(&tmp_docker_file_path)?;
    if verbose {
        println!("{}", "created Dockerfile successfully".to_string().dimmed());
    }

    if verbose {
        println!(
            "{}",
            format!("writing to Dockerfile at path: {}", tmp_docker_file_path.display()).dimmed()
        );
    }
    writeln!(docker_file_file, "{docker_file}")?;

    if verbose {
        println!(
            "{}",
            format!("creating ignore file at: {}", tmp_ignore_file_path.display()).dimmed()
        );
    }
    let mut ignore_file_file = File::create(&tmp_ignore_file_path)?;
    if verbose {
        println!("{}", "created ignore file successfully".to_string().dimmed());
    }

    if verbose {
        println!("{}", "writing to ignore file".to_string().dimmed());
    }
    writeln!(ignore_file_file, "{}", ignore_file.unwrap_or_default())?;

    let cmd = "docker";
    let mut args = vec!["build"];
    args.append(&mut docker_args.iter().map(|x| &**x).collect());
    let tmp_docker_file_path_display = tmp_docker_file_path.display().to_string();
    args.append(&mut vec!["--file", &tmp_docker_file_path_display]);
    if verbose {
        println!("{}", format!("{cmd} {}", args.join(" ")).dimmed());
        let docker_file = read_to_string(tmp_docker_file_path)?;
        println!("{}", docker_file.dimmed());
    }

    let output = Command::new(cmd)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()?;

    if !output.status.success() {
        return Err(Error::msg(format!(
            "docker failed with status {}",
            output.status.code().unwrap()
        )));
    }

    println!("successfully built image");

    Ok(())
}

pub struct SplitDockerArgs<'a> {
    pub tag: &'a str,
    pub other: Vec<&'a str>,
}

pub fn split_docker_args(docker_args: &[String]) -> Result<SplitDockerArgs<'_>, Error> {
    let mut tag_index = None;
    let mut tag_slice_index = 0;
    let mut omit_indices = std::collections::HashSet::new();
    for (i, arg) in docker_args.iter().enumerate() {
        if (arg == "-t" || arg == "--tag") && docker_args.len() > i + 1 {
            omit_indices.insert(i);
            omit_indices.insert(i + 1);
            tag_index = Some(i + 1);
        }
        if arg.len() >= 6 && &arg[..6] == "--tag=" {
            omit_indices.insert(i);
            tag_index = Some(i);
            tag_slice_index = 6;
        }
    }
    let tag_index = tag_index.ok_or_else(|| Error::msg("no image tag provided"))?;
    Ok(SplitDockerArgs {
        tag: &docker_args[tag_index][tag_slice_index..],
        other: docker_args
            .iter()
            .enumerate()
            .filter_map(|(i, arg)| if omit_indices.contains(&i) { None } else { Some(&**arg) })
            .collect(),
    })
}

#[derive(Clone, Debug)]
struct DockerConfig {
    docker_file: String,
    ignore_file: Option<String>,
}

fn get_docker_file_and_docker_ignore_file(
    cwd: &Path,
    file_text: Option<String>,
    docker_file: Option<PathBuf>,
    ignore_file: Option<PathBuf>,
    verbose: bool,
) -> Result<DockerConfig, Error> {
    if let Some(file_text) = file_text {
        return Ok(DockerConfig {
            docker_file: file_text,
            ignore_file: ignore_file.map(read_to_string).transpose()?,
        });
    }

    let docker_file = docker_file.unwrap_or_else(|| cwd.join("Dockerfile"));

    if !docker_file.exists() {
        return Err(Error::msg(format!(
            "no Dockerfile found at path: {}",
            docker_file.display()
        )));
    }

    let docker_file_parent = docker_file.parent().ok_or_else(|| {
        Error::msg(format!(
            "unable to process path to Dockerfile: {}",
            docker_file.display()
        ))
    })?;

    let docker_file_name = docker_file
        .file_name()
        .and_then(OsStr::to_str)
        .map(Path::new)
        .ok_or_else(|| {
            Error::msg(format!(
                "unable to process path to Dockerfile: {}",
                docker_file.display()
            ))
        })?;

    let ignore_files = match ignore_file {
        Some(x) => vec![x],
        None => {
            let docker_file_name_display = docker_file_name.display().to_string();

            let mut ignore_files =
                if docker_file_name_display == "Dockerfile" || docker_file_name_display == ".Dockerfile" {
                    vec![]
                } else if let Some("Dockerfile") = docker_file_name.extension().and_then(OsStr::to_str) {
                    let docker_file_stem = docker_file_name
                        .file_stem()
                        .and_then(OsStr::to_str)
                        .map(Path::new)
                        .ok_or_else(|| {
                            Error::msg(format!(
                                "unable to process path to Dockerfile: {}",
                                docker_file.display()
                            ))
                        })?
                        .display()
                        .to_string()
                        .replace(r"^\.+", "");
                    vec![
                        docker_file_parent.join(format!(".{docker_file_stem}.dockerignore")),
                        docker_file_parent.join(format!("{docker_file_stem}.dockerignore")),
                    ]
                } else if docker_file_name_display.len() > 11 && &docker_file_name_display[..11] == "Dockerfile." {
                    let docker_file_suffix = &docker_file_name_display[11..];
                    vec![
                        docker_file_parent.join(format!(".Dockerfile.{docker_file_suffix}.dockerignore")),
                        docker_file_parent.join(format!("Dockerfile.{docker_file_suffix}.dockerignore")),
                    ]
                } else if docker_file_name_display.len() > 12 && &docker_file_name_display[..12] == ".Dockerfile." {
                    let docker_file_suffix = &docker_file_name_display[12..];
                    vec![
                        docker_file_parent.join(format!(".Dockerfile.{docker_file_suffix}.dockerignore")),
                        docker_file_parent.join(format!("Dockerfile.{docker_file_suffix}.dockerignore")),
                    ]
                } else {
                    vec![]
                };

            ignore_files.push(docker_file_parent.join(".dockerignore"));
            ignore_files
        }
    };

    let mut ignore_file = None;
    for path_buf in ignore_files.iter() {
        if path_buf.exists() {
            ignore_file = Some(path_buf);
            break;
        }
    }

    if verbose {
        println!(
            "{}",
            format!("using Dockerfile at path: {}", docker_file.display()).dimmed()
        );
        match &ignore_file {
            Some(ignore_file) => println!(
                "{}",
                format!("using .dockerignore at path: {}", ignore_file.display()).dimmed()
            ),
            None => {
                if ignore_files.len() == 1 {
                    println!(
                        "{}",
                        format!("no .dockerignore file found at path: {}", ignore_files[0].display()).dimmed()
                    );
                } else {
                    println!(
                        "{}",
                        format!(
                            "no .dockerignore files found at paths:{}",
                            ignore_files
                                .iter()
                                .map(|x| x.display().to_string())
                                .collect::<Vec<_>>()
                                .join("\n - ")
                        )
                        .dimmed()
                    );
                }
            }
        }
    }

    Ok(DockerConfig {
        docker_file: read_to_string(docker_file)?,
        ignore_file: ignore_file.map(read_to_string).transpose()?,
    })
}

pub fn get_registry_from_tag(tag: &str) -> Result<&str, Error> {
    let registry = tag
        .split_once('/')
        .ok_or_else(|| Error::msg("cannot parse image registry from image tag: no `/` character found"))?
        .0;
    Ok(registry)
}

pub fn get_repository_from_tag(tag: &str) -> Result<&str, Error> {
    let non_registry = tag
        .split_once('/')
        .ok_or_else(|| Error::msg("cannot parse image repository from image tag: no `/` character found"))?
        .1;
    let repository = non_registry
        .split_once(':')
        .ok_or_else(|| Error::msg("cannot parse image repository from image tag: no `:` character found"))?
        .0;
    Ok(repository)
}
