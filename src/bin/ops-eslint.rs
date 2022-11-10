use anyhow::Error;
use clap::Parser;
use ops::eslint::*;

fn main() -> Result<(), Error> {
    eslint(EslintArgs::parse())
}
