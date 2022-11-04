#[macro_use]
extern crate serde;

use anyhow::Error;
use clap::Parser;
use colored::Colorize;
use path_absolutize::*;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::{collections::HashMap, env, fs, io::Write, path::Path};
use toml::{map::Map, Value};
use walkdir::{DirEntry, WalkDir};

const BASE_DOCKERFILE: &str = include_str!("base.Dockerfile");
const SERVICE_DOCKERFILE: &str = include_str!("service.Dockerfile");

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about, long_about = None, trailing_var_arg=true)]
struct Args {
    /// additional COPY commands to be included in this docker image prior to building
    #[clap(short, long)]
    copy: Vec<String>,

    #[clap(long)]
    /// whether to build the default binary: enabled if no feature sets are passed in, otherwise defaults to false
    empty_feature_set: bool,

    #[clap(long, value_parser, action = clap::ArgAction::Append)]
    /// comma separated set of features to use for a binary build: the build will include this binary as `{package_name}_{feature_set.join("_")}`
    feature_set: Vec<String>,

    /// push image to image repository after successful build
    #[clap(short, long)]
    push: Option<String>,

    /// docker image repo
    #[clap(short, long)]
    repo: String,

    /// docker image tag
    #[clap(short, long)]
    tag: Option<String>,

    #[clap(short, long)]
    verbose: bool,

    #[clap(short, long)]
    workspace_dir: PathBuf,

    /// docker build args
    #[clap(value_parser)]
    docker_args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
enum Push {
    Aws { region: String },
}

fn main() -> Result<(), Error> {
    let Args {
        copy,
        docker_args,
        empty_feature_set,
        feature_set,
        push,
        repo,
        tag,
        verbose,
        workspace_dir: provided_workspace_dir,
    } = Args::parse();

    let push = push
        .as_ref()
        .map(|x| serde_urlencoded::from_str::<Push>(x))
        .transpose()?;

    let cwd = env::current_dir()?;

    let cwd = Path::new(&cwd);

    let mut workspace_dir = provided_workspace_dir;
    if !workspace_dir.has_root() {
        workspace_dir = cwd.join(workspace_dir).absolutize()?.to_path_buf();
    }

    if !cwd.starts_with(&workspace_dir) {
        return Err(Error::msg(
            "current directory is not contained within the workspace directory",
        ));
    }

    let service_dir = cwd.strip_prefix(&workspace_dir)?;

    let service_cargo = fs::read_to_string("Cargo.toml")?.parse::<Value>()?;
    let service_name = service_cargo
        .get("package")
        .ok_or_else(|| Error::msg("cannot parse service level Cargo.toml: missing key `package`"))?
        .get("name")
        .ok_or_else(|| {
            Error::msg("cannot parse service level Cargo.toml: missing key `package.name`")
        })?
        .as_str()
        .ok_or_else(|| {
            Error::msg("cannot parse service level Cargo.toml: key `package.name` must be a string")
        })?;

    let mut feature_sets: Vec<Vec<&str>> =
        feature_set.iter().map(|x| x.split(',').collect()).collect();

    if feature_sets.is_empty() || empty_feature_set {
        feature_sets.insert(0, vec![]);
    }

    let DockerArgs {
        image_name,
        args: docker_args,
        build_rust_args: build_rust_docker_args,
    } = process_docker_args(docker_args, service_name, &repo, tag)?;

    let workspace_deps = get_deps(&workspace_dir, Dependencies::Workspace)?;

    let mut package_local_deps: HashMap<String, String> = Default::default();
    get_dep_paths(
        &workspace_dir,
        &service_dir.display().to_string(),
        &workspace_deps,
        &mut package_local_deps,
    )?;

    fs::create_dir_all(workspace_dir.join("tmp"))?;
    let mut tar_builder = tar::Builder::new(
        fs::File::create(
            workspace_dir
                .join("tmp")
                .join("crate_dependencies.build.tar"),
        )
        .unwrap(),
    );

    for path in package_local_deps.values() {
        let entries = WalkDir::new(workspace_dir.join(path))
            .into_iter()
            .filter_entry(|e: &DirEntry| {
                let file_name = e.file_name().to_str();
                if file_name.is_none() {
                    return false;
                }
                let file_name = file_name.unwrap();
                file_name != "node_modules"
                    && file_name != "target"
                    && (e.file_type().is_dir()
                        || &file_name[file_name.len() - 2..] == "rs"
                        || file_name == "Cargo.toml")
            });

        for entry in entries {
            let entry = entry.unwrap();
            if entry.file_type().is_file() {
                let mut f = fs::File::open(entry.path())?;
                tar_builder.append_file(entry.path().strip_prefix(&workspace_dir)?, &mut f)?;
            }
        }
    }

    env::set_current_dir(&workspace_dir)?;

    let service_dockerfile = get_service_dockerfile(service_name, &feature_sets, &copy)?;

    let cmd = "docker";
    let mut args = vec!["build", ".", "-t", "build-rust"];
    args.append(&mut build_rust_docker_args.iter().map(|x| &**x).collect());
    args.append(&mut vec!["-f", "-"]);

    if verbose {
        println!("{}", format!("{cmd} {}", args.join(" ")).dimmed());
        println!("{}", BASE_DOCKERFILE.dimmed());
    }

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::msg("could not take child process stdin"))?;
    std::thread::spawn(move || stdin.write_all(BASE_DOCKERFILE.as_bytes()))
        .join()
        .map_err(|_| Error::msg("thread error"))??;

    let output = child.wait_with_output()?;

    if !output.status.success() {
        return Err(Error::msg(format!(
            "docker failed with status {}",
            output.status.code().unwrap()
        )));
    }

    let cmd = "docker";
    let mut args = vec!["build", "."];
    args.append(&mut docker_args.iter().map(|x| &**x).collect());
    args.append(&mut vec!["-f", "-"]);
    if verbose {
        println!("{}", format!("{cmd} {}", args.join(" ")).dimmed());
        println!("{}", service_dockerfile.dimmed());
    }

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::msg("could not take child process stdin"))?;
    std::thread::spawn(move || stdin.write_all(service_dockerfile.as_bytes()))
        .join()
        .map_err(|_| Error::msg("thread error"))??;

    let output = child.wait_with_output()?;

    if !output.status.success() {
        return Err(Error::msg(format!(
            "docker failed with status {}",
            output.status.code().unwrap()
        )));
    }

    println!(
        "successfully built image{}",
        image_name
            .as_ref()
            .map(|x| format!(": {x}"))
            .unwrap_or_default()
    );

    if let (Some(push), Some(image_name)) = (push, image_name.as_ref()) {
        match push {
            Push::Aws { region } => {
                if verbose {
                    println!(
                        "{}",
                        format!("aws ecr get-login-password --region {region} | docker login --username AWS --password-stdin {repo}").dimmed()
                    );
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
                    .args(["--password-stdin", &repo])
                    .stdin(aws_ecr_get_login_password.stdout.take().unwrap())
                    .output()?;

                if !output.status.success() {
                    return Err(Error::msg(format!(
                        "docker login failed with status {}",
                        output.status.code().unwrap()
                    )));
                }

                if verbose {
                    println!("{}", format!("docker push {image_name}").dimmed());
                }

                let output = Command::new("docker")
                    .args(["push", image_name])
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .output()?;

                if !output.status.success() {
                    return Err(Error::msg(format!(
                        "docker push failed with status {}",
                        output.status.code().unwrap()
                    )));
                }
            }
        }
    }

    Ok(())
}

enum Dependencies {
    Package,
    Workspace,
}

fn get_deps(service_dir: &Path, deps: Dependencies) -> Result<Map<String, Value>, Error> {
    let file_path = service_dir.join("Cargo.toml");
    let value = fs::read_to_string(&file_path)?.parse::<Value>()?;
    let mut table = match value {
        Value::Table(mut table) => match deps {
            Dependencies::Package => table,
            Dependencies::Workspace => {
                match table.remove("workspace").ok_or_else(|| {
                    Error::msg(format!(
                        "missing `workspace` key in {}",
                        file_path.display()
                    ))
                })? {
                    Value::Table(table) => table,
                    _ => panic!(),
                }
            }
        },
        _ => panic!(),
    };
    Ok(
        match table
            .remove("dependencies")
            .unwrap_or_else(|| Value::Table(Map::default()))
        {
            Value::Table(deps) => deps,
            _ => panic!(),
        },
    )
}

fn get_dep_paths(
    workspace_dir: &Path,
    service_dir: &str,
    workspace_deps: &Map<String, Value>,
    package_local_deps: &mut HashMap<String, String>,
) -> Result<(), Error> {
    let package_deps = get_deps(&workspace_dir.join(service_dir), Dependencies::Package)?;

    for package in package_deps.keys() {
        if let Some(Value::Table(data)) = workspace_deps.get(package) {
            if let Some(Value::String(path)) = data.get("path") {
                if package_deps.contains_key(package) {
                    if !package_local_deps.contains_key(path) {
                        package_local_deps.insert(package.clone(), path.clone());
                        get_dep_paths(workspace_dir, path, workspace_deps, package_local_deps)?;
                    } else {
                        package_local_deps.insert(package.clone(), path.clone());
                    }
                }
            }
        }
    }

    Ok(())
}

fn get_features_flag(feature_set: &[&str]) -> String {
    if feature_set.is_empty() {
        "".into()
    } else {
        format!("--features={}", feature_set.join(","))
    }
}

fn get_service_dockerfile(
    service_name: &str,
    feature_sets: &[Vec<&str>],
    copy: &[String],
) -> Result<String, Error> {
    let service_dockerfile = SERVICE_DOCKERFILE.replace("$service", service_name);

    let additional_copies = copy
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let line = line.trim();
            if i == 0 {
                line.to_string()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let service_dockerfile = service_dockerfile.replace("$file_copy", &additional_copies);

    let mut service_docker_pre_builds = feature_sets
        .iter()
        .map(|feature_set| {
            format!(
                "  RUN cargo build  --release {}",
                get_features_flag(feature_set)
            )
        })
        .collect::<Vec<_>>();
    service_docker_pre_builds.insert(0, "  RUN cargo build  --release".to_string());
    service_docker_pre_builds.push(format!(
        "  RUN rm /app/target/release/rust_build && rm /app/target/release/{service_name}"
    ));

    let service_dockerfile =
        service_dockerfile.replace("$pre_build", service_docker_pre_builds.join("\n").trim());

    let mut service_docker_build_binaries = feature_sets
        .iter()
        .map(|feature_set| {
            let features_flag = get_features_flag(feature_set);
            format!(
                "  RUN cargo build --release {features_flag}\n  RUN mv /app/target/release/{service_name} /app/target/release/{service_name}_{}",
                feature_set.join("_"),
            )
        })
        .collect::<Vec<_>>();
    service_docker_build_binaries.push("  RUN cargo build --release".to_string());

    let service_dockerfile =
        service_dockerfile.replace("$build", service_docker_build_binaries.join("\n").trim());

    let mut service_docker_copy_binaries = feature_sets
        .iter()
        .map(|feature_set| {
            format!(
                "  COPY --from=build-{service_name} /app/target/release/{service_name}_{} /app/{service_name}_{}",
                feature_set.join("_"),
                feature_set.join("_"),
            )
        })
        .collect::<Vec<_>>();
    service_docker_copy_binaries.push(format!(
        "  COPY --from=build-{service_name} /app/target/release/{service_name} /app/{service_name}"
    ));

    let service_dockerfile = service_dockerfile.replace(
        "$binary_copy",
        service_docker_copy_binaries.join("\n").trim(),
    );

    Ok(service_dockerfile)
}

struct DockerArgs {
    image_name: Option<String>,
    args: Vec<String>,
    build_rust_args: Vec<String>,
}

fn process_docker_args(
    docker_args: Vec<String>,
    service_name: &str,
    repo: &str,
    tag: Option<String>,
) -> Result<DockerArgs, Error> {
    let mut build_rust_docker_args = vec![];
    let mut tag_arg_index = None;
    for (i, arg) in docker_args.clone().into_iter().enumerate() {
        if arg == "-t" || arg == "--tag" {
            tag_arg_index = Some(i);
            continue;
        }
        if let Some(tag_arg_index) = tag_arg_index.as_ref() {
            if i == tag_arg_index + 1 {
                continue;
            }
        }
        build_rust_docker_args.push(arg);
    }

    if let Some(tag) = tag {
        let mut new_docker_args = vec![];
        let mut tag_arg_index = None;
        for (i, arg) in docker_args.into_iter().enumerate() {
            if arg == "-t" || arg == "--tag" {
                tag_arg_index = Some(i);
                continue;
            }
            if let Some(tag_arg_index) = tag_arg_index.as_ref() {
                if i == tag_arg_index + 1 {
                    continue;
                }
            }
            new_docker_args.push(arg);
        }

        let image_name = format!("{repo}/{service_name}:{tag}");
        new_docker_args.push("--tag".to_string());
        new_docker_args.push(image_name.clone());

        Ok(DockerArgs {
            image_name: Some(image_name),
            args: new_docker_args,
            build_rust_args: build_rust_docker_args,
        })
    } else {
        let mut image_name = None;
        let mut tag_arg_index = None;
        for (i, arg) in docker_args.iter().enumerate() {
            if arg == "-t" || arg == "--tag" {
                tag_arg_index = Some(i);
                continue;
            }
            if let Some(tag_arg_index) = tag_arg_index.as_ref() {
                if i == tag_arg_index + 1 {
                    image_name = Some(arg.clone());
                }
            }
        }

        Ok(DockerArgs {
            args: docker_args,
            build_rust_args: build_rust_docker_args,
            image_name,
        })
    }
}
