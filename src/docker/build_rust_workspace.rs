use crate::docker::build::*;
use anyhow::Error;
use clap::Parser;
use path_absolutize::*;
use std::path::{Path, PathBuf};
use std::{env, fs};
use toml::Value;

const FETCH_DOCKERFILE: &str = include_str!("fetch.Dockerfile");
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

    /// service dependencies to omit during pre-build (e.g. if one service depends on another, you should omit the service dependency during
    /// pre-build if both are likely to change frequently)
    #[clap(short, long, value_parser, action = clap::ArgAction::Append)]
    pub pre_build_omit: Vec<String>,

    /// which rust profile to build rust binaries -- as opposed to cargo, debug and release
    /// can be specified through this flag in addition to any profiles listed in a manifest
    /// defaults to release
    #[clap(long)]
    pub profile: Option<String>,

    /// push image to image repository after successful build
    #[clap(long)]
    pub push: Option<String>,

    /// rust docker image version -- actual rust version used in built binaries should be
    /// specified with a workspace level rust-toolchain.toml file -- defaults to latest
    #[clap(short, long)]
    pub rust_version: Option<String>,

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
    let DockerBuildRustWorkspaceArgs {
        copy,
        docker_args,
        default_feature_set,
        feature_set,
        ignore_file,
        pre_build_omit,
        profile,
        push,
        rust_version,
        service: provided_service_dir,
        verbose,
    } = args;

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
        // important to push the default binary to the back so that as we build each binary,
        // we can rename them with their features and the first binary isn't replaced (would
        // be if it is the default binary since it doesn't receive a rename)
        feature_sets.push(vec![]);
    }

    let workspace_dir = get_workspace_dir(&service_dir)?;
    env::set_current_dir(workspace_dir)?;

    let profile = profile.unwrap_or_else(|| "release".to_string());

    let fetch_dockerfile = get_fetch_dockerfile(workspace_dir, &rust_version)?;

    let base_dockerfile = get_base_dockerfile(&profile);

    let service_dockerfile = get_service_dockerfile(service_name, &profile, &feature_sets, &copy, &pre_build_omit)?;

    let DockerImageName { mut args_without_image_name, .. } = get_docker_image_name(&docker_args)?;

    let mut fetch_docker_args = vec![];
    fetch_docker_args.append(&mut args_without_image_name.clone());
    fetch_docker_args.append(&mut vec!["--tag", "fetch-rust"]);

    docker_build(DockerBuildArgs {
        docker_args: fetch_docker_args.into_iter().map(String::from).collect(),
        file: None,
        file_text: Some(fetch_dockerfile),
        push: None,
        ignore_file: ignore_file.clone(),
        verbose,
    })?;

    let base_tag = format!("build-rust-{profile}");
    let mut base_docker_args = vec![];
    base_docker_args.append(&mut args_without_image_name);
    base_docker_args.append(&mut vec!["--tag", &base_tag]);

    let build_profile_arg = if profile == "debug" {
        "build_profile=".to_string()
    } else if profile == "release" {
        "build_profile=--release".to_string()
    } else {
        format!("build_profile=--profile={profile}")
    };
    base_docker_args.append(&mut vec!["--build-arg", &build_profile_arg]);

    docker_build(DockerBuildArgs {
        docker_args: base_docker_args.into_iter().map(String::from).collect(),
        file: None,
        file_text: Some(base_dockerfile),
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
        format!(" --features={}", feature_set.join(","))
    }
}

fn get_fetch_dockerfile(workspace_dir: &Path, rust_version: &Option<String>) -> Result<String, Error> {
    let rust_toolchain_path = workspace_dir.join("rust-toolchain.toml");

    let rustup_toolchain_override = "COPY rust-toolchain.toml rust-toolchain.toml\n  RUN cat rust-toolchain.toml | tomlq -t '.toolchain.profile = \"minimal\"' > rust-toolchain2.toml && mv rust-toolchain2.toml rust-toolchain.toml";
    let rustup_update = "RUN rustup update";
    let rustup_toolchain = rust_toolchain_path
        .try_exists()
        .ok()
        .and_then(|exists| {
            if exists {
                Some(format!("{rustup_toolchain_override}\n  {rustup_update}"))
            } else {
                None
            }
        })
        .unwrap_or_else(|| rustup_update.to_string());

    let full_cargo_lock = fs::read_to_string(workspace_dir.join("Cargo.lock"))?.parse::<Value>()?;

    let mut full_cargo_lock = match full_cargo_lock {
        Value::Table(table) => table,
        _ => return Err(Error::msg("unable to parse Cargo.lock: file is not a toml table")),
    };

    let cargo_lock_package = full_cargo_lock.remove("package").ok_or_else(|| Error::msg("unable to parse Cargo.lock: no package field found"))?;

    let packages = match cargo_lock_package {
        Value::Array(packages) => packages,
        _ => return Err(Error::msg("unable to parse Cargo.lock: no package field is not an array")),
    };
    let packages = packages
        .into_iter()
        .filter(|package| match package {
            Value::Table(table) => table.contains_key("source") && table.contains_key("checksum"),
            _ => true,
        })
        .collect();

    let fetch_cargo_lock_toml = Value::Table(toml::value::Map::from_iter([("package".to_string(), Value::Array(packages))]));
    let fetch_cargo_lock_toml = toml::ser::to_string(&fetch_cargo_lock_toml)?;

    let fetch_dockerfile = FETCH_DOCKERFILE
        .replace("$rust_version", rust_version.as_ref().map(|x| &**x).unwrap_or_else(|| "latest"))
        .replace("$rustup_toolchain", &rustup_toolchain)
        .replace("$fetch_cargo_lock", &format!("RUN echo '{}' > Cargo.lock", fetch_cargo_lock_toml).replace('\n', "\\n\\\n"));

    Ok(fetch_dockerfile)
}

fn get_base_dockerfile(profile: &str) -> String {
    BASE_DOCKERFILE.replace("$profile", profile)
}

fn get_service_dockerfile(service_name: &str, profile: &str, feature_sets: &[Vec<&str>], copy: &[String], pre_build_omit: &[String]) -> Result<String, Error> {
    let service_dockerfile =
        SERVICE_DOCKERFILE.replace("$service", service_name).replace("$profile", profile).replace("$base_image", &format!("build-rust-{profile}"));

    let mut additional_copies = copy
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

    if !additional_copies.is_empty() {
        additional_copies = format!("\n\n{additional_copies}");
    }

    let service_dockerfile = service_dockerfile.replace("$file_copy", &additional_copies);

    let pre_build_omit_deps = format!(r#"'{{{}}}'"#, pre_build_omit.iter().map(|x| format!(r#""{x}": true"#)).collect::<Vec<_>>().join(","));
    let service_dockerfile = service_dockerfile.replace("$pre_build_omit_deps", &pre_build_omit_deps);

    let build_profile = if profile == "debug" {
        "".to_string()
    } else if profile == "release" {
        " --release".to_string()
    } else {
        format!(" --profile={profile}")
    };

    let mut service_docker_pre_builds =
        feature_sets.iter().map(|feature_set| format!("  RUN cargo build{build_profile}{}", get_features_flag(feature_set))).collect::<Vec<_>>();
    service_docker_pre_builds.push(format!("  RUN rm /app/target/{profile}/rust_build && rm /app/target/{profile}/{service_name}"));

    let service_dockerfile = service_dockerfile.replace("$pre_build", service_docker_pre_builds.join("\n").trim());

    let service_docker_build_binaries = feature_sets
        .iter()
        .map(|feature_set| {
            let features_flag = get_features_flag(feature_set);
            let build_cmd = format!("  RUN cargo build{build_profile}{features_flag}");
            if feature_set.is_empty() {
                return build_cmd;
            }
            let feature_set = feature_set.iter().map(|x| format!("_{x}")).collect::<Vec<_>>().join("");
            format!("{build_cmd}\n  RUN mv /app/target/{profile}/{service_name} /app/target/{profile}/{service_name}{feature_set}")
        })
        .collect::<Vec<_>>();

    let service_dockerfile = service_dockerfile.replace("$build", service_docker_build_binaries.join("\n").trim());

    let service_docker_copy_binaries = feature_sets
        .iter()
        .map(|feature_set| {
            let feature_set = feature_set.iter().map(|x| format!("_{x}")).collect::<Vec<_>>().join("");
            format!("  COPY --from=build-{service_name}-{profile} /app/target/{profile}/{service_name}{feature_set} /app/{service_name}{feature_set}")
        })
        .collect::<Vec<_>>();

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
