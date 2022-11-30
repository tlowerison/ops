# should only be used as a build stage for subsequent images, otherwise image sizes will be > 1Gb
FROM fetch-rust AS build-rust

  ARG build_profile

  COPY .cargo ../.cargo

  # Compile external dependencies
  RUN cargo build $build_profile

  COPY crates ../crates

  # Compile all dependencies
  RUN printf '[package] \n name = "rust_build"\nversion = "0.0.0"\nedition.workspace = true\n' > Cargo.toml
  RUN cat ../Cargo.toml | tomlq -t '.workspace.dependencies | to_entries | map(.key = "dependencies." + .key | .value = { "workspace": true }) | from_entries' | sed 's/"dependencies/dependencies/g' | sed 's/"]/]/g' >> Cargo.toml
  RUN cargo build $build_profile

  WORKDIR /app
  RUN rm -rf rust_build
  COPY Cargo.toml Cargo.toml

  # neccessary to replace the formerly edited Cargo.lock used in the fetch and base build steps
  COPY Cargo.lock Cargo.lock
