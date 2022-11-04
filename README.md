# ops
CLI/CI/CD tools for git repo management. Designed to be integrate with [pre-commit](https://pre-commit.com).

The main inspiration for the `clippy-workspace` and `eslint` crates is to provide git savvy wrappers around the two tools when integrated with git pre-push hooks as opposed to git pre-commit hooks. Both `cargo clippy` and `eslint` can have very long run times in large codebases and prohibit fast development if run prior to every commit. Integrating them as pre-push hooks provides the same level of safety without becoming cumbersome.
