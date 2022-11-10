use anyhow::Error;
use clap::Parser;
use ops::docker::build::*;

fn main() -> Result<(), Error> {
    docker_build(DockerBuildArgs::parse())
}
