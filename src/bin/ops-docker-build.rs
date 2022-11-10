use clap::Parser;
use ops::docker_build::*;

fn main() -> Result<(), anyhow::Error> {
    docker_build(DockerBuildArgs::parse())
}
