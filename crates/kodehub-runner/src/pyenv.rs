//! A shared Python virtualenv under `$KODEHUB_HOME/pyenv` with pytest and any packages that
//! versions request via `attributes.packages`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::exec;
use crate::{Result, Runner, RunnerError};

impl Runner {
    pub fn python(&self) -> PathBuf {
        self.pyenv.join("bin").join("python")
    }

    fn installed_file(&self) -> PathBuf {
        self.pyenv.join(".kode-packages")
    }

    /// Ensure the venv exists and contains pytest + `packages`.
    pub async fn ensure_python(&self, packages: &[String]) -> Result<PathBuf> {
        let _g = self.py_lock.lock().await;
        let venv = self.pyenv.clone();
        let py = self.python();
        let uv = which("uv");
        if !py.exists() {
            std::fs::create_dir_all(&self.home)?;
            let out = if let Some(uv) = &uv {
                let c = exec::command(uv, &["venv", "--quiet", venv.to_str().unwrap()], false);
                exec::run(c, None, Duration::from_secs(300), None).await?
            } else {
                let c = exec::command(
                    &which("python3").unwrap_or_else(|| "python3".into()),
                    &["-m", "venv", venv.to_str().unwrap()],
                    false,
                );
                exec::run(c, None, Duration::from_secs(300), None).await?
            };
            if !out.success() {
                return Err(RunnerError::Build(format!(
                    "could not create python venv:\n{}",
                    out.combined()
                )));
            }
        }
        let have: BTreeSet<String> = std::fs::read_to_string(self.installed_file())
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let mut want: BTreeSet<String> = packages.iter().cloned().collect();
        want.insert("pytest".into());
        let missing: Vec<String> = want.difference(&have).cloned().collect();
        if missing.is_empty() {
            return Ok(py);
        }
        let mut args: Vec<String> = Vec::new();
        let program = if let Some(uv) = &uv {
            args.extend(["pip", "install", "--quiet", "--python"].map(String::from));
            args.push(py.to_string_lossy().into());
            uv.clone()
        } else {
            args.extend(["-m", "pip", "install", "--quiet"].map(String::from));
            py.clone()
        };
        args.extend(missing.iter().cloned());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = exec::run(
            exec::command(&program, &argv, false),
            None,
            Duration::from_secs(600),
            None,
        )
        .await?;
        if !out.success() {
            return Err(RunnerError::Build(format!(
                "could not install python packages {missing:?}:\n{}",
                out.combined()
            )));
        }
        let all: Vec<String> = have.union(&want).cloned().collect();
        std::fs::write(self.installed_file(), all.join("\n"))?;
        Ok(py)
    }
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let p = PathBuf::from(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let p = PathBuf::from(extra).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
