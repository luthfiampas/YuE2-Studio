//! De-reverb: one model, one run.
//!
//! The vocal stem goes in, the dry take comes out beside the reverb that was
//! removed. It shares the Python environment with voice swap (torch is the same
//! half) and owns everything else: its model, its runs, its history.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::voice_swap::{observe, valid_id, Job};

const INSTALLER: &str = include_str!("dereverb/install.sh");
const RUNNER: &str = include_str!("dereverb/runner.py");
pub const MODEL_FILE: &str = "UVR-DeEcho-DeReverb.pth";

/// One run: the stem it was made from, and the two files it left.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    /// The name it was chosen by.
    pub source: String,
    /// Its name inside the run's folder.
    pub source_file: String,
    pub created: String,
    /// `running`, `done` or `error`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

pub struct Dereverb {
    root: PathBuf,
    /// The shared environment; only its loader and model are added here.
    venv: PathBuf,
    job: Arc<std::sync::Mutex<Option<Job>>>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    group: Arc<std::sync::Mutex<Option<u32>>>,
}

impl Dereverb {
    pub fn new(data_root: &Path) -> Self {
        let dereverb = Self {
            root: data_root.join("dereverb"),
            venv: data_root.join("python").join("venv"),
            job: Arc::new(std::sync::Mutex::new(None)),
            task: tokio::sync::Mutex::new(None),
            group: Arc::new(std::sync::Mutex::new(None)),
        };
        // Nothing survives a service restart: a run left "running" has no
        // process behind it.
        dereverb.interrupt_running();
        dereverb
    }

    fn interrupt_running(&self) {
        for run in self.runs().into_iter().filter(|run| run.status == "running") {
            self.save_run(&Run { status: "error".into(), error: Some("Interrupted by a restart.".into()), ..run })
                .ok();
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn venv_python(&self) -> PathBuf {
        self.venv.join("bin/python")
    }

    fn model(&self) -> PathBuf {
        self.root.join("models").join(MODEL_FILE)
    }

    /// Both halves for this tool: the shared interpreter, and the model.
    pub fn installed(&self) -> bool {
        // The marker is written last: a model file left by a failed install is
        // not an install, and the page would hide its only repair.
        self.runtime_installed() && self.model().is_file() && self.root.join(".ready").is_file()
    }

    pub fn runtime_installed(&self) -> bool {
        self.venv_python().is_file() && self.venv.join(".runtime-ready").is_file()
    }

    pub fn job(&self) -> Option<Job> {
        self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub async fn busy(&self) -> bool {
        self.task.lock().await.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    fn write_scripts(&self) -> Result<()> {
        let scripts = self.root.join("scripts");
        std::fs::create_dir_all(&scripts).with_context(|| format!("create {}", scripts.display()))?;
        std::fs::write(scripts.join("install.sh"), INSTALLER)?;
        std::fs::write(scripts.join("runner.py"), RUNNER)?;
        Ok(())
    }

    pub fn runs(&self) -> Vec<Run> {
        let Ok(entries) = std::fs::read_dir(self.root.join("jobs")) else { return Vec::new() };
        let mut runs: Vec<Run> = entries
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("run.json")).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        runs.sort_by(|a, b| b.created.cmp(&a.created));
        runs
    }

    pub fn run(&self, id: &str) -> Result<Run> {
        if !valid_id(id) {
            bail!("not a run id");
        }
        let path = self.root.join("jobs").join(id).join("run.json");
        let text = std::fs::read_to_string(&path).with_context(|| "no such run".to_string())?;
        Ok(serde_json::from_str(&text)?)
    }

    fn save_run(&self, run: &Run) -> Result<()> {
        let directory = self.root.join("jobs").join(&run.id);
        std::fs::create_dir_all(&directory)?;
        let text = serde_json::to_string_pretty(run)?;
        let temporary = directory.join("run.json.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, directory.join("run.json"))?;
        Ok(())
    }

    /// Copies the stem in and records the run, before any job exists: an upload
    /// that ends with its request must not pull a queued run out from under it.
    pub fn create_run(&self, name: &str, source: &Path) -> Result<Run> {
        let stamp = chrono::Utc::now();
        let id = format!("d{}{:04}", stamp.timestamp_millis(), unique_counter());
        let directory = self.root.join("jobs").join(&id);
        std::fs::create_dir_all(&directory)?;
        let extension = Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("wav")
            .to_lowercase();
        let source_file = format!("source.{extension}");
        std::fs::copy(source, directory.join(&source_file))?;
        let run = Run {
            id,
            source: name.to_string(),
            source_file,
            created: stamp.to_rfc3339(),
            status: "running".into(),
            error: None,
        };
        self.save_run(&run)?;
        Ok(run)
    }

    pub fn delete_run(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("not a run id");
        }
        let directory = self.root.join("jobs").join(id);
        if !directory.is_dir() {
            bail!("no such run");
        }
        std::fs::remove_dir_all(&directory).with_context(|| format!("remove {}", directory.display()))?;
        Ok(())
    }

    pub async fn install(&self) -> Result<()> {
        if !self.runtime_installed() {
            bail!("the Python runtime is not installed; set it up under Settings, Python runtime");
        }
        self.write_scripts()?;
        let script = self.root.join("scripts/install.sh");
        let arguments = vec![script.to_string_lossy().to_string()];
        let environment = vec![("DEREVERB_VENV".to_string(), self.venv.to_string_lossy().to_string())];
        self.start("install", None, "bash", arguments, environment).await
    }

    pub async fn run_job(&self, id: &str) -> Result<()> {
        if !self.installed() {
            bail!("the de-reverb model is not installed");
        }
        let run = self.run(id)?;
        self.write_scripts()?;
        let directory = self.root.join("jobs").join(id);
        let job = serde_json::json!({
            "source": directory.join(&run.source_file),
            "output": directory,
            "model": self.model(),
        });
        let job_path = directory.join("job.json");
        std::fs::write(&job_path, serde_json::to_string_pretty(&job)?)?;
        let python = self.venv_python();
        let arguments = vec![
            self.root.join("scripts/runner.py").to_string_lossy().to_string(),
            "run".into(),
            "--root".into(),
            self.root.to_string_lossy().to_string(),
            "--job".into(),
            job_path.to_string_lossy().to_string(),
        ];
        self.start("run", Some(id.to_string()), &python.to_string_lossy(), arguments, Vec::new()).await
    }

    /// One job at a time; the runner is the only process this tool starts.
    async fn start(
        &self,
        kind: &str,
        subject: Option<String>,
        program: &str,
        arguments: Vec<String>,
        environment: Vec<(String, String)>,
    ) -> Result<()> {
        let mut task = self.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            bail!("another de-reverb job is running");
        }

        let root = self.root.clone();
        let kind = kind.to_string();
        let program = program.to_string();
        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default();
        *self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Job {
            kind: kind.clone(),
            subject: subject.clone().unwrap_or_default(),
            stage: "Starting".into(),
            started_ms,
            ..Job::default()
        });

        let shared = self.job.clone();
        *self.group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let group = self.group.clone();
        let handle = tokio::spawn(async move {
            let mut command = tokio::process::Command::new(&program);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.as_std_mut().process_group(0);
            }
            command
                .args(&arguments)
                .current_dir(&root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            for (key, value) in environment {
                command.env(key, value);
            }
            let started = command.spawn();
            let mut child = match started {
                Ok(child) => child,
                Err(error) => {
                    finish(&shared, Some(format!("could not start {program}: {error}")), &kind, subject.as_deref(), &root);
                    *group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                    return;
                }
            };

            *group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = child.id();
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let errors = stderr.map(|stream| {
                let shared = shared.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut lines = tokio::io::BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let mut job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        if let Some(current) = job.as_mut() {
                            observe(current, &line);
                        }
                    }
                })
            });
            if let Some(stdout) = stdout {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                let mut ended = false;
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(current) = job.as_mut() {
                        ended = observe(current, &line);
                    }
                    drop(job);
                    if ended {
                        break;
                    }
                }
            }
            let status = child.wait().await;
            if let Some(errors) = errors {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), errors).await;
            }
            let clean = {
                let job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                job.as_ref().is_some_and(|job| job.done && job.error.is_none())
            };
            let reported = {
                let job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                job.as_ref().and_then(|job| job.error.clone())
            };
            let failure = match status {
                Ok(_) if clean => None,
                Ok(status) => Some(reported.unwrap_or_else(|| format!("{kind} exited with {status}"))),
                Err(error) => Some(format!("{kind} did not finish: {error}")),
            };
            if let Some(message) = failure {
                finish(&shared, Some(message), &kind, subject.as_deref(), &root);
            } else if let Some(id) = subject.as_deref() {
                settle_run(&root, id, None);
            }
            *group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        });
        *task = Some(handle);
        Ok(())
    }

    pub async fn cancel(&self) -> bool {
        let mut task = self.task.lock().await;
        let Some(handle) = task.as_mut() else { return false };
        if handle.is_finished() {
            return false;
        }
        if let Some(pid) = *self.group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            #[cfg(unix)]
            let _ = std::process::Command::new("kill").arg("-KILL").arg(format!("-{pid}")).status();
            #[cfg(windows)]
            let _ = std::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).status();
        }
        handle.abort();
        let subject = {
            let mut job = self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let subject = job.as_ref().filter(|job| job.kind == "run").map(|job| job.subject.clone());
            if let Some(job) = job.as_mut() {
                job.done = true;
                job.error = Some("Stopped.".into());
            }
            subject
        };
        if let Some(id) = subject.filter(|id| valid_id(id)) {
            settle_run(&self.root, &id, Some("Stopped."));
        }
        true
    }
}

/// A digit that keeps two runs made in the same millisecond apart.
fn unique_counter() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 10_000
}

fn settle_run(root: &Path, id: &str, failure: Option<&str>) {
    let path = root.join("jobs").join(id).join("run.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(mut run) = serde_json::from_str::<Run>(&text) else { return };
    match failure {
        None => run.status = "done".into(),
        Some(message) => {
            run.status = "error".into();
            run.error = Some(message.to_string());
        }
    }
    let directory = root.join("jobs").join(id);
    if let Ok(text) = serde_json::to_string_pretty(&run) {
        let temporary = directory.join("run.json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, directory.join("run.json"));
        }
    }
    // The source was only a copy, and a finished run keeps just its two stems.
    if failure.is_none() {
        let _ = std::fs::remove_file(directory.join(&run.source_file));
    }
}

fn finish(shared: &Arc<std::sync::Mutex<Option<Job>>>, message: Option<String>, kind: &str, subject: Option<&str>, root: &Path) {
    if let Some(subject) = subject {
        settle_run(root, subject, message.as_deref());
    }
    let mut job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(current) = job.as_mut() {
        if current.done {
            return;
        }
        current.done = true;
        if let Some(message) = message {
            current.error = Some(format!("{kind} failed: {message}"));
        }
        return;
    }
    *job = Some(Job { kind: kind.to_string(), done: true, error: message, ..Job::default() });
}
