use crate::docker::build::*;
use anyhow::Error;
use clap::Parser;
use path_absolutize::*;
use std::path::{Path, PathBuf};
use std::{env, fs};
use toml::Value;

const BASE_DOCKERFILE: &str = include_str!("base.Dockerfile");
const SERVICE_DOCKERFILE: &str = include_str!("service.Dockerfile");

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about, long_about = None, trailing_var_arg=true)]
pub struct DockerBuildRustWorkspaceArgs {
    /// additional COPY commands to be included in this docker image prior to building
    #[clap(short, long)]
    pub copy: Vec<String>,

    /// whether to build the default binary: enabled if no feature sets are passed in, otherwise defaults to false
    #[clap(long)]
    pub default_feature_set: bool,

    /// comma separated set of features to use for a binary build: the build will include this binary as `{package_name}_{feature_set.join("_")}`
    #[clap(long, value_parser, action = clap::ArgAction::Append)]
    pub feature_set: Vec<String>,

    /// .dockerignore file override
    /// - defaults to a file named `.dockerignore` in the current working directory or if a Dockerfile
    ///   is specified in it looks for a `.dockerignore` in the Dockerfile's directory with a corresponding name
    /// - relative paths are relative to current working directory
    #[clap(short, long)]
    pub ignore_file: Option<PathBuf>,

    /// push image to image repository after successful build
    #[clap(short, long)]
    pub push: Option<String>,

    /// path to service to build, defaults to current working directory
    #[clap(short, long)]
    pub service: Option<PathBuf>,

    /// log commands prior to running them
    #[clap(short, long)]
    pub verbose: bool,

    /// docker build args
    #[clap(value_parser)]
    pub docker_args: Vec<String>,
}

pub fn docker_build_rust_workspace(args: DockerBuildRustWorkspaceArgs) -> Result<(), Error> {
    let DockerBuildRustWorkspaceArgs { copy, docker_args, default_feature_set, feature_set, ignore_file, push, service: provided_service_dir, verbose } = args;

    let cwd = env::current_dir()?;
    let cwd = Path::new(&cwd);

    let mut service_dir = cwd.to_path_buf();
    if let Some(mut provided_service_dir) = provided_service_dir {
        if !provided_service_dir.has_root() {
            provided_service_dir = cwd.join(provided_service_dir).absolutize()?.to_path_buf();
        }
        service_dir = provided_service_dir;
    }

    let service_manifest = fs::read_to_string(service_dir.join("Cargo.toml"))?.parse::<Value>()?;
    let service_name = service_manifest
        .get("package")
        .ok_or_else(|| Error::msg("cannot parse service level Cargo.toml: missing key `package`"))?
        .get("name")
        .ok_or_else(|| Error::msg("cannot parse service level Cargo.toml: missing key `package.name`"))?
        .as_str()
        .ok_or_else(|| Error::msg("cannot parse service level Cargo.toml: key `package.name` must be a string"))?;

    let mut feature_sets: Vec<Vec<&str>> = feature_set.iter().map(|x| x.split(',').collect()).collect();

    if feature_sets.is_empty() || default_feature_set {
        feature_sets.insert(0, vec![]);
    }

    env::set_current_dir(get_workspace_dir(&service_dir)?)?;

    let service_dockerfile = get_service_dockerfile(service_name, &feature_sets, &copy)?;

    let DockerImageName { mut args_without_image_name, .. } = get_docker_image_name(&docker_args)?;

    let mut base_docker_args = vec!["."];
    base_docker_args.append(&mut args_without_image_name);
    base_docker_args.append(&mut vec!["--tag", "build-rust"]);

    docker_build(DockerBuildArgs {
        docker_args: base_docker_args.into_iter().map(String::from).collect(),
        file: None,
        file_text: Some(BASE_DOCKERFILE.to_string()),
        push: None,
        ignore_file: ignore_file.clone(),
        verbose,
    })?;

    docker_build(DockerBuildArgs { file: None, file_text: Some(service_dockerfile), docker_args, ignore_file, push, verbose })?;

    Ok(())
}

fn get_features_flag(feature_set: &[&str]) -> String {
    if feature_set.is_empty() {
        "".into()
    } else {
        format!("--features={}", feature_set.join(","))
    }
}

fn get_service_dockerfile(service_name: &str, feature_sets: &[Vec<&str>], copy: &[String]) -> Result<String, Error> {
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

    let mut service_docker_pre_builds =
        feature_sets.iter().map(|feature_set| format!("  RUN cargo build  --release {}", get_features_flag(feature_set))).collect::<Vec<_>>();
    service_docker_pre_builds.insert(0, "  RUN cargo build  --release".to_string());
    service_docker_pre_builds.push(format!("  RUN rm /app/target/release/rust_build && rm /app/target/release/{service_name}"));

    let service_dockerfile = service_dockerfile.replace("$pre_build", service_docker_pre_builds.join("\n").trim());

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

    let service_dockerfile = service_dockerfile.replace("$build", service_docker_build_binaries.join("\n").trim());

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
    service_docker_copy_binaries.push(format!("  COPY --from=build-{service_name} /app/target/release/{service_name} /app/{service_name}"));

    let service_dockerfile = service_dockerfile.replace("$binary_copy", service_docker_copy_binaries.join("\n").trim());

    Ok(service_dockerfile)
}

fn get_workspace_dir(service_dir: &Path) -> Result<&Path, Error> {
    let mut dir = service_dir;
    Ok(loop {
        dir = dir.parent().ok_or_else(|| Error::msg("unable to locate cargo workspace root"))?;
        let manifest_path = dir.join("Cargo.toml");
        if manifest_path.exists() {
            let manifest = fs::read_to_string(manifest_path)?.parse::<Value>()?;
            if manifest.get("workspace").is_some() {
                break dir;
            }
        }
    })
}
