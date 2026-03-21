#![allow(missing_docs)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::MuAgentError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstructionFile {
    pub path: PathBuf,
    pub contents: String,
}

pub fn load_instruction_files(
    cwd: &Path,
    home_override: Option<&Path>,
) -> Result<Vec<InstructionFile>, MuAgentError> {
    let mut files = Vec::new();
    if let Some(home) = home_override {
        let global = home.join(".mu/agent/AGENTS.md");
        if global.exists() {
            files.push(read_file(global)?);
        }
    } else if let Ok(home) = std::env::var("MU_HOME") {
        let global = PathBuf::from(home).join("agent/AGENTS.md");
        if global.exists() {
            files.push(read_file(global)?);
        }
    } else if let Some(home) = dirs::home_dir() {
        let global = home.join(".mu/agent/AGENTS.md");
        if global.exists() {
            files.push(read_file(global)?);
        }
    }

    let mut dirs = Vec::new();
    let mut current = Some(cwd.to_path_buf());
    while let Some(dir) = current {
        dirs.push(dir.clone());
        current = dir.parent().map(Path::to_path_buf);
    }
    dirs.reverse();

    for dir in dirs {
        let agents = dir.join("AGENTS.md");
        let claude = dir.join("CLAUDE.md");
        if agents.exists() {
            files.push(read_file(agents)?);
        } else if claude.exists() {
            files.push(read_file(claude)?);
        }
    }

    Ok(files)
}

pub fn render_instruction_text(files: &[InstructionFile]) -> String {
    files
        .iter()
        .map(|file| format!("## {}\n{}", file.path.display(), file.contents.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn read_file(path: PathBuf) -> Result<InstructionFile, MuAgentError> {
    let contents = std::fs::read_to_string(&path)?;
    Ok(InstructionFile { path, contents })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{load_instruction_files, render_instruction_text};

    #[test]
    fn loads_global_and_nested_instructions() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let home = tempdir.path().join("home");
        let cwd = tempdir.path().join("repo/a/b");
        if let Err(error) = std::fs::create_dir_all(home.join(".mu/agent")) {
            panic!("home dir should exist: {error}");
        }
        if let Err(error) = std::fs::create_dir_all(&cwd) {
            panic!("cwd should exist: {error}");
        }
        if let Err(error) = std::fs::write(home.join(".mu/agent/AGENTS.md"), "global") {
            panic!("write should succeed: {error}");
        }
        if let Err(error) = std::fs::write(tempdir.path().join("repo/AGENTS.md"), "repo") {
            panic!("write should succeed: {error}");
        }
        if let Err(error) = std::fs::write(tempdir.path().join("repo/a/CLAUDE.md"), "nested") {
            panic!("write should succeed: {error}");
        }

        let files = match load_instruction_files(&cwd, Some(&home)) {
            Ok(value) => value,
            Err(error) => panic!("instructions should load: {error}"),
        };
        let rendered = render_instruction_text(&files);
        assert!(rendered.contains("global"));
        assert!(rendered.contains("repo"));
        assert!(rendered.contains("nested"));
    }
}
