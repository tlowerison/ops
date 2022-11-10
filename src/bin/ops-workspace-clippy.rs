use anyhow::Error;
use clap::Parser;
use ops::workspace_clippy::*;

fn main() -> Result<(), Error> {
    workspace_clippy(WorkspaceClippyArgs::parse())
}
