# should only be used as a build stage for subsequent images, otherwise image sizes will be > 1Gb
FROM rust:1.65.0 AS build-rust

  # Download public key for github.com
  RUN mkdir -p -m 0700 ~/.ssh
  RUN ssh-keyscan github.com >> ~/.ssh/known_hosts

  WORKDIR /app

  RUN apt-get update
  RUN apt-get -y install jq python3-pip
  RUN pip3 install yq

  COPY rust-toolchain.toml rust-toolchain.toml
  RUN rustup update
  RUN echo '[package]\nname = "temp"\nversion = "0.0.0"\nedition = "2021"' > Cargo.toml
  RUN mkdir src && echo "fn main() {}" > src/main.rs
  RUN cargo fetch
  RUN rm -rf src

  COPY Cargo.toml Cargo.toml
  COPY Cargo.lock Cargo.lock

  # only include root-level crates to start
  RUN cat Cargo.toml | tomlq -t '. | setpath(["workspace", "members"]; ["rust_build"]) | setpath(["workspace", "exclude"]; [])' | tomlq -t '. | delpaths([["workspace", "dependencies"]])' > Cargo2.toml

  RUN cat Cargo.toml \
      | tomlq -t '.workspace.dependencies | to_entries | map(select(.value | type == "string")) | from_entries' \
      | sed 's/"dependencies/dependencies/g' \
      | sed 's/"]/]/g' \
      > simple_dependencies.toml

  RUN cat Cargo.toml \
      | tomlq -t '.workspace.dependencies | to_entries | map(select(.value | type != "string" and (.path == null or .path[0:7] == "crates/"))) | from_entries' \
      | sed 's/"dependencies/dependencies/g' \
      | sed 's/"]/]/g' \
      > complex_dependencies.toml

  RUN grep -l '^\[' complex_dependencies.toml | xargs sed -i 's/^\[/\[workspace.dependencies./g'

  RUN echo '\n[workspace.dependencies]' >> Cargo2.toml
  RUN cat simple_dependencies.toml >> Cargo2.toml
  RUN echo >> Cargo2.toml
  RUN cat complex_dependencies.toml >> Cargo2.toml
  RUN mv Cargo2.toml Cargo.toml
  RUN rm simple_dependencies.toml complex_dependencies.toml

  # Create minimal valid rust project
  RUN mkdir -p rust_build

  WORKDIR /app/rust_build

  RUN mkdir src
  RUN echo "fn main() {}" >> src/main.rs

  # Install dependencies
  RUN printf '[package] \n name = "rust_build"\nversion = "0.0.0"\nedition.workspace = true\n' > Cargo.toml
  RUN cat ../Cargo.toml | tomlq -t '.workspace.dependencies | to_entries | map(select(.value | type == "string" or .path == null)) | map(.key = "dependencies." + .key | .value = { "workspace": true }) | from_entries' | sed 's/"dependencies/dependencies/g' | sed 's/"]/]/g' >> Cargo.toml

  RUN cargo fetch

  COPY .cargo ../.cargo

  # Compile external dependencies
  RUN cargo build --release

  COPY crates ../crates

  # Compile all dependencies
  RUN printf '[package] \n name = "rust_build"\nversion = "0.0.0"\nedition.workspace = true\n' > Cargo.toml
  RUN cat ../Cargo.toml | tomlq -t '.workspace.dependencies | to_entries | map(.key = "dependencies." + .key | .value = { "workspace": true }) | from_entries' | sed 's/"dependencies/dependencies/g' | sed 's/"]/]/g' >> Cargo.toml
  RUN cargo build --release

  WORKDIR /app
  RUN rm -rf rust_build
  COPY Cargo.toml Cargo.toml
