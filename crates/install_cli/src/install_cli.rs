#[cfg(not(target_os = "windows"))]
mod install_cli_binary;
mod register_autocode_scheme;

#[cfg(not(target_os = "windows"))]
pub use install_cli_binary::{InstallCliBinary, install_cli_binary};
pub use register_autocode_scheme::{RegisterAutocodeScheme, register_autocode_scheme};
