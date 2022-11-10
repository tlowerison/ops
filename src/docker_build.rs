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

    /// push image to image repository after successful build
    #[clap(short, long)]
    pub push: Option<String>,

    /// log commands prior to running them
    #[clap(short, long)]
    pub verbose: bool,

    /// docker build args
    #[clap(value_parser)]
    pub docker_args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum DockerBuildPush {
    Aws { region: String },
}

pub fn docker_build(docker_build_args: DockerBuildArgs) -> Result<(), Error> {
    let DockerBuildArgs { docker_args, file: docker_file, file_text, ignore_file, push, verbose } = docker_build_args;

    let push = push.as_ref().map(|x| serde_urlencoded::from_str::<DockerBuildPush>(x)).transpose()?;

    let cwd = env::current_dir()?;

    let cwd = Path::new(&cwd);

    let DockerConfig { docker_file, ignore_file } = get_docker_file_and_docker_ignore_file(cwd, file_text, docker_file, ignore_file, verbose)?;
    let DockerImageName { image_name, .. } = get_docker_image_name(&docker_args)?;

    let tmpdir = tempfile::tempdir()?;
    let tmpdir = tmpdir.path();

    ctrlc::set_handler({
        let tmpdir = tmpdir.to_path_buf();
        move || delete_tmpdir(&tmpdir, verbose).unwrap()
    })?;

    let result = std::panic::catch_unwind(|| {
        if verbose {
            println!("{}", format!("created temporary directory for Dockerfile and .dockerignore at path: {}", tmpdir.display()).dimmed());
        }
        let tmp_docker_file_path = tmpdir.join("Dockerfile.tmp");
        let tmp_ignore_file_path = tmpdir.join("Dockerfile.tmp.dockerignore");

        if verbose {
            println!("{}", format!("writing to Dockerfile at path: {}", tmp_docker_file_path.display()).dimmed());
        }
        writeln!(File::create(&tmp_docker_file_path)?, "{docker_file}")?;
        if verbose {
            println!("{}", format!("writing to .dockerignore at path: {}", tmp_ignore_file_path.display()).dimmed());
        }
        writeln!(File::create(&tmp_ignore_file_path)?, "{}", ignore_file.unwrap_or_default())?;

        let cmd = "docker";
        let mut args = vec!["build"];
        args.append(&mut docker_args.iter().map(|x| &**x).collect());
        let tmp_docker_file_path_display = tmp_docker_file_path.display().to_string();
        args.append(&mut vec!["--file", &tmp_docker_file_path_display]);
        if verbose {
            println!("{}", format!("{cmd} {}", args.join(" ")).dimmed());
        }

        let output = Command::new(cmd).args(args).stdout(Stdio::inherit()).stderr(Stdio::inherit()).output()?;

        if !output.status.success() {
            return Err(Error::msg(format!("docker failed with status {}", output.status.code().unwrap())));
        }

        println!("successfully built image: {image_name}");

        if let Some(push) = push {
            match push {
                DockerBuildPush::Aws { region } => {
                    let repo = get_repo_from_image_name(image_name)?;

                    if verbose {
                        println!("{}", format!("aws ecr get-login-password --region {region} | docker login --username AWS --password-stdin {repo}").dimmed());
                    }

                    let mut aws_ecr_get_login_password = Command::new("aws")
                        .args(["ecr", "get-login-password"])
                        .args(["--region", &region])
                        .stdin(Stdio::inherit())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit())
                        .spawn()?;

                    let output = Command::new("docker")
                        .arg("login")
                        .args(["--username", "AWS"])
                        .args(["--password-stdin", repo])
                        .stdin(aws_ecr_get_login_password.stdout.take().unwrap())
                        .output()?;

                    if !output.status.success() {
                        return Err(Error::msg(format!("docker login failed with status {}", output.status.code().unwrap())));
                    }

                    if verbose {
                        println!("{}", format!("docker push {image_name}").dimmed());
                    }

                    let output =
                        Command::new("docker").args(["push", image_name]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).output()?;

                    if !output.status.success() {
                        return Err(Error::msg(format!("docker push failed with status {}", output.status.code().unwrap())));
                    }
                }
            }
        }

        Ok(())
    });

    delete_tmpdir(tmpdir, verbose)?;

    result.unwrap_or_else(|_| Err(Error::msg("an unexpected error ocurred")))
}

pub struct DockerImageName<'a> {
    pub image_name: &'a str,
    pub args_without_image_name: Vec<&'a str>,
}

pub fn get_docker_image_name(docker_args: &[String]) -> Result<DockerImageName<'_>, Error> {
    for (i, arg) in docker_args.iter().enumerate() {
        if (arg == "-t" || arg == "--tag") && docker_args.len() > i + 1 {
            return Ok(DockerImageName {
                image_name: &docker_args[i + 1],
                args_without_image_name: docker_args.iter().enumerate().filter_map(|(j, arg)| if j == i || j == i + 1 { Some(&**arg) } else { None }).collect(),
            });
        }
        if arg.len() >= 6 && &arg[..6] == "--tag=" {
            return Ok(DockerImageName {
                image_name: &docker_args[i][6..],
                args_without_image_name: docker_args.iter().enumerate().filter_map(|(j, arg)| if j == i { Some(&**arg) } else { None }).collect(),
            });
        }
    }
    Err(Error::msg("no image tag found"))
}

fn get_repo_from_image_name(image_name: &str) -> Result<&str, Error> {
    Ok(image_name.split_once('/').ok_or_else(|| Error::msg("cannot parse image repo from image name: no `/` character found in image name"))?.0)
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
        return Ok(DockerConfig { docker_file: file_text, ignore_file: ignore_file.map(read_to_string).transpose()? });
    }

    let docker_file = docker_file.unwrap_or_else(|| cwd.join("Dockerfile"));

    if !docker_file.exists() {
        return Err(Error::msg(format!("no Dockerfile found at path: {}", docker_file.display())));
    }

    let docker_file_parent = docker_file.parent().ok_or_else(|| Error::msg(format!("unable to process path to Dockerfile: {}", docker_file.display())))?;

    let docker_file_name = docker_file
        .file_name()
        .and_then(OsStr::to_str)
        .map(Path::new)
        .ok_or_else(|| Error::msg(format!("unable to process path to Dockerfile: {}", docker_file.display())))?;

    let ignore_files = match ignore_file {
        Some(x) => vec![x],
        None => {
            let docker_file_name_display = docker_file_name.display().to_string();

            let mut ignore_files = if docker_file_name_display == "Dockerfile" || docker_file_name_display == ".Dockerfile" {
                vec![]
            } else if let Some("Dockerfile") = docker_file_name.extension().and_then(OsStr::to_str) {
                let docker_file_stem = docker_file_name
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .map(Path::new)
                    .ok_or_else(|| Error::msg(format!("unable to process path to Dockerfile: {}", docker_file.display())))?
                    .display()
                    .to_string()
                    .replace(r"^\.+", "");
                vec![docker_file_parent.join(format!(".{docker_file_stem}.dockerignore")), docker_file_parent.join(format!("{docker_file_stem}.dockerignore"))]
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
        println!("{}", format!("using Dockerfile at path: {}", docker_file.display()).dimmed());
        match &ignore_file {
            Some(ignore_file) => println!("{}", format!("using .dockerignore at path: {}", ignore_file.display()).dimmed()),
            None => {
                if ignore_files.len() == 1 {
                    println!("{}", format!("no .dockerignore file found at path: {}", ignore_files[0].display()).dimmed());
                } else {
                    println!(
                        "{}",
                        format!(
                            "no .dockerignore files found at paths:{}",
                            ignore_files.iter().map(|x| x.display().to_string()).collect::<Vec<_>>().join("\n - ")
                        )
                        .dimmed()
                    );
                }
            }
        }
    }

    Ok(DockerConfig { docker_file: read_to_string(docker_file)?, ignore_file: ignore_file.map(read_to_string).transpose()? })
}

fn delete_tmpdir(tmpdir: &Path, verbose: bool) -> Result<(), Error> {
    if verbose {
        println!("{}", format!("deleting temporary build directory at path: {}", tmpdir.display()).dimmed());
    }
    Ok(())
}
