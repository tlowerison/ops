FROM $base_image as build-$service
  WORKDIR /app

  RUN ls -a .
  RUN cat Cargo.toml | \
    tomlq -t \
      --argjson members $(\
        cat Cargo.toml | \
        tomlq ".workspace.members | .[] | select(. | startswith(\"crates/\") or . == \"$service\")" | \
        jq -c -s . \
      ) \
      '. | setpath(["workspace", "members"]; $members)' \
      > Cargo.new.toml \
      && mv Cargo.new.toml Cargo.toml

  WORKDIR /app/$service

  COPY $service/Cargo.toml Cargo.toml

  RUN echo "[package]" > Cargo2.toml
  RUN cat Cargo.toml | tomlq -t '.package' | sed 's/^\[/\[package./g' >> Cargo2.toml
  RUN echo >> Cargo2.toml
  RUN cat Cargo.toml \
      | tomlq -t \
        --argjson prebuild_omit_deps $pre_build_omit_deps \
        '.dependencies | to_entries | map(select(.value.type == "string" or (.value.path == null and (.key | (type != "string" or in($prebuild_omit_deps)) | not)))) | from_entries' \
      | sed 's/^\[/\[dependencies./g' \
      >> Cargo2.toml
  RUN echo "\n[features]" >> Cargo2.toml
  RUN cat Cargo.toml \
    | tomlq \
      --arg path_deps_regex $(cat Cargo.toml | tomlq -cr '.dependencies | to_entries | map(select(.value | type != "string" and .path != null)) | from_entries | keys | .[]' | xargs echo | sed 's/ /|/g' | xargs -I {} echo '^({})(/|$)') \
      -t '.features | map_values(map_values(select(. | . == null or test($path_deps_regex) | not)))' \
    >> Cargo2.toml
  RUN mv Cargo2.toml Cargo.toml$file_copy

  RUN rm -rf src && mkdir src && echo "fn main() {}" > src/main.rs

  $pre_build

  RUN rm -rf src
  COPY $service/Cargo.toml Cargo.toml

  COPY $service/migrations migrations
  COPY $service/src src

  $build

FROM debian:11-slim
  WORKDIR /app

  RUN apt-get update
  RUN apt-get -y install \
    ca-certificates \
    libpq5 \
    libssl-dev
  RUN rm -rf /var/lib/apt/lists/*

  $binary_copy
