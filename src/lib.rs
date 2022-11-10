#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate serde;

pub mod docker;
pub mod eslint;
pub mod git;
pub mod workspace_clippy;

pub mod prelude {
    use super::*;
    pub use docker::prelude::*;
    pub use eslint::*;
    pub use git::prelude::*;
    pub use workspace_clippy::*;
}
