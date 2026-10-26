//! The frame diff harness: our run and an external reference emulator's run both stop at
//! every vertical interrupt handler entry and save the machine state there (work RAM, VRAM,
//! CRAM, VSRAM, VDP registers) in one dump layout; the diff lists every stop where they
//! differ, realigning on whole-stop shifts and reporting each.
//!
//! Stops are counted from the first vertical interrupt taken after reset, so the runs align
//! on what the game sees rather than on frames: a frame whose interrupt the game held back
//! (a lag frame) merges into the next delivery in both runs, and a different count shows up
//! as a shift or a divergence. [`input`] is the replay format both runs read, [`script`] writes the
//! reference emulator's debugger script, [`mask`] reads the signed-off masks.

pub mod diff;
pub mod dump;
pub mod input;
pub mod mask;
pub mod script;

use std::{io, path::PathBuf};

pub use diff::{Folder, Report, Run, Shift, diff};
pub use dump::{Dump, Region};
pub use input::{Buttons, Input};
pub use mask::{Mask, Masks};

/// What reading or writing the harness's files can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{} is {size} bytes, not {expected}", path.display())]
    Size {
        path: PathBuf,
        size: usize,
        expected: usize,
    },
    #[error("input line {line}: {problem}")]
    Input { line: usize, problem: String },
    #[error("masks: {0}")]
    Masks(String),
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process,
    };

    /// A folder of its own under the system's temporary folder, removed when dropped.
    pub struct Scratch(PathBuf);

    impl Scratch {
        pub fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("anygenesis-framediff-{name}-{}", process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temporary folder");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
