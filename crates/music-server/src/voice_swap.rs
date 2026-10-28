//! Voice swap: training a voice from recordings and singing a vocal stem in it.
//!
//! The heavy part is RVC, which is Python. It is an optional engine like the
//! assistant's llama.cpp: fetched on request into a folder of its own, and only
//! alive while a job runs, so it holds no memory between swaps.
//!
//! The studio talks to one process per job; the runner prints the job's stages
//! as JSON lines (`voice_swap/runner.py` is the contract). Nothing here knows
//! what an epoch is - it starts a process and reports what the process says.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The pin: RVC's repository, commit, and the requirements file to install.
const PIN: &str = include_str!("../../../engines/rvc-source.json");
const INSTALLER: &str = include_str!("voice_swap/install.sh");
const RUNNER: &str = include_str!("voice_swap/runner.py");

/// The weights the runner needs. HuBERT is a Transformers directory at this
/// commit, not the single `.pt` file older RVC builds used.
const WEIGHTS: [(&str, u64); 6] = [
    ("rvc/assets/hubert_base/config.json", 1_492),
    ("rvc/assets/hubert_base/preprocessor_config.json", 225),
    ("rvc/assets/hubert_base/pytorch_model.bin", 189_206_711),
    ("rvc/assets/rmvpe/rmvpe.pt", 181_184_272),
    ("rvc/assets/pretrained_v2/f0G48k.pth", 75_465_569),
    ("rvc/assets/pretrained_v2/f0D48k.pth", 142_875_703),
];

/// Ids the tool modules hand out: `v`, `s` or `d`, then digits. Checked before an id is
/// joined into a path - a `..` from a request must never name a folder.
pub fn valid_id(id: &str) -> bool {
    let mut characters = id.chars();
    let prefix = matches!(characters.next(), Some('v') | Some('s') | Some('d'));
    let digits: Vec<char> = characters.collect();
    prefix && !digits.is_empty() && digits.iter().all(char::is_ascii_digit)
}

/// Writes through a temporary file: a reader polling every second must never
/// see half a JSON document and drop the voice it describes.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    let temporary = path.with_extension(format!("{}-{nanos}-{sequence}.tmp", std::process::id()));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

#[derive(Debug, Clone, Deserialize)]
struct Pin {
    repository: String,
    commit: String,
    requirements: String,
}

/// What a job is doing, as the last line it printed described it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Job {
    /// `install`, `selftest`, `train` or `convert`.
    pub kind: String,
    /// The voice or swap the job belongs to, empty for the engine jobs.
    #[serde(default)]
    pub subject: String,
    pub stage: String,
    pub progress: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The last lines the process wrote, for the reason a job failed.
    pub log: Vec<String>,
    /// When it started, as epoch milliseconds, so the page can show how long a
    /// slow download has been going.
    #[serde(skip)]
    pub started_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub done: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum Line {
    Stage {
        stage: String,
        progress: Option<f32>,
        detail: Option<String>,
    },
    Log {
        line: String,
    },
    Done {
        output: Option<String>,
        detail: Option<String>,
    },
    Error {
        message: String,
    },
}

/// Reads one line of the runner's protocol into `job`. Returns whether the job
/// has ended, so the caller stops reading.
///
/// A line that is not JSON, or is JSON of another shape, is the process talking
/// to itself - RVC's own output is logged as it is rather than lost.
pub fn observe(job: &mut Job, line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    match serde_json::from_str::<Line>(line) {
        Ok(Line::Stage { stage, progress, detail }) => {
            job.stage = stage;
            if let Some(progress) = progress {
                job.progress = progress.clamp(0.0, 1.0);
            }
            if detail.is_some() {
                job.detail = detail;
            }
            false
        }
        Ok(Line::Log { line }) => {
            push_log(job, line);
            false
        }
        Ok(Line::Done { output, detail }) => {
            if output.is_some() {
                job.detail = output.or(detail);
            } else if detail.is_some() {
                job.detail = detail;
            }
            job.progress = 1.0;
            job.done = true;
            true
        }
        Ok(Line::Error { message }) => {
            job.error = Some(message.clone());
            job.done = true;
            push_log(job, message);
            true
        }
        Err(_) => {
            push_log(job, line.to_string());
            false
        }
    }
}

/// A digit that keeps two ids minted in the same millisecond apart.
fn unique_digit() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 10_000
}

fn push_log(job: &mut Job, line: String) {
    // A run that prints thousands of lines is not worth holding; the tail is
    // what explains a failure.
    if job.log.len() >= 200 {
        job.log.remove(0);
    }
    job.log.push(line);
}

/// The current job, or the last one, shared with the task that runs it.
type Shared = std::sync::Mutex<Option<Job>>;

/// What a finished job has to write back.
#[derive(Debug, Clone)]
enum Subject {
    Engine,
    Train { voice: String, epochs: u32, save_every: u32 },
    Convert { swap: String },
}

pub struct VoiceSwap {
    root: PathBuf,
    /// The shared Python environment: one torch for every Python tool, kept
    /// beside the tools (`data/python/venv`) rather than inside one of them.
    venv: PathBuf,
    job: Arc<Shared>,
    /// The task running it, so a cancel can end the process it owns.
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The process group of the job. The runner starts RVC's own scripts, so
    /// killing only the runner would leave them training on the card.
    group: Arc<std::sync::Mutex<Option<u32>>>,
    /// Serialises the read-modify-write of voice.json / swap.json.
    writing: std::sync::Mutex<()>,
}

impl VoiceSwap {
    pub fn new(data_root: &Path) -> Self {
        let swap = Self {
            root: data_root.join("voice-swap"),
            venv: data_root.join("python").join("venv"),
            job: Arc::new(std::sync::Mutex::new(None)),
            task: tokio::sync::Mutex::new(None),
            group: Arc::new(std::sync::Mutex::new(None)),
            writing: std::sync::Mutex::new(()),
        };
        // A swap left "running" has no process behind it after a restart.
        swap.interrupt_running();
        swap
    }

    fn interrupt_running(&self) {
        for mut swap in self.swaps().into_iter().filter(|swap| swap.status == "running") {
            swap.status = "error".into();
            swap.error = Some("Interrupted by a restart.".into());
            let _ = self.save_swap(&swap);
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn pin() -> Pin {
        serde_json::from_str(PIN).expect("the RVC pin is part of the build")
    }

    fn venv_python(&self) -> PathBuf {
        self.venv.join("bin/python")
    }

    /// What is on disk. Deliberately computed from files, not from an install
    /// record alone: a half-deleted folder shows as uninstalled instead of
    /// looking ready and failing on the first swap.
    pub fn installed(&self) -> bool {
        // Written by the installer's last step, like the other tools: a clone
        // and half the weights are not an install.
        self.root.join(".ready").is_file()
            && self.venv_python().is_file()
            && self.root.join("rvc/infer/cli.py").is_file()
            && WEIGHTS.iter().all(|(path, bytes)| {
                let path = self.root.join(path);
                std::fs::metadata(path).is_ok_and(|meta| meta.len() == *bytes)
            })
    }

    /// What is missing, cheapest check first, empty when it is all there.
    pub fn missing(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if !self.venv_python().is_file() {
            missing.push("the Python environment".into());
        }
        if !self.root.join("rvc/infer/cli.py").is_file() {
            missing.push("RVC".into());
        }
        for (path, bytes) in WEIGHTS {
            let present = std::fs::metadata(self.root.join(path)).is_ok_and(|meta| meta.len() == bytes);
            if !present {
                missing.push(
                    Path::new(path)
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or(path)
                        .to_string(),
                );
            }
        }
        missing
    }

    pub fn job(&self) -> Option<Job> {
        self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub async fn busy(&self) -> bool {
        let task = self.task.lock().await;
        task.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    /// Writes the two scripts the server owns. They travel inside the binary,
    /// so the studio and its runner can never be different versions.
    fn write_scripts(&self) -> Result<()> {
        let scripts = self.root.join("scripts");
        std::fs::create_dir_all(&scripts).with_context(|| format!("create {}", scripts.display()))?;
        std::fs::write(scripts.join("install.sh"), INSTALLER)?;
        std::fs::write(scripts.join("runner.py"), RUNNER)?;
        Ok(())
    }

    /// Whether the shared Python environment is there. The RVC checkout and
    /// its weights are the engine; this is the runtime both halves need.
    pub fn runtime_installed(&self) -> bool {
        // The marker is the last thing the installer writes. torch's folder
        // exists minutes earlier, while pip is still unpacking the rest.
        self.venv_python().is_file() && self.venv.join(".runtime-ready").is_file()
    }

    pub async fn install(&self, runtime_only: bool) -> Result<()> {
        let pin = Self::pin();
        self.write_scripts()?;
        let script = self.root.join("scripts/install.sh");
        let arguments = vec![script.to_string_lossy().to_string()];
        let environment = vec![
            ("VOICE_RVC_REPOSITORY".to_string(), pin.repository),
            ("VOICE_RVC_COMMIT".to_string(), pin.commit),
            ("VOICE_RVC_REQUIREMENTS".to_string(), pin.requirements),
            ("VOICE_VENV".to_string(), self.venv.to_string_lossy().to_string()),
            ("VOICE_RUNTIME_ONLY".to_string(), if runtime_only { "1" } else { "0" }.to_string()),
        ];
        let kind = if runtime_only { "runtime" } else { "install" };
        self.start(kind, Subject::Engine, "bash", arguments, environment).await
    }

    pub async fn selftest(&self) -> Result<()> {
        if !self.installed() {
            bail!("the engine is not installed");
        }
        self.write_scripts()?;
        let python = self.venv_python();
        if !python.is_file() {
            bail!("the environment is missing its Python");
        }
        let arguments = vec![
            self.root.join("scripts/runner.py").to_string_lossy().to_string(),
            "selftest".into(),
            "--root".into(),
            self.root.to_string_lossy().to_string(),
        ];
        self.start("selftest", Subject::Engine, &python.to_string_lossy(), arguments, Vec::new()).await
    }

    /// Starts a training run for one voice. The runner splits the recordings,
    /// reads pitch and timbre, trains and builds the index; the checkpoints it
    /// leaves are picked up by `settle` when the process ends.
    pub async fn train(&self, voice_id: &str, name: Option<&str>, epochs: u32, save_every: u32, batch: u32) -> Result<()> {
        let voice = self.voice(voice_id)?;
        if self.trained(&voice) {
            bail!("this voice already has a model");
        }
        if !self.installed() {
            bail!("the engine is not installed");
        }
        if self.busy().await {
            bail!("another voice swap job is running");
        }
        if voice.recordings.is_empty() {
            bail!("this voice has no recordings");
        }
        // Named only once the run is accepted, so a refused click does not
        // retitle the draft.
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
            self.update_voice(&voice.id, Some(name), None, None, None)?;
        }
        self.write_scripts()?;
        let job = serde_json::json!({
            "name": voice.id,
            "recordings": self.voices_dir().join(&voice.id).join("recordings"),
            "output": self.voices_dir().join(&voice.id),
            "epochs": epochs.max(1),
            "save_every": save_every.max(1),
            // Zero means "let the runner size it from the card it computes on".
            "batch": batch,
        });
        let job_path = self.voices_dir().join(&voice.id).join("train.json");
        std::fs::write(&job_path, serde_json::to_string_pretty(&job)?)?;
        let python = self.venv_python();
        let arguments = vec![
            self.root.join("scripts/runner.py").to_string_lossy().to_string(),
            "train".into(),
            "--root".into(),
            self.root.to_string_lossy().to_string(),
            "--job".into(),
            job_path.to_string_lossy().to_string(),
        ];
        self.start(
            "train",
            Subject::Train { voice: voice.id, epochs: epochs.max(1), save_every: save_every.max(1) },
            &python.to_string_lossy(),
            arguments,
            Vec::new(),
        )
        .await
    }

    /// Starts a conversion. The stem is already on disk, copied in when the
    /// swap was made, so a queued job does not depend on an upload staying open.
    pub async fn convert(&self, swap_id: &str) -> Result<()> {
        if !self.installed() {
            bail!("the engine is not installed");
        }
        let swap = self.swap(swap_id)?;
        let voice = self.voice(&swap.voice)?;
        let model = voice.model().context("this voice has no trained model yet")?;
        let directory = self.swaps_dir().join(swap_id);
        let model_path = self.voices_dir().join(&voice.id).join(model);
        if !model_path.is_file() {
            bail!("the model file for this voice is missing");
        }
        let source = directory.join(&swap.source_file);
        let output = directory.join("output.wav");
        let index = self.voices_dir().join(&voice.id).join("model.index");
        let job = serde_json::json!({
            "model": model_path,
            "index": if index.is_file() { serde_json::json!(index) } else { serde_json::Value::Null },
            "source": source,
            "output": output,
            "pitch": swap.pitch,
            "index_rate": swap.index_rate,
            "protect": swap.protect,
            "rms_mix_rate": swap.rms_mix,
        });
        let job_path = directory.join("convert.json");
        std::fs::write(&job_path, serde_json::to_string_pretty(&job)?)?;
        self.write_scripts()?;
        let python = self.venv_python();
        let arguments = vec![
            self.root.join("scripts/runner.py").to_string_lossy().to_string(),
            "convert".into(),
            "--root".into(),
            self.root.to_string_lossy().to_string(),
            "--job".into(),
            job_path.to_string_lossy().to_string(),
        ];
        self.start(
            "convert",
            Subject::Convert { swap: swap_id.to_string() },
            &python.to_string_lossy(),
            arguments,
            Vec::new(),
        )
        .await
    }

    /// Starts one job. One at a time: two installers would fight over the same
    /// folder, and the runner would take the graphics card twice.
    async fn start(
        &self,
        kind: &str,
        subject: Subject,
        program: &str,
        arguments: Vec<String>,
        environment: Vec<(String, String)>,
    ) -> Result<()> {
        let mut task = self.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            bail!("another voice swap job is running");
        }

        let root = self.root.clone();
        let kind = kind.to_string();
        let program = program.to_string();
        let label = match &subject {
            Subject::Train { voice, .. } => voice.clone(),
            Subject::Convert { swap } => swap.clone(),
            Subject::Engine => String::new(),
        };
        let settle = subject.clone();
        *self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Job {
            kind: kind.clone(),
            subject: label,
            stage: "Starting".into(),
            started_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or_default(),
            ..Job::default()
        });

        // The child is killed when its task ends, so a cancel - or a studio
        // that closes - never leaves an installer holding the volume.
        let shared = self.job.clone();
        *self.group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let self_group = self.group.clone();
        let handle = tokio::spawn(async move {
            let mut command = tokio::process::Command::new(&program);
            // Its own process group: cancelling takes RVC's scripts with it.
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
            let group = self_group.clone();
            let mut child = match started {
                Ok(child) => child,
                Err(error) => {
                    let message = format!("could not start {program}: {error}");
                    finish(&shared, Err(message.clone()), &kind);
                    settle_job(&root, &settle, Some(&message));
                    *group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                    return;
                }
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let errors = stderr.map(|stream| {
                let shared = shared.clone();
                tokio::spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut lines = tokio::io::BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        push_line(&shared, line);
                    }
                })
            });
            *group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = child.id();
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
                // A process the runner started may still hold the pipe; the
                // job is over when the runner is, not when the pipe closes.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), errors).await;
            }
            let ended_cleanly = {
                let job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                job.as_ref().is_some_and(|job| job.done && job.error.is_none())
            };
            let failure = match status {
                // The line it printed is the answer, whatever the exit code.
                Ok(_) if ended_cleanly => None,
                Ok(status) => Some(format!("{kind} exited with {status}")),
                Err(error) => Some(format!("{kind} did not finish: {error}")),
            };
            if let Some(message) = failure.clone() {
                finish(&shared, Err(message), &kind);
            }
            settle_job(&root, &settle, failure.as_deref());
        });
        *task = Some(handle);
        Ok(())
    }

    /// Ends the running job by dropping its task, which kills the process.
    pub async fn cancel(&self) -> bool {
        let mut task = self.task.lock().await;
        let Some(handle) = task.as_mut() else { return false };
        if handle.is_finished() {
            // The task ended without settling. Repair the record, or the page
            // stays disabled by a job that will never finish.
            let mut job = self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(job) = job.as_mut().filter(|job| !job.done) {
                job.done = true;
                job.error = Some("The job ended without reporting a result.".into());
            }
            return false;
        }
        // The whole group, not just the runner: RVC's scripts are its children.
        if let Some(pid) = *self.group.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            #[cfg(unix)]
            let _ = std::process::Command::new("kill").arg("-KILL").arg(format!("-{pid}")).status();
            #[cfg(windows)]
            let _ = std::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).status();
        }
        handle.abort();
        let mut subject = None;
        {
            let mut job = self.job.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(job) = job.as_mut() {
                job.done = true;
                job.error = Some("Stopped.".into());
                if job.kind == "convert" {
                    subject = Some(job.subject.clone());
                }
            }
        }
        if let Some(swap) = subject.filter(|swap| valid_id(swap)) {
            finish_swap(&self.root, &swap, Some("Stopped."));
        }
        true
    }
}

fn push_line(shared: &Arc<Shared>, line: String) {
    let mut job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(current) = job.as_mut() {
        observe(current, &line);
    }
}

fn finish(shared: &Arc<Shared>, outcome: Result<(), String>, kind: &str) {
    let mut job = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match job.as_mut() {
        Some(current) => {
            if current.done {
                return;
            }
            current.done = true;
            match outcome {
                Ok(()) => current.progress = 1.0,
                Err(message) => {
                    let message = format!("{kind} failed: {message}");
                    push_log(current, message.clone());
                    current.error = Some(message);
                }
            }
        }
        None => {
            *job = Some(Job {
                kind: kind.to_string(),
                stage: "Finished".into(),
                done: true,
                error: outcome.err(),
                ..Job::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job { kind: "selftest".into(), ..Job::default() }
    }

    #[test]
    fn a_stage_line_moves_the_job() {
        let mut job = job();
        assert!(!observe(&mut job, r#"{"event":"stage","stage":"Checking RVC","progress":0.6}"#));
        assert_eq!(job.stage, "Checking RVC");
        assert!((job.progress - 0.6).abs() < f32::EPSILON);
        assert!(!job.done);
    }

    #[test]
    fn a_done_line_ends_it_and_keeps_the_output() {
        let mut job = job();
        assert!(observe(&mut job, r#"{"event":"done","output":"/tmp/out.wav"}"#));
        assert!(job.done);
        assert_eq!(job.detail.as_deref(), Some("/tmp/out.wav"));
        assert!((job.progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_error_names_the_reason() {
        let mut job = job();
        assert!(observe(&mut job, r#"{"event":"error","message":"torch does not load"}"#));
        assert!(job.done);
        assert_eq!(job.error.as_deref(), Some("torch does not load"));
    }

    #[test]
    fn rvc_chatter_is_logged_rather_than_lost() {
        let mut job = job();
        assert!(!observe(&mut job, "some line from torch"));
        assert_eq!(job.log, vec!["some line from torch".to_string()]);
        assert!(job.stage.is_empty());
    }

    #[test]
    fn a_progress_outside_the_range_is_clamped() {
        let mut job = job();
        observe(&mut job, r#"{"event":"stage","stage":"x","progress":1.7}"#);
        assert_eq!(job.progress, 1.0);
        observe(&mut job, r#"{"event":"stage","stage":"x","progress":-3}"#);
        assert_eq!(job.progress, 0.0);
    }

    #[test]
    fn the_embedded_pin_parses() {
        let pin = VoiceSwap::pin();
        assert_eq!(pin.commit.len(), 40);
        assert!(pin.repository.ends_with(".git"));
        assert!(pin.requirements.starts_with("requirments"));
    }

    #[test]
    fn an_uninstalled_folder_says_what_is_missing() {
        let directory = std::env::temp_dir().join(format!("voice-swap-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let swap = VoiceSwap::new(&directory);
        assert!(!swap.installed());
        let missing = swap.missing();
        assert!(missing.iter().any(|name| name.contains("Python")));
        assert!(missing.iter().any(|name| name == "RVC"));
        assert!(missing.iter().any(|name| name == "pytorch_model"));
        assert!(missing.iter().any(|name| name == "f0G48k"));
    }
}

/// One uploaded take, as the voice keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub name: String,
    pub seconds: f64,
}

/// A voice: the recordings it was trained from, and the checkpoints training
/// left behind. The model files live beside `voice.json`, not inside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voice {
    pub id: String,
    pub name: String,
    pub created: String,
    /// What the last training run was asked for.
    #[serde(default)]
    pub epochs: u32,
    #[serde(default)]
    pub save_every: u32,
    /// The checkpoint a swap uses.
    #[serde(default)]
    pub used: u32,
    /// The epochs a checkpoint was written at.
    #[serde(default)]
    pub checkpoints: Vec<u32>,
    #[serde(default)]
    pub recordings: Vec<Recording>,
    /// What the recordings lasted, kept after they are deleted: a trained
    /// voice keeps the number, not the audio it learned from.
    #[serde(default)]
    pub trained_seconds: f64,
}

impl Voice {
    pub fn minutes(&self) -> f64 {
        let from_recordings: f64 = self.recordings.iter().map(|recording| recording.seconds).sum();
        let seconds = if from_recordings > 0.0 { from_recordings } else { self.trained_seconds };
        (seconds / 60.0).max(0.0)
    }

    /// The model file a swap with this voice loads: `model-e<epoch>.pth`.
    pub fn model(&self) -> Option<std::path::PathBuf> {
        let epoch = if self.used == 0 { *self.checkpoints.last()? } else { self.used };
        Some(std::path::PathBuf::from(format!("model-e{epoch}.pth")))
    }
}

impl VoiceSwap {
    fn voices_dir(&self) -> PathBuf {
        self.root.join("voices")
    }

    pub fn voices(&self) -> Vec<Voice> {
        let Ok(entries) = std::fs::read_dir(self.voices_dir()) else { return Vec::new() };
        let mut voices: Vec<Voice> = entries
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("voice.json")).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        voices.sort_by(|a, b| b.created.cmp(&a.created));
        voices
    }

    pub fn voice(&self, id: &str) -> Result<Voice> {
        if !valid_id(id) {
            bail!("not a voice id");
        }
        let path = self.voices_dir().join(id).join("voice.json");
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    fn save_voice(&self, voice: &Voice) -> Result<()> {
        let directory = self.voices_dir().join(&voice.id);
        std::fs::create_dir_all(directory.join("recordings"))?;
        write_atomic(&directory.join("voice.json"), &serde_json::to_string_pretty(voice)?)?;
        Ok(())
    }

    pub fn create_voice(&self, name: &str) -> Result<Voice> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // A failed upload or a second click must not strand an empty folder
        // that nothing can reach: the draft waiting for training is reused.
        if let Some(mut draft) = self.voices().into_iter().find(|voice| !self.trained(voice)) {
            if draft.recordings.is_empty() && !name.trim().is_empty() {
                draft.name = name.trim().to_string();
                self.save_voice(&draft)?;
            }
            return Ok(draft);
        }
        let stamp = chrono::Utc::now();
        let voice = Voice {
            id: format!("v{}{:04}", stamp.timestamp_millis(), unique_digit()),
            name: name.trim().to_string(),
            created: stamp.to_rfc3339(),
            epochs: 0,
            save_every: 0,
            used: 0,
            checkpoints: Vec::new(),
            recordings: Vec::new(),
            trained_seconds: 0.0,
        };
        self.save_voice(&voice)?;
        Ok(voice)
    }

    pub fn delete_voice(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("not a voice id");
        }
        let directory = self.voices_dir().join(id);
        if !directory.is_dir() {
            bail!("no voice {id}");
        }
        std::fs::remove_dir_all(&directory).with_context(|| format!("remove {}", directory.display()))?;
        Ok(())
    }

    /// Copies one uploaded take into the voice and measures it. The duration is
    /// what the page shows and what decides whether there is enough to train.
    pub fn add_recording(&self, id: &str, name: &str, source: &Path) -> Result<Voice> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut voice = self.voice(id)?;
        if self.trained(&voice) {
            bail!("this voice already has a model; upload into a new draft instead");
        }
        let base = Path::new(name)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("recording.wav")
            .to_string();
        let seconds = crate::audio_pcm::duration_seconds(source).unwrap_or(0.0);
        // A second take with the same name does not replace the first: it gets a
        // number, so a folder of exports does not silently shrink.
        let mut file = base.clone();
        let mut counter = 0;
        while self.voices_dir().join(id).join("recordings").join(&file).exists() {
            counter += 1;
            file = format!("{counter}-{base}");
        }
        let destination = self.voices_dir().join(id).join("recordings").join(&file);
        std::fs::copy(source, &destination).with_context(|| format!("copy into {}", destination.display()))?;
        voice.recordings.push(Recording { name: file, seconds });
        self.save_voice(&voice)?;
        Ok(voice)
    }

    pub fn remove_recording(&self, id: &str, name: &str) -> Result<Voice> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut voice = self.voice(id)?;
        let base = Path::new(name).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
        let before = voice.recordings.len();
        voice.recordings.retain(|recording| recording.name != base);
        if voice.recordings.len() == before {
            bail!("no such recording");
        }
        let _ = std::fs::remove_file(self.voices_dir().join(id).join("recordings").join(&base));
        self.save_voice(&voice)?;
        Ok(voice)
    }

    /// Deletes one saved model. The voice keeps its recordings and the other
    /// checkpoints; the last one is refused, because a voice without a model is
    /// just a draft again and that is a different decision.
    pub fn remove_checkpoint(&self, id: &str, epoch: u32) -> Result<Voice> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut voice = self.voice(id)?;
        if !voice.checkpoints.contains(&epoch) {
            bail!("no checkpoint at epoch {epoch}");
        }
        if voice.checkpoints.len() <= 1 {
            bail!("this is the only checkpoint; delete the voice instead");
        }
        let path = self.voices_dir().join(&voice.id).join(format!("model-e{epoch}.pth"));
        voice.checkpoints.retain(|checkpoint| *checkpoint != epoch);
        if voice.used == epoch {
            voice.used = voice.checkpoints.last().copied().unwrap_or(0);
        }
        self.save_voice(&voice)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone is the state we wanted.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
        }
        Ok(voice)
    }

    /// Keeps the named checkpoints and deletes the rest, in one pass: leaving
    /// forty of them behind is the normal state after a long run.
    pub fn prune_checkpoints(&self, id: &str, keep: &[u32]) -> Result<Voice> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut voice = self.voice(id)?;
        let kept: Vec<u32> = voice.checkpoints.iter().copied().filter(|epoch| keep.contains(epoch)).collect();
        if kept.is_empty() {
            bail!("keep at least one checkpoint");
        }
        for epoch in voice.checkpoints.iter().copied().filter(|epoch| !kept.contains(epoch)) {
            let path = self.voices_dir().join(&voice.id).join(format!("model-e{epoch}.pth"));
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("remove {}", path.display())),
            }
        }
        voice.checkpoints = kept;
        if !voice.checkpoints.contains(&voice.used) {
            voice.used = voice.checkpoints.last().copied().unwrap_or(0);
        }
        self.save_voice(&voice)?;
        Ok(voice)
    }

    pub fn update_voice(
        &self,
        id: &str,
        name: Option<&str>,
        used: Option<u32>,
        epochs: Option<u32>,
        save_every: Option<u32>,
    ) -> Result<Voice> {
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut voice = self.voice(id)?;
        if let Some(name) = name {
            let name = name.trim();
            if !name.is_empty() {
                voice.name = name.to_string();
            }
        }
        if let Some(used) = used {
            if voice.checkpoints.contains(&used) || voice.checkpoints.is_empty() {
                voice.used = used;
            }
        }
        // What the next training would be asked for: kept with the draft so a
        // reload or a page change does not put the defaults back.
        if let Some(epochs) = epochs {
            if epochs > 0 {
                voice.epochs = epochs;
            }
        }
        if let Some(save_every) = save_every {
            if save_every > 0 {
                voice.save_every = save_every;
            }
        }
        self.save_voice(&voice)?;
        Ok(voice)
    }
}


/// A finished conversion: the stem it came from, the voice it was sung in, and
/// the settings it ran with, so the history explains itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Swap {
    pub id: String,
    /// The name the file was uploaded with.
    pub source: String,
    /// Its name inside the swap's folder.
    pub source_file: String,
    pub voice: String,
    pub voice_name: String,
    pub pitch: i32,
    pub index_rate: f32,
    pub protect: f32,
    pub rms_mix: f32,
    pub created: String,
    /// `running`, `done` or `error`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

impl VoiceSwap {
    /// Whether training left a model this voice can be swapped with. An
    /// untrained voice folder is the draft the page shows before training.
    pub fn trained(&self, voice: &Voice) -> bool {
        voice
            .model()
            .is_some_and(|model| self.voices_dir().join(&voice.id).join(model).is_file())
    }

    pub fn swaps_dir(&self) -> PathBuf {
        self.root.join("swaps")
    }

    pub fn swaps(&self) -> Vec<Swap> {
        let Ok(entries) = std::fs::read_dir(self.swaps_dir()) else { return Vec::new() };
        let mut swaps: Vec<Swap> = entries
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path().join("swap.json")).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        swaps.sort_by(|a, b| b.created.cmp(&a.created));
        swaps
    }

    pub fn swap(&self, id: &str) -> Result<Swap> {
        if !valid_id(id) {
            bail!("not a swap id");
        }
        let path = self.swaps_dir().join(id).join("swap.json");
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    fn save_swap(&self, swap: &Swap) -> Result<()> {
        let directory = self.swaps_dir().join(&swap.id);
        std::fs::create_dir_all(&directory)?;
        write_atomic(&directory.join("swap.json"), &serde_json::to_string_pretty(swap)?)?;
        Ok(())
    }

    /// Records a conversion and copies the stem in. The copy is what the job
    /// reads: an upload that ends with its request cannot pull a queued job out
    /// from under itself.
    pub fn create_swap(&self, voice_id: &str, name: &str, source: &Path, pitch: i32, index_rate: f32, protect: f32, rms_mix: f32) -> Result<Swap> {
        let voice = self.voice(voice_id)?;
        if voice.model().is_none() {
            bail!("this voice has no trained model yet");
        }
        let stamp = chrono::Utc::now();
        let id = format!("s{}{:04}", stamp.timestamp_millis(), unique_digit());
        let directory = self.swaps_dir().join(&id);
        std::fs::create_dir_all(&directory)?;
        let extension = Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("wav")
            .to_lowercase();
        let source_file = format!("source.{extension}");
        std::fs::copy(source, directory.join(&source_file))?;
        let swap = Swap {
            id,
            source: name.to_string(),
            source_file,
            voice: voice.id.clone(),
            voice_name: voice.name.clone(),
            pitch,
            index_rate,
            protect,
            rms_mix,
            created: stamp.to_rfc3339(),
            status: "running".into(),
            error: None,
        };
        self.save_swap(&swap)?;
        Ok(swap)
    }

    pub fn delete_swap(&self, id: &str) -> Result<()> {
        if !valid_id(id) {
            bail!("not a swap id");
        }
        let directory = self.swaps_dir().join(id);
        if !directory.is_dir() {
            bail!("no swap {id}");
        }
        std::fs::remove_dir_all(&directory).with_context(|| format!("remove {}", directory.display()))?;
        Ok(())
    }
}

/// Writes a finished job's result back into what the page reads.
fn settle_job(root: &Path, subject: &Subject, failure: Option<&str>) {
    match subject {
        Subject::Train { voice, epochs, save_every } => {
            if failure.is_some() {
                return;
            }
            apply_training(root, voice, *epochs, *save_every);
        }
        Subject::Convert { swap } => finish_swap(root, swap, failure),
        Subject::Engine => {}
    }
}

/// Copies the epochs training left on disk into the voice: what the checkpoint
/// buttons list comes from the files, not from what the runner claimed.
fn apply_training(root: &Path, id: &str, epochs: u32, save_every: u32) {
    let directory = root.join("voices").join(id);
    let mut checkpoints: Vec<u32> = std::fs::read_dir(&directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter_map(|name| Some(name.strip_prefix("model-e")?.strip_suffix(".pth")?.to_string()))
        .filter_map(|epoch| epoch.parse::<u32>().ok())
        .collect();
    checkpoints.sort_unstable();
    let Ok(text) = std::fs::read_to_string(directory.join("voice.json")) else { return };
    let Ok(mut voice) = serde_json::from_str::<Voice>(&text) else { return };
    voice.checkpoints = checkpoints.clone();
    voice.epochs = epochs;
    voice.save_every = save_every;
    voice.used = checkpoints.last().copied().unwrap_or(0);
    // The model is what the recordings were for: the minutes stay, the audio
    // goes. A trained voice cannot be trained again, so nothing could read it.
    let seconds: f64 = voice.recordings.iter().map(|recording| recording.seconds).sum();
    if seconds > 0.0 {
        voice.trained_seconds = seconds;
        voice.recordings.clear();
        let _ = std::fs::remove_dir_all(directory.join("recordings"));
        let _ = std::fs::create_dir_all(directory.join("recordings"));
    }
    if let Ok(text) = serde_json::to_string_pretty(&voice) {
        let _ = write_atomic(&directory.join("voice.json"), &text);
    }
}

fn finish_swap(root: &Path, id: &str, failure: Option<&str>) {
    let path = root.join("swaps").join(id).join("swap.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(mut swap) = serde_json::from_str::<Swap>(&text) else { return };
    match failure {
        None => swap.status = "done".into(),
        Some(message) => {
            swap.status = "error".into();
            swap.error = Some(message.to_string());
        }
    }
    if let Ok(text) = serde_json::to_string_pretty(&swap) {
        let _ = write_atomic(&path, &text);
    }
    // The source was only ever a copy - the user's file, or a stem that still
    // lives in the library - so a finished swap keeps just its result. A failed
    // one keeps the copy until the row is deleted.
    if failure.is_none() {
        let _ = std::fs::remove_file(root.join("swaps").join(id).join(&swap.source_file));
    }
}

#[cfg(test)]
mod guards {
    use super::*;

    #[test]
    fn only_ids_this_tool_made_may_name_a_folder() {
        assert!(valid_id("v1793221957815"));
        assert!(valid_id("s1"));
        assert!(!valid_id(".."));
        assert!(!valid_id("../voices"));
        assert!(!valid_id("v1/../../etc"));
        assert!(!valid_id(""));
        assert!(!valid_id("v"));
        assert!(!valid_id("v12a"));
    }

    #[test]
    fn a_voice_id_that_is_not_ours_is_refused_before_touching_disk() {
        let directory = std::env::temp_dir().join(format!("voice-swap-guard-{}", std::process::id()));
        let swap = VoiceSwap::new(&directory);
        assert!(swap.delete_voice("..").is_err());
        assert!(swap.voice("../rvc").is_err());
        assert!(swap.delete_swap("..").is_err());
        assert!(swap.voice("v123").is_err());
    }
}


#[cfg(test)]
mod training_cleanup {
    use super::*;

    #[test]
    fn a_trained_voice_keeps_the_minutes_and_loses_the_audio() {
        let root = std::env::temp_dir().join(format!("voice-swap-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let directory = root.join("voices").join("v1");
        std::fs::create_dir_all(directory.join("recordings")).unwrap();
        std::fs::write(directory.join("recordings/take.wav"), b"audio").unwrap();
        let voice = Voice {
            id: "v1".into(),
            name: "Mine".into(),
            created: "now".into(),
            epochs: 0,
            save_every: 0,
            used: 0,
            checkpoints: Vec::new(),
            recordings: vec![Recording { name: "take.wav".into(), seconds: 600.0 }],
            trained_seconds: 0.0,
        };
        std::fs::write(directory.join("voice.json"), serde_json::to_string(&voice).unwrap()).unwrap();
        std::fs::write(directory.join("model-e20.pth"), b"weights").unwrap();

        apply_training(&root, "v1", 20, 5);

        let after: Voice = serde_json::from_str(&std::fs::read_to_string(directory.join("voice.json")).unwrap()).unwrap();
        assert!(after.recordings.is_empty(), "the recordings are gone from the record");
        assert!((after.minutes() - 10.0).abs() < 0.01, "the minutes stay: {}", after.minutes());
        assert!(!directory.join("recordings/take.wav").exists(), "the audio is deleted");
        assert_eq!(after.checkpoints, vec![20]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
