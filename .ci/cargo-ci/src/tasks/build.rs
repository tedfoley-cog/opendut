use std::path::PathBuf;

use crate::{constants, Package};
use crate::core::commands::CROSS;
use crate::types::Arch;
use crate::core::types::parsing::target::TargetSelection;
use crate::util::RunRequiringSuccess;


/// Perform a release build, without bundling a distribution.
#[derive(Debug, clap::Parser)]
#[command(hide=true)]
pub struct DistributionBuildCli {
    #[arg(long, default_value_t)]
    pub target: TargetSelection,

    /// Build artifacts in release mode, with optimizations
    #[arg(short='r', long="release")]
    pub release_build: bool,
}

#[tracing::instrument(skip_all)]
pub fn distribution_build(package: Package, target: Arch, release_build: bool) -> crate::Result {
    let mut command = CROSS.command();

    command
        .arg("build")
        .arg("--package").arg(package.ident())
        .arg("--target-dir").arg(cross_target_dir().as_os_str()) //explicitly set target-base-dir to fix unreliable caching behavior
        .arg("--target").arg(target.triple());

    if release_build {
        command.arg("--release");
    } else {
         // Optimize for binary size, since compressing, uncompressing and transferring the binary can take a long time: https://doc.rust-lang.org/cargo/reference/profiles.html
         // We still want a non-release profile, since `shadow-rs` will update timestamps in those, which breaks caching of distributions.
        command
            .arg("--config").arg("profile.dev.opt-level='s'")
            .arg("--config").arg("profile.dev.debug=false");
    }

    command.run_requiring_success()
}

pub fn out_file(package: Package, target: Arch, release_build: bool) -> PathBuf {
    let profile = if release_build { "release" } else { "debug" };
    cross_target_dir().join(target.triple()).join(profile).join(package.ident())
}

fn cross_target_dir() -> PathBuf {
    let cargo_target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| constants::target_dir());
    cargo_target_dir.join("cross")
}
