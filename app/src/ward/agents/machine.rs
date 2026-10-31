//! The machine under the worker: hardware and load, tools in `PATH`, clones in the root, and
//! the processes of sessions. [`Machine`] is faked in tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use command::Stdio;
use command::blocking::Command;
use instant::Instant;
use parking_lot::Mutex;

use super::contract::Resources;
use super::launch::RunnerKind;

/// Hardware totals for the passport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hardware {
    pub cpu_cores: u64,
    pub ram_total_bytes: u64,
    pub disk_total_bytes: u64,
}

pub trait Machine: Send + Sync {
    /// `root`: the repository root, whose disk is reported.
    fn hardware(&self, root: &Path) -> Hardware;
    fn resources(&self, root: &Path) -> Resources;
    fn has_tool(&self, name: &str) -> bool;
    /// First line of `<name> --version`.
    fn tool_version(&self, name: &str) -> Option<String>;
    fn is_alive(&self, pid: u32) -> bool;
    /// Ends the processes and their children.
    fn stop(&self, pids: &[u32]);
    /// Which runner the tabs use, with Git Bash's `bash.exe` for sh on Windows.
    fn runner(&self) -> Option<(RunnerKind, Option<PathBuf>)>;
}

/// `owner/repo` of the git clones at `<root>/<owner>/<repo>`.
pub fn scan_repos(root: &Path) -> Vec<String> {
    let mut repos = Vec::new();
    let Ok(owners) = std::fs::read_dir(root) else {
        return repos;
    };
    for owner in owners.flatten() {
        if !owner.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(names) = std::fs::read_dir(owner.path()) else {
            continue;
        };
        for name in names.flatten() {
            if name.path().join(".git").exists() {
                repos.push(format!(
                    "{}/{}",
                    owner.file_name().to_string_lossy(),
                    name.file_name().to_string_lossy()
                ));
            }
        }
    }
    repos.sort();
    repos
}

/// Finds `name` in `path` (the `PATH` value), trying `extensions` (`PATHEXT` on Windows).
pub fn find_in_path(name: &str, path: &std::ffi::OsStr, extensions: &[String]) -> Option<PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        if extensions.is_empty() {
            let candidate = dir.join(name);
            return candidate.is_file().then_some(candidate);
        }
        extensions.iter().find_map(|extension| {
            let candidate = dir.join(format!("{name}{extension}"));
            candidate.is_file().then_some(candidate)
        })
    })
}

fn path_extensions() -> Vec<String> {
    if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        pathext
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| extension.to_lowercase())
            .collect()
    } else {
        Vec::new()
    }
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_path(name, &path, &path_extensions())
}

fn hidden_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    command
}

/// How long `--version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The machine Ward runs on.
pub struct RealMachine {
    system: Mutex<sysinfo::System>,
}

impl Default for RealMachine {
    fn default() -> Self {
        Self {
            system: Mutex::new(sysinfo::System::new()),
        }
    }
}

/// The disk holding `path`: the longest mount point that is a prefix of it.
fn disk_of(path: &Path) -> Option<(u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let path = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf());
    let path = super::provider::normalize_path(&path.to_string_lossy());
    disks
        .list()
        .iter()
        .filter(|disk| {
            let mount = super::provider::normalize_path(&disk.mount_point().to_string_lossy());
            path.starts_with(&mount)
        })
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| (disk.total_space(), disk.available_space()))
}

impl Machine for RealMachine {
    fn hardware(&self, root: &Path) -> Hardware {
        let mut system = self.system.lock();
        system.refresh_memory();
        system.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
        Hardware {
            cpu_cores: system.cpus().len() as u64,
            ram_total_bytes: system.total_memory(),
            disk_total_bytes: disk_of(root).map(|(total, _)| total).unwrap_or_default(),
        }
    }

    fn resources(&self, root: &Path) -> Resources {
        let mut system = self.system.lock();
        system.refresh_memory();
        // The usage is measured since the previous refresh, one heartbeat ago.
        system.refresh_cpu_usage();
        Resources {
            cpu_percent: f64::from(system.global_cpu_usage()),
            ram_free_bytes: system.available_memory(),
            disk_free_bytes: disk_of(root).map(|(_, free)| free).unwrap_or_default(),
        }
    }

    fn has_tool(&self, name: &str) -> bool {
        which(name).is_some()
    }

    fn tool_version(&self, name: &str) -> Option<String> {
        let program = which(name)?;
        let mut child = hidden_command(&program)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < VERSION_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = child.kill();
                    return None;
                }
            }
        }
        let mut output = String::new();
        use std::io::Read as _;
        child.stdout.take()?.read_to_string(&mut output).ok()?;
        output
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    }

    fn is_alive(&self, pid: u32) -> bool {
        let pid = sysinfo::Pid::from_u32(pid);
        let mut system = self.system.lock();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).is_some()
    }

    fn stop(&self, pids: &[u32]) {
        for pid in pids {
            let result = if cfg!(windows) {
                hidden_command(Path::new("taskkill"))
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
            } else {
                // The runner's children first (the agent), then the given process itself.
                let _ = hidden_command(Path::new("pkill"))
                    .args(["-TERM", "-P", &pid.to_string()])
                    .status();
                hidden_command(Path::new("kill"))
                    .args(["-TERM", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
            };
            if let Err(err) = result {
                log::warn!("Ward agents: could not stop process {pid}: {err}");
            }
        }
    }

    fn runner(&self) -> Option<(RunnerKind, Option<PathBuf>)> {
        if cfg!(windows) {
            if which("pwsh").is_some() {
                return Some((RunnerKind::Pwsh, None));
            }
            let git_bash = std::env::var_os("ProgramFiles")
                .map(|dir| PathBuf::from(dir).join("Git").join("bin").join("bash.exe"))
                .filter(|path| path.is_file())?;
            Some((RunnerKind::Sh, Some(git_bash)))
        } else {
            Some((RunnerKind::Sh, None))
        }
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
