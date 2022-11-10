use anyhow::Error;
use clap::Parser;
use ops::docker::build_rust_workspace::*;

fn main() -> Result<(), Error> {
    docker_build_rust_workspace(DockerBuildRustWorkspaceArgs::parse())
}
