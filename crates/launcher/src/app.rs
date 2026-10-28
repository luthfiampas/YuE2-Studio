use crate::config::Config;
use crate::remote::{RemoteTelemetryClient, ResourceSnapshot};
use crate::runpod::{GpuType, NetworkVolume, RunPodClient, RunPodItem, RunPodPort, RunPodRuntime};
use crate::ssh::{REMOTE_PORT, SshAuthDetector, SshAuthMethod, SshRemoteOps, SshTunnel};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum ModalState {
    None,
    ProvisioningVolumeSelect {
        selected_volume_index: usize,
    },
    ProvisioningGpuSelect {
        selected_volume: Option<NetworkVolume>,
        gpus: Vec<GpuType>,
        selected_index: usize,
    },
    ProvisioningPodName {
        selected_volume: Option<NetworkVolume>,
        gpu: GpuType,
        input: Input,
    },
    ConfirmPod {
        pod_id: String,
        pod_name: String,
        action: PodAction,
        selected_choice: usize,
    },
    ConfirmRestartDaemon {
        host: String,
        port: u16,
        selected_choice: usize,
    },
    ConfirmBuild {
        target: BuildTarget,
        selected_choice: usize,
    },
    /// The only dialog where "no" still exits the app.
    ConfirmQuit {
        running_pods: usize,
        selected_choice: usize,
    },
    Settings {
        selected_index: usize,
        editing: bool,
        input: Input,
    },
    Help {
        scroll: usize,
    },
}

impl ModalState {
    /// Resolves a yes/no dialog: `Some(true)` confirms, `Some(false)` cancels
    /// and `None` means the key is not a decision. Every confirmation dialog
    /// answers to the same keys.
    fn choice(&mut self, code: KeyCode) -> Option<bool> {
        let choice = match self {
            ModalState::ConfirmPod {
                selected_choice, ..
            }
            | ModalState::ConfirmRestartDaemon {
                selected_choice, ..
            }
            | ModalState::ConfirmBuild {
                selected_choice, ..
            } => selected_choice,
            _ => return None,
        };
        match code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                *choice = usize::from(*choice == 0);
                None
            }
            KeyCode::Enter => Some(*choice == 0),
            KeyCode::Char('y' | 'Y') => Some(true),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
            _ => None,
        }
    }
}

/// A single-line text field: the value plus a cursor measured in chars, so
/// editing works on text rather than on the bytes under the cursor.
#[derive(Debug, Clone)]
pub struct Input {
    pub value: String,
    pub cursor: usize,
}

impl Input {
    pub fn new(value: String) -> Self {
        Self {
            cursor: value.chars().count(),
            value,
        }
    }

    fn handle_key(&mut self, code: KeyCode) {
        let len = self.value.chars().count();
        match code {
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.value.remove(self.byte_of(self.cursor));
            }
            KeyCode::Delete if self.cursor < len => {
                self.value.remove(self.byte_of(self.cursor));
            }
            KeyCode::Char(c) => {
                self.value.insert(self.byte_of(self.cursor), c);
                self.cursor += 1;
            }
            _ => {}
        }
    }

    fn byte_of(&self, char_index: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_index)
            .map_or(self.value.len(), |(byte, _)| byte)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PodAction {
    Start,
    Stop,
    Terminate,
}

impl PodAction {
    fn busy_message(self, pod_id: &str) -> String {
        match self {
            PodAction::Start => format!("Starting pod {}...", pod_id),
            PodAction::Stop => format!("Stopping pod {} (releasing GPU)...", pod_id),
            PodAction::Terminate => format!("Terminating pod {}...", pod_id),
        }
    }

    fn done_message(self, pod_id: &str) -> String {
        match self {
            PodAction::Start => format!("Start signal sent to pod {}.", pod_id),
            PodAction::Stop => format!("Pod {} stopped. GPU charges paused.", pod_id),
            PodAction::Terminate => format!("Pod {} terminated. Network volume preserved.", pod_id),
        }
    }

    const fn failure(self) -> &'static str {
        match self {
            PodAction::Start => "Failed to start pod",
            PodAction::Stop => "Failed to stop pod",
            PodAction::Terminate => "Failed to terminate pod",
        }
    }

    const fn cancel_message(self) -> &'static str {
        match self {
            PodAction::Start => "Pod start cancelled.",
            PodAction::Stop => "Pod stop cancelled.",
            PodAction::Terminate => "Termination cancelled.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTarget {
    Server,
    Engine,
}

impl BuildTarget {
    const fn cancel_message(self) -> &'static str {
        match self {
            BuildTarget::Server => "Build server cancelled.",
            BuildTarget::Engine => "Build engine cancelled.",
        }
    }
}

#[derive(Debug)]
pub enum AsyncEvent {
    Log(String),
    SetBusy(Option<String>),
    TaskCompleted {
        message: String,
        success: bool,
    },
    PodsUpdated {
        pods: Vec<RunPodItem>,
        volumes: Vec<NetworkVolume>,
        balance: Option<f64>,
    },
    TelemetryUpdated {
        is_healthy: bool,
        resources: Option<ResourceSnapshot>,
    },
    GpuCatalogLoaded {
        volume: Option<NetworkVolume>,
        gpus: Vec<GpuType>,
    },
    WorkspaceUpdated(Option<WorkspaceUsage>),
    RemoteReady,
    Quit,
}

/// Workspace bytes read over SSH, tagged with the pod they came from so a
/// selection change never shows the previous pod's volume.
#[derive(Debug, Clone)]
pub struct WorkspaceUsage {
    pub pod_id: String,
    pub used_mb: u64,
}

pub struct App {
    pub config: Config,
    pub pods: Vec<RunPodItem>,
    pub network_volumes: Vec<NetworkVolume>,
    pub selected_pod_index: usize,
    pub runpod_client: Option<RunPodClient>,
    pub balance: Option<f64>,

    // SSH & Remote
    pub ssh_tunnel: Option<SshTunnel>,
    pub tunnel_status: String,
    /// Whether the ssh child from the last health poll was still alive.
    pub tunnel_alive: bool,
    pub auth_method: SshAuthMethod,
    pub telemetry_client: RemoteTelemetryClient,
    pub last_resources: Option<ResourceSnapshot>,
    pub is_server_healthy: bool,
    pub server_consecutive_failures: u32,
    pub web_ui_process: Option<tokio::process::Child>,
    pub workspace: Option<WorkspaceUsage>,
    last_workspace_poll: std::time::Instant,
    workspace_poll_running: bool,

    // Logs & Spinner Progress
    pub logs: VecDeque<String>,
    pub log_search_query: Option<String>,
    pub spinner_idx: usize,
    pub busy_operation: Option<String>,

    // Navigation & Modals
    pub modal: ModalState,
    pub status_message: String,
    pub should_quit: bool,
    pub tx: UnboundedSender<AsyncEvent>,
}

const SPINNER_CHARS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Row of the theme picker in the settings dialog. Rows before it are text
/// fields, so `apply_setting` and the renderer must agree on this index.
pub const THEME_ROW: usize = 5;

impl App {
    pub fn new(manual_endpoint: Option<(String, u16)>, tx: UnboundedSender<AsyncEvent>) -> Self {
        let config = Config::load();
        crate::theme::set(crate::theme::Theme::from_config(&config));
        let runpod_client = config
            .runpod_api_key
            .as_ref()
            .map(|k| RunPodClient::new(k.clone()));
        let auth_method = SshAuthDetector::detect(config.ssh_key_path.as_deref());
        let telemetry_client = RemoteTelemetryClient::new(config.local_port);

        let pods = manual_endpoint.map(direct_pod).into_iter().collect();

        let mut app = Self {
            config,
            pods,
            network_volumes: Vec::new(),
            selected_pod_index: 0,
            runpod_client,
            balance: None,
            ssh_tunnel: None,
            tunnel_status: "Disconnected".to_string(),
            tunnel_alive: false,
            auth_method,
            telemetry_client,
            last_resources: None,
            is_server_healthy: false,
            server_consecutive_failures: 0,
            web_ui_process: None,
            workspace: None,
            last_workspace_poll: std::time::Instant::now(),
            workspace_poll_running: false,
            logs: VecDeque::with_capacity(250),
            log_search_query: None,
            spinner_idx: 0,
            busy_operation: None,
            modal: ModalState::None,
            status_message: "Ready.".to_string(),
            should_quit: false,
            tx,
        };

        app.log("YuE2 Studio Launcher initialized.");
        app
    }

    pub fn log(&mut self, msg: impl Into<String>) {
        let s = msg.into();
        let now = chrono_free_timestamp();
        let line = format!("[{}] {}", now, s);
        if self.logs.len() >= 200 {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
        self.status_message = s;
    }

    pub fn spinner(&self) -> &'static str {
        SPINNER_CHARS[self.spinner_idx % SPINNER_CHARS.len()]
    }

    pub fn tick_spinner(&mut self) {
        self.spinner_idx = (self.spinner_idx + 1) % SPINNER_CHARS.len();
    }

    /// The RunPod API key, or a logged explanation of why an API call cannot
    /// run. Callers clone it into the task that needs it.
    fn api_key(&mut self) -> Option<String> {
        match self.config.runpod_api_key.as_deref().map(str::trim) {
            Some(key) if !key.is_empty() => Some(key.to_string()),
            _ => {
                self.log("No RunPod API key configured. Press 'c' to add one.");
                None
            }
        }
    }

    pub fn selected_pod(&self) -> Option<&RunPodItem> {
        self.pods.get(self.selected_pod_index)
    }

    pub fn handle_async_event(&mut self, event: AsyncEvent) {
        match event {
            AsyncEvent::Log(msg) => {
                self.log(msg);
            }
            AsyncEvent::SetBusy(busy) => {
                self.busy_operation = busy;
            }
            AsyncEvent::TaskCompleted { message, success } => {
                self.busy_operation = None;
                if success {
                    self.log(format!("✅ {}", message));
                } else {
                    self.log(format!("❌ {}", message));
                }
            }
            AsyncEvent::PodsUpdated {
                pods,
                volumes,
                balance,
            } => {
                self.busy_operation = None;
                self.pods = pods;
                self.network_volumes = volumes;
                if let Some(b) = balance {
                    self.balance = Some(b);
                }
                if self.selected_pod_index >= self.pods.len() && !self.pods.is_empty() {
                    self.selected_pod_index = self.pods.len() - 1;
                }
                if let Some(b) = self.balance {
                    self.log(format!(
                        "Synchronized {} pods and {} network volumes. (Balance: ${:.2})",
                        self.pods.len(),
                        self.network_volumes.len(),
                        b,
                    ));
                } else {
                    self.log(format!(
                        "Synchronized {} pods and {} network volumes.",
                        self.pods.len(),
                        self.network_volumes.len()
                    ));
                }
            }
            AsyncEvent::TelemetryUpdated {
                is_healthy,
                resources,
            } => {
                if is_healthy {
                    self.server_consecutive_failures = 0;
                    let was_healthy = self.is_server_healthy;
                    self.is_server_healthy = true;
                    if !was_healthy {
                        self.log(format!(
                            "⏱️ Studio server connected. Polling :{} every {}s.",
                            self.config.local_port, self.config.server_poll_interval_secs
                        ));
                    }
                    if let Some(r) = resources {
                        self.last_resources = Some(r);
                    }
                } else {
                    self.server_consecutive_failures += 1;
                    // Require 2 consecutive failed health checks before flipping to down
                    if self.server_consecutive_failures >= 2 {
                        self.is_server_healthy = false;
                        self.last_resources = None;
                    }
                }
            }
            AsyncEvent::GpuCatalogLoaded { volume, gpus } => {
                self.busy_operation = None;
                self.modal = ModalState::ProvisioningGpuSelect {
                    selected_volume: volume,
                    gpus,
                    selected_index: 0,
                };
            }
            AsyncEvent::WorkspaceUpdated(usage) => {
                self.workspace_poll_running = false;
                self.workspace = usage;
            }
            AsyncEvent::RemoteReady => {
                self.is_server_healthy = true;
                if self.web_ui_process.is_none() {
                    self.start_web_ui();
                }
            }
            AsyncEvent::Quit => self.should_quit = true,
        }
    }

    pub fn refresh_pods_and_volumes(&mut self) {
        let Some(api_key) = self.api_key() else {
            return;
        };

        let tx = self.tx.clone();

        let _ = tx.send(AsyncEvent::SetBusy(Some(
            "Querying RunPod API for pods & volumes...".into(),
        )));
        tokio::spawn(async move {
            let client = RunPodClient::new(api_key);
            match client.list_pods_and_volumes().await {
                Ok((pods, volumes, balance)) => {
                    let _ = tx.send(AsyncEvent::PodsUpdated {
                        pods,
                        volumes,
                        balance,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("API sync failed: {:#}", e),
                        success: false,
                    });
                }
            }
        });
    }

    pub fn open_provision_wizard(&mut self) {
        if self.runpod_client.is_none() {
            self.log("RunPod API key missing! Cannot provision pods.");
            return;
        }

        self.modal = ModalState::ProvisioningVolumeSelect {
            selected_volume_index: 0,
        };
        self.log("Step 1: Select network volume to determine datacenter region.");
    }

    pub fn on_volume_selected(&mut self, vol_idx: usize) {
        if self.runpod_client.is_none() {
            return;
        }

        let selected_vol = if vol_idx < self.network_volumes.len() {
            Some(self.network_volumes[vol_idx].clone())
        } else {
            None
        };

        let dc_id = selected_vol.as_ref().and_then(|v| v.data_center_id.clone());
        let dc_label = dc_id.clone().unwrap_or_else(|| "Global".to_string());

        let Some(api_key) = self.api_key() else {
            return;
        };
        let tx = self.tx.clone();

        let _ = tx.send(AsyncEvent::SetBusy(Some(format!(
            "Fetching GPUs available in {}...",
            dc_label
        ))));

        tokio::spawn(async move {
            let client = RunPodClient::new(api_key);
            match client.list_gpu_types_for_region(dc_id.as_deref()).await {
                Ok(gpus) => {
                    let in_stock_count = gpus.iter().filter(|g| g.is_in_region_stock()).count();
                    let _ = tx.send(AsyncEvent::Log(format!(
                        "Loaded {} GPUs ({} in stock in {}).",
                        gpus.len(),
                        in_stock_count,
                        dc_label
                    )));
                    let _ = tx.send(AsyncEvent::GpuCatalogLoaded {
                        volume: selected_vol,
                        gpus,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Failed to fetch regional GPUs: {:#}", e),
                        success: false,
                    });
                }
            }
        });
    }

    pub fn confirm_deploy(
        &mut self,
        gpu: GpuType,
        volume: Option<NetworkVolume>,
        custom_name: &str,
    ) {
        if self.runpod_client.is_none() {
            return;
        }
        let vol_name = volume
            .as_ref()
            .map(|v| v.name.clone())
            .unwrap_or_else(|| "Ephemeral".to_string());
        let vol_id = volume.as_ref().map(|v| v.id.clone());

        let Some(api_key) = self.api_key() else {
            return;
        };
        let tx = self.tx.clone();
        let gpu_name = gpu.display_name.clone();

        self.modal = ModalState::None;
        let pod_name = if custom_name.trim().is_empty() {
            default_datetime_pod_name()
        } else {
            custom_name.trim().to_string()
        };
        let _ = tx.send(AsyncEvent::SetBusy(Some(format!(
            "Deploying pod '{}' on {} with volume '{}'...",
            pod_name, gpu_name, vol_name
        ))));

        let gpu_clone = gpu.clone();
        tokio::spawn(async move {
            let client = RunPodClient::new(api_key);
            match client
                .deploy_pod(
                    &pod_name,
                    crate::config::DOCKER_IMAGE,
                    &gpu_clone,
                    vol_id.as_deref(),
                )
                .await
            {
                Ok(pod_id) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("🎉 Pod deployed successfully! ID: {}", pod_id),
                        success: true,
                    });
                    // Refresh inventory
                    if let Ok((pods, volumes, balance)) = client.list_pods_and_volumes().await {
                        let _ = tx.send(AsyncEvent::PodsUpdated {
                            pods,
                            volumes,
                            balance,
                        });
                    }
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Deployment failed: {:#}", e),
                        success: false,
                    });
                }
            }
        });
    }

    /// Asks before the selected pod is started, stopped or terminated.
    fn confirm_pod(&mut self, action: PodAction) {
        if let Some(pod) = self.selected_pod() {
            self.modal = ModalState::ConfirmPod {
                pod_id: pod.id.clone(),
                pod_name: pod.name.clone(),
                action,
                selected_choice: 0,
            };
        }
    }

    /// Start, stop or terminate, then refresh the inventory. All three actions
    /// differ only in which client call they make.
    fn run_pod_action(&mut self, pod_id: String, action: PodAction) {
        let Some(api_key) = self.api_key() else {
            return;
        };
        let tx = self.tx.clone();

        self.modal = ModalState::None;
        let _ = tx.send(AsyncEvent::SetBusy(Some(action.busy_message(&pod_id))));

        tokio::spawn(async move {
            let client = RunPodClient::new(api_key);
            let result = match action {
                PodAction::Start => client.start_pod(&pod_id, 1).await,
                PodAction::Stop => client.stop_pod(&pod_id).await,
                PodAction::Terminate => client.terminate_pod(&pod_id).await,
            };

            match result {
                Ok(()) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: action.done_message(&pod_id),
                        success: true,
                    });
                    if let Ok((pods, volumes, balance)) = client.list_pods_and_volumes().await {
                        let _ = tx.send(AsyncEvent::PodsUpdated {
                            pods,
                            volumes,
                            balance,
                        });
                    }
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("{}: {:#}", action.failure(), e),
                        success: false,
                    });
                }
            }
        });
    }

    pub fn connect_selected_pod_tunnel(&mut self) {
        let Some(pod) = self.selected_pod() else {
            self.log("No pod selected.");
            return;
        };

        let Some((ip, port)) = pod.ssh_connection_info() else {
            self.log(format!(
                "Pod '{}' has no active public SSH endpoint (status: {}).",
                pod.name, pod.desired_status
            ));
            return;
        };

        if let Some(mut existing) = self.ssh_tunnel.take() {
            existing.stop();
        }

        self.log(format!("Establishing SSH tunnel to {}:{}...", ip, port));
        let mut tunnel = SshTunnel::new(
            ip.clone(),
            port,
            self.config.local_port,
            REMOTE_PORT,
            self.auth_method.clone(),
        );

        match tunnel.start(Some(self.tx.clone())) {
            Ok(_) => {
                self.tunnel_status = format!("Connected ({}:{})", ip, port);
                self.tunnel_alive = true;
                self.ssh_tunnel = Some(tunnel);
                self.log(format!(
                    "Tunnel active: localhost:{} -> remote:{}",
                    self.config.local_port, REMOTE_PORT
                ));

                // Verify remote daemon health and auto-launch if uninitialized
                let tx = self.tx.clone();
                let auth = self.auth_method.clone();
                let local_port = self.config.local_port;
                tokio::spawn(async move {
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(2))
                        .build()
                        .unwrap_or_default();
                    let health_url = format!("http://127.0.0.1:{}/health", local_port);

                    // A pod that is already running usually still has its
                    // daemon up; give it time to answer before restarting it,
                    // because the restart kills any job in progress.
                    let mut is_healthy = false;
                    for _ in 0..10 {
                        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                        if let Ok(res) = client.get(&health_url).send().await
                            && res.status().is_success()
                        {
                            is_healthy = true;
                            break;
                        }
                    }

                    if !is_healthy {
                        let _ = tx.send(AsyncEvent::Log(format!("Remote studio server not responding on :{}. Launching runner via SSH...", REMOTE_PORT)));
                        match SshRemoteOps::restart_remote(&ip, port, &auth).await {
                            Ok(msg) => {
                                let _ = tx.send(AsyncEvent::Log(format!(
                                    "Remote runner initialized: {}",
                                    msg
                                )));
                            }
                            Err(e) => {
                                let _ = tx.send(AsyncEvent::Log(format!(
                                    "❌ Failed to launch remote runner: {:#}",
                                    e
                                )));
                                return;
                            }
                        }

                        // Poll for daemon readiness
                        for i in 1..=20 {
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            if let Ok(res) = client.get(&health_url).send().await
                                && res.status().is_success()
                            {
                                is_healthy = true;
                                break;
                            }
                            if i % 5 == 0 {
                                let _ = tx.send(AsyncEvent::Log(format!(
                                    "Waiting for remote studio server ({}/20s)...",
                                    i
                                )));
                            }
                        }
                    }

                    if is_healthy {
                        let _ = tx.send(AsyncEvent::Log(format!(
                            "🎉 Remote studio server is ready on http://127.0.0.1:{}!",
                            local_port
                        )));
                        let _ = tx.send(AsyncEvent::RemoteReady);
                    } else {
                        let _ = tx.send(AsyncEvent::Log(format!("❌ Studio server failed to respond on :{} within 20s. Press 'R' to retry launch or check pod logs.", local_port)));
                    }
                });
            }
            Err(e) => {
                self.tunnel_status = "Connection Failed".to_string();
                self.log(format!("Failed to open tunnel: {:#}", e));
            }
        }
    }

    pub fn disconnect_tunnel(&mut self) {
        if let Some(mut tunnel) = self.ssh_tunnel.take() {
            tunnel.stop();
            self.tunnel_status = "Disconnected".to_string();
            self.tunnel_alive = false;
            self.log("SSH tunnel closed.");
        }
        self.stop_web_ui();
        self.last_resources = None;
        self.is_server_healthy = false;
        self.server_consecutive_failures = 0;
    }

    pub fn start_web_ui(&mut self) {
        if self.web_ui_process.is_some() {
            self.log("Local Web UI is already running on http://localhost:3791.");
            return;
        }

        let app_dir = crate::repo_root().join("app");

        self.log("Starting local Vite web UI (npm run dev)...");
        let mut cmd = tokio::process::Command::new("npm");
        cmd.arg("run")
            .arg("dev")
            .current_dir(app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        match cmd.spawn() {
            Ok(mut child) => {
                let tx = self.tx.clone();
                if let Some(stdout) = child.stdout.take() {
                    tokio::spawn(async move {
                        let reader = BufReader::new(stdout);
                        let mut lines = reader.lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let trim = line.trim();
                            if trim.contains("Local:") || trim.contains("ready in") {
                                let _ = tx.send(AsyncEvent::Log(format!("Vite: {}", trim)));
                            }
                        }
                    });
                }
                self.web_ui_process = Some(child);
                self.log("🚀 Web UI active: http://localhost:3791 (Press 'o' to open in browser)");
            }
            Err(e) => {
                self.log(format!("❌ Failed to launch npm run dev: {:#}", e));
            }
        }
    }

    pub fn stop_web_ui(&mut self) {
        if let Some(mut child) = self.web_ui_process.take() {
            let _ = child.start_kill();
            self.log("Local Web UI stopped.");
        }
    }

    pub fn toggle_web_ui(&mut self) {
        if self.web_ui_process.is_some() {
            self.stop_web_ui();
        } else {
            self.start_web_ui();
        }
    }

    pub fn push_selected_pod(&mut self) {
        if self.tunnel_alive {
            self.log("⚠️ Cannot push binaries while connected to pod. Please disconnect tunnel ([Enter] or [d]) first.");
            return;
        }

        let Some(pod) = self.selected_pod() else {
            self.log("No pod selected.");
            return;
        };
        let Some((ip, port)) = pod.ssh_connection_info() else {
            self.log("Selected pod has no active SSH endpoint.");
            return;
        };

        let auth = self.auth_method.clone();
        let tx = self.tx.clone();

        let _ = tx.send(AsyncEvent::SetBusy(Some(format!(
            "Uploading binaries to {}:{}...",
            ip, port
        ))));
        let _ = tx.send(AsyncEvent::Log(format!(
            "Starting SCP upload of binaries to root@{}:{}...",
            ip, port
        )));

        tokio::spawn(async move {
            match SshRemoteOps::push_and_restart(&ip, port, &auth, Some(&tx)).await {
                Ok(out) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Push succeeded! Remote output: {}", out),
                        success: true,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Push failed: {:#}", e),
                        success: false,
                    });
                }
            }
        });
    }

    pub fn restart_remote_daemon_direct(&mut self, ip: &str, port: u16) {
        let auth = self.auth_method.clone();
        let tx = self.tx.clone();
        let host = ip.to_string();

        let _ = tx.send(AsyncEvent::SetBusy(Some(format!(
            "Restarting studio daemon on {}:{}...",
            host, port
        ))));
        tokio::spawn(async move {
            match SshRemoteOps::restart_remote(&host, port, &auth).await {
                Ok(out) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Daemon restarted: {}", out),
                        success: true,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Restart failed: {:#}", e),
                        success: false,
                    });
                }
            }
        });
    }

    /// Runs one of the `runpod/` build scripts, streaming its output into the
    /// console and reporting the exit status.
    fn run_script(&self, script: PathBuf, args: Vec<std::ffi::OsString>, busy: &str, done: &str) {
        let tx = self.tx.clone();
        let done = done.to_string();
        let _ = tx.send(AsyncEvent::SetBusy(Some(busy.to_string())));
        let _ = tx.send(AsyncEvent::Log(format!("Running {}", script.display())));

        tokio::spawn(async move {
            let mut cmd = tokio::process::Command::new("bash");
            cmd.arg(&script)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => {
                    let _ = tx.send(AsyncEvent::TaskCompleted {
                        message: format!("Failed to spawn {}: {:#}", script.display(), e),
                        success: false,
                    });
                    return;
                }
            };

            if let Some(stdout) = child.stdout.take() {
                forward_output(stdout, tx.clone());
            }
            if let Some(stderr) = child.stderr.take() {
                forward_output(stderr, tx.clone());
            }

            let (message, success) = match child.wait().await {
                Ok(status) if status.success() => (done, true),
                Ok(status) => (
                    format!("Build exited with status code: {:?}", status.code()),
                    false,
                ),
                Err(e) => (format!("Build error: {:#}", e), false),
            };
            let _ = tx.send(AsyncEvent::TaskCompleted { message, success });
        });
    }

    pub fn build_server_locally(&mut self) {
        self.run_script(
            crate::repo_root().join("runpod").join("build-server.sh"),
            Vec::new(),
            "Building headless music-server via Docker...",
            "Headless music-server ready. Press 'p' to push to the active pod.",
        );
    }

    pub fn build_engine_locally(&mut self) {
        let output = crate::repo_root()
            .ancestors()
            .nth(1)
            .map(|root| root.join("build-cache").join("out-engine-all"))
            .unwrap_or_else(|| PathBuf::from("build-cache/out-engine-all"));

        self.run_script(
            crate::repo_root().join("runpod").join("build-engine.sh"),
            vec![output.into_os_string()],
            "Building multi-arch CUDA yue-server via Docker...",
            "CUDA yue-server engine built. Press 'p' to package and push.",
        );
    }

    /// Ids of the pods that are billing right now.
    fn running_pod_ids(&self) -> Vec<String> {
        self.pods
            .iter()
            .filter(|pod| pod.desired_status.eq_ignore_ascii_case("RUNNING"))
            .map(|pod| pod.id.clone())
            .collect()
    }

    /// Quits immediately, unless a pod is still billing — then it asks.
    fn request_quit(&mut self) {
        let running = self.running_pod_ids().len();
        if running == 0 {
            self.should_quit = true;
        } else {
            self.modal = ModalState::ConfirmQuit {
                running_pods: running,
                selected_choice: 0,
            };
        }
    }

    /// Stops every billing pod, then closes the launcher. The quit event is
    /// only sent after the last stop returns, so the runtime cannot cancel
    /// the calls by tearing down the tasks first.
    fn stop_running_pods_then_quit(&mut self) {
        self.modal = ModalState::None;
        let ids = self.running_pod_ids();
        let Some(api_key) = self.api_key() else {
            // The key is what stops billing; still honour the quit.
            self.should_quit = true;
            return;
        };

        let tx = self.tx.clone();
        let _ = tx.send(AsyncEvent::SetBusy(Some(format!(
            "Stopping {} pod(s) before quitting...",
            ids.len()
        ))));
        tokio::spawn(async move {
            let client = RunPodClient::new(api_key);
            for id in &ids {
                if let Err(e) = client.stop_pod(id).await {
                    let _ = tx.send(AsyncEvent::Log(format!(
                        "❌ Failed to stop {}: {:#}",
                        id, e
                    )));
                }
            }
            let _ = tx.send(AsyncEvent::Quit);
        });
    }

    pub fn poll_remote_status(&mut self) {
        if let Some(ref mut tunnel) = self.ssh_tunnel {
            let alive = tunnel.is_running();
            self.tunnel_alive = alive;
            if !alive {
                let err = tunnel.last_error.as_deref().unwrap_or("Process Exited");
                self.tunnel_status = format!("Disconnected ({})", err);
            }
        } else {
            self.tunnel_alive = false;
        }

        let port = self.config.local_port;
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let client = RemoteTelemetryClient::new(port);
            let is_healthy = client.check_health().await;
            let mut resources = None;

            if is_healthy && let Ok(res) = client.get_resources().await {
                resources = Some(res);
            }

            let _ = tx.send(AsyncEvent::TelemetryUpdated {
                is_healthy,
                resources,
            });
        });
    }

    /// Reads the selected pod's `/workspace` usage over SSH. `du` walks the
    /// volume, so this runs every five minutes and never overlaps itself.
    pub fn poll_workspace_usage(&mut self) {
        const INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

        if self.workspace_poll_running {
            return;
        }
        let Some(pod) = self.selected_pod() else {
            return;
        };
        let (pod_id, endpoint) = (pod.id.clone(), pod.ssh_connection_info());
        let selected_is_new = self
            .workspace
            .as_ref()
            .is_none_or(|usage| usage.pod_id != pod_id);
        if !selected_is_new && self.last_workspace_poll.elapsed() < INTERVAL {
            return;
        }

        let Some((ip, port)) = endpoint else {
            // Without an SSH endpoint the volume cannot be measured, so the
            // last reading is dropped rather than left looking current.
            self.workspace = None;
            return;
        };
        self.last_workspace_poll = std::time::Instant::now();
        self.workspace_poll_running = true;

        let auth = self.auth_method.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let usage = SshRemoteOps::workspace_usage(&ip, port, &auth)
                .await
                .ok()
                .map(|used_mb| WorkspaceUsage { pod_id, used_mb });
            let _ = tx.send(AsyncEvent::WorkspaceUpdated(usage));
        });
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            // Second Ctrl-C gets out even if the quit dialog is up.
            if matches!(self.modal, ModalState::ConfirmQuit { .. }) {
                self.should_quit = true;
            } else {
                self.request_quit();
            }
            return;
        }

        // Active log search input handler
        if let Some(ref mut query) = self.log_search_query {
            match key.code {
                KeyCode::Esc | KeyCode::Enter if query.is_empty() => self.log_search_query = None,
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c) => query.push(c),
                _ => {}
            }
            return;
        }

        // The quit dialog is the only one where "no" still exits, so it does
        // not go through `choice`.
        if let ModalState::ConfirmQuit {
            selected_choice, ..
        } = &mut self.modal
        {
            let choice = match key.code {
                KeyCode::Esc => {
                    self.modal = ModalState::None;
                    return;
                }
                KeyCode::Char('n' | 'N') => Some(1),
                KeyCode::Char('y' | 'Y') => Some(0),
                KeyCode::Enter => Some(*selected_choice),
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    *selected_choice = usize::from(*selected_choice == 0);
                    None
                }
                _ => None,
            };
            match choice {
                Some(0) => self.stop_running_pods_then_quit(),
                Some(_) => self.should_quit = true,
                None => {}
            }
            return;
        }

        // Every yes/no dialog answers to the same keys.
        if let Some(confirmed) = self.modal.choice(key.code) {
            let modal = std::mem::replace(&mut self.modal, ModalState::None);
            self.resolve_confirmation(modal, confirmed);
            return;
        }

        // Handle active modals
        match &mut self.modal {
            ModalState::ProvisioningVolumeSelect {
                selected_volume_index,
            } => {
                let options_count = self.network_volumes.len() + 1; // +1 for "No Volume"
                match key.code {
                    KeyCode::Esc => {
                        self.modal = ModalState::None;
                        self.log("Provisioning cancelled.");
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *selected_volume_index + 1 < options_count {
                            *selected_volume_index += 1;
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *selected_volume_index > 0 {
                            *selected_volume_index -= 1;
                        }
                    }
                    KeyCode::Enter => {
                        let idx = *selected_volume_index;
                        self.on_volume_selected(idx);
                    }
                    KeyCode::Char('r') => {
                        self.refresh_pods_and_volumes();
                    }
                    _ => {}
                }
                return;
            }
            ModalState::ProvisioningGpuSelect {
                selected_volume,
                gpus,
                selected_index,
            } => {
                let vol = selected_volume.clone();
                match key.code {
                    KeyCode::Esc => {
                        // Return to volume selection step
                        let vol_idx = volume_index(&self.network_volumes, vol.as_ref());
                        self.modal = ModalState::ProvisioningVolumeSelect {
                            selected_volume_index: vol_idx,
                        };
                    }
                    KeyCode::Char('r') => {
                        let vol_idx = volume_index(&self.network_volumes, vol.as_ref());
                        self.on_volume_selected(vol_idx);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !gpus.is_empty() && *selected_index + 1 < gpus.len() {
                            *selected_index += 1;
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *selected_index > 0 {
                            *selected_index -= 1;
                        }
                    }
                    KeyCode::PageDown => {
                        *selected_index = (*selected_index + 10).min(gpus.len().saturating_sub(1));
                    }
                    KeyCode::PageUp => {
                        *selected_index = selected_index.saturating_sub(10);
                    }
                    KeyCode::Enter => {
                        if let Some(gpu) = gpus.get(*selected_index).cloned() {
                            if vol.is_some() && !gpu.is_in_region_stock() {
                                let dc_name = vol
                                    .as_ref()
                                    .and_then(|v| v.data_center_id.as_deref())
                                    .unwrap_or("this region");
                                self.log(format!(
                                    "⚠️ {} has no instances in {}. Select an [IN STOCK 🟢] GPU.",
                                    gpu.display_name, dc_name
                                ));
                                return;
                            }
                            self.modal = ModalState::ProvisioningPodName {
                                selected_volume: vol,
                                gpu,
                                input: Input::new(default_datetime_pod_name()),
                            };
                        }
                    }
                    _ => {}
                }
                return;
            }
            ModalState::ProvisioningPodName {
                selected_volume,
                gpu,
                input,
            } => {
                match key.code {
                    KeyCode::Esc => {
                        // Return to GPU select step
                        let vol_idx = volume_index(&self.network_volumes, selected_volume.as_ref());
                        self.on_volume_selected(vol_idx);
                    }
                    KeyCode::Enter => {
                        let vol = selected_volume.clone();
                        let gpu = gpu.clone();
                        let name = input.value.clone();
                        self.confirm_deploy(gpu, vol, &name);
                    }
                    code => input.handle_key(code),
                }
                return;
            }
            ModalState::ConfirmPod { .. }
            | ModalState::ConfirmRestartDaemon { .. }
            | ModalState::ConfirmQuit { .. }
            | ModalState::ConfirmBuild { .. } => {
                // Arrows, Enter, y/n and Esc are already handled by `choice`.
            }
            ModalState::Help { scroll } => {
                match key.code {
                    KeyCode::Esc
                    | KeyCode::Enter
                    | KeyCode::Char('q')
                    | KeyCode::Char('?')
                    | KeyCode::Char('h') => {
                        self.modal = ModalState::None;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *scroll + 1 < 16 {
                            *scroll += 1;
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *scroll = scroll.saturating_sub(1);
                    }
                    _ => {}
                }
                return;
            }
            ModalState::Settings {
                selected_index,
                editing,
                input,
            } => {
                if *editing {
                    match key.code {
                        KeyCode::Esc => *editing = false,
                        KeyCode::Enter => {
                            let index = *selected_index;
                            let value = input.value.trim().to_string();
                            *editing = false;
                            self.apply_setting(index, value);
                            self.log("Settings updated & saved.");
                        }
                        code => input.handle_key(code),
                    }
                } else {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            self.modal = ModalState::None;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if *selected_index < THEME_ROW {
                                *selected_index += 1;
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if *selected_index > 0 {
                                *selected_index -= 1;
                            }
                        }
                        KeyCode::Left | KeyCode::Right if *selected_index == THEME_ROW => {
                            let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                            self.cycle_theme(delta);
                        }
                        KeyCode::Enter | KeyCode::Char('e') => {
                            if *selected_index == THEME_ROW {
                                self.cycle_theme(1);
                                return;
                            }
                            *editing = true;
                            let value = match *selected_index {
                                0 => self.config.runpod_api_key.clone().unwrap_or_default(),
                                1 => self.config.local_port.to_string(),
                                2 => self.config.server_poll_interval_secs.to_string(),
                                3 => self.config.pod_refresh_interval_secs.to_string(),
                                4 => self
                                    .config
                                    .ssh_key_path
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default(),
                                _ => self.config.theme.clone(),
                            };
                            *input = Input::new(value);
                        }
                        _ => {}
                    }
                }
                return;
            }
            ModalState::None => {}
        }

        // Main Dashboard Controls
        match key.code {
            KeyCode::Char('q') => {
                self.request_quit();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.pods.is_empty() && self.selected_pod_index + 1 < self.pods.len() {
                    self.selected_pod_index += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_pod_index > 0 {
                    self.selected_pod_index -= 1;
                }
            }
            KeyCode::Enter => {
                if self.ssh_tunnel.is_some() {
                    self.disconnect_tunnel();
                } else {
                    self.connect_selected_pod_tunnel();
                }
            }
            KeyCode::Char('n') => {
                self.open_provision_wizard();
            }
            KeyCode::Char('s') => self.confirm_pod(PodAction::Start),
            KeyCode::Char('x') => self.confirm_pod(PodAction::Stop),
            KeyCode::Char('t') | KeyCode::Char('T') => self.confirm_pod(PodAction::Terminate),
            KeyCode::Char('b') => {
                self.modal = ModalState::ConfirmBuild {
                    target: BuildTarget::Server,
                    selected_choice: 0,
                };
            }
            KeyCode::Char('e') => {
                self.modal = ModalState::ConfirmBuild {
                    target: BuildTarget::Engine,
                    selected_choice: 0,
                };
            }
            KeyCode::Char('p') => {
                self.push_selected_pod();
            }
            KeyCode::Char('d') | KeyCode::Char('R') => {
                if let Some(pod) = self.selected_pod() {
                    if let Some((ip, port)) = pod.ssh_connection_info() {
                        self.modal = ModalState::ConfirmRestartDaemon {
                            host: ip,
                            port,
                            selected_choice: 0,
                        };
                    } else {
                        self.log("Selected pod has no active SSH endpoint.");
                    }
                }
            }
            KeyCode::Char('r') => {
                self.refresh_pods_and_volumes();
            }
            KeyCode::Char('w') => {
                self.toggle_web_ui();
            }
            KeyCode::Char('o') => {
                if self.web_ui_process.is_none() {
                    self.start_web_ui();
                }
                let url = "http://localhost:3791";
                open_browser(url);
                self.log(format!("Opening browser to {}", url));
            }
            KeyCode::Char('c') | KeyCode::Char('S') => {
                self.modal = ModalState::Settings {
                    selected_index: 0,
                    editing: false,
                    input: Input::new(String::new()),
                };
            }
            KeyCode::Char('?') | KeyCode::Char('h') => {
                self.modal = ModalState::Help { scroll: 0 };
            }
            KeyCode::Char('/') => {
                self.log_search_query = Some(String::new());
            }
            KeyCode::Esc => {
                self.log_search_query = None;
            }
            _ => {}
        }
    }

    /// Applies a decision from a yes/no dialog that the caller already took
    /// out of `self.modal`.
    fn resolve_confirmation(&mut self, modal: ModalState, confirmed: bool) {
        match modal {
            ModalState::ConfirmPod { pod_id, action, .. } => {
                if confirmed {
                    self.run_pod_action(pod_id, action);
                } else {
                    self.log(action.cancel_message());
                }
            }
            ModalState::ConfirmRestartDaemon { host, port, .. } => {
                if confirmed {
                    self.restart_remote_daemon_direct(&host, port);
                } else {
                    self.log("Daemon restart cancelled.");
                }
            }
            ModalState::ConfirmBuild { target, .. } => {
                if confirmed {
                    match target {
                        BuildTarget::Server => self.build_server_locally(),
                        BuildTarget::Engine => self.build_engine_locally(),
                    }
                } else {
                    self.log(target.cancel_message());
                }
            }
            _ => {}
        }
    }

    /// Writes one row of the settings dialog back into the config.
    fn apply_setting(&mut self, index: usize, value: String) {
        match index {
            0 => {
                self.config.runpod_api_key = if value.is_empty() { None } else { Some(value) };
                self.runpod_client = self
                    .config
                    .runpod_api_key
                    .as_ref()
                    .map(|k| RunPodClient::new(k.clone()));
            }
            1 => {
                if let Ok(port) = value.parse::<u16>() {
                    self.config.local_port = port;
                    self.telemetry_client = RemoteTelemetryClient::new(port);
                }
            }
            2 => {
                if let Ok(secs) = value.parse::<u64>() {
                    self.config.server_poll_interval_secs = secs;
                }
            }
            3 => {
                if let Ok(secs) = value.parse::<u64>() {
                    self.config.pod_refresh_interval_secs = secs;
                }
            }
            4 => {
                self.config.ssh_key_path = if value.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(value))
                };
                self.auth_method = SshAuthDetector::detect(self.config.ssh_key_path.as_deref());
            }
            _ => {
                let name = value.to_ascii_lowercase();
                if crate::theme::Theme::preset(&name).is_some() {
                    self.config.theme = name;
                } else {
                    self.log(format!(
                        "Unknown theme '{}'. Presets: {}.",
                        value,
                        crate::theme::PRESETS.join(", ")
                    ));
                }
            }
        }
        let _ = self.config.save();
        crate::theme::set(crate::theme::Theme::from_config(&self.config));
    }

    fn cycle_theme(&mut self, delta: isize) {
        self.config.theme = crate::theme::cycle(&self.config.theme, delta).to_string();
        crate::theme::set(crate::theme::Theme::from_config(&self.config));
        let _ = self.config.save();
    }
}

/// Index of a volume in the list, or the ephemeral entry that follows them.
fn volume_index(volumes: &[NetworkVolume], volume: Option<&NetworkVolume>) -> usize {
    volume
        .and_then(|v| volumes.iter().position(|nv| nv.id == v.id))
        .unwrap_or(volumes.len())
}

/// The pseudo-pod that points the launcher at an already reachable host.
fn direct_pod((host, port): (String, u16)) -> RunPodItem {
    RunPodItem {
        id: "manual-pod".to_string(),
        name: format!("Direct Pod ({}:{})", host, port),
        desired_status: "RUNNING".to_string(),
        cost_per_hr: None,
        runtime: Some(RunPodRuntime {
            uptime_in_seconds: None,
            ports: Some(vec![RunPodPort {
                ip: Some(host),
                private_port: Some(22),
                public_port: Some(port),
            }]),
        }),
        machine: None,
        network_volume: None,
    }
}

fn chrono_free_timestamp() -> String {
    let now = chrono::Local::now();
    now.format("%H:%M:%S").to_string()
}

pub fn default_datetime_pod_name() -> String {
    let now = chrono::Local::now();
    now.format("%Y-%m-%d-%H%M%S").to_string()
}

/// Streams a child's output into the console one line at a time.
fn forward_output<R>(stream: R, tx: UnboundedSender<AsyncEvent>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                let _ = tx.send(AsyncEvent::Log(line));
            }
        }
    });
}

fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut tunnel) = self.ssh_tunnel.take() {
            tunnel.stop();
        }
        if let Some(mut child) = self.web_ui_process.take() {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_edits_at_the_cursor() {
        let mut input = Input::new("abc".to_string());
        assert_eq!(input.cursor, 3);

        input.handle_key(KeyCode::Home);
        input.handle_key(KeyCode::Char('X'));
        assert_eq!(input.value, "Xabc");
        assert_eq!(input.cursor, 1);

        input.handle_key(KeyCode::Delete);
        assert_eq!(input.value, "Xbc");
        input.handle_key(KeyCode::Backspace);
        assert_eq!(input.value, "bc");
        assert_eq!(input.cursor, 0);

        // Backspace at the start and Delete at the end do nothing.
        input.handle_key(KeyCode::Backspace);
        input.handle_key(KeyCode::End);
        input.handle_key(KeyCode::Delete);
        assert_eq!(input.value, "bc");
    }

    #[test]
    fn input_cursor_counts_chars_not_bytes() {
        let mut input = Input::new("é日".to_string());
        input.handle_key(KeyCode::Left);
        input.handle_key(KeyCode::Char('x'));
        assert_eq!(input.value, "éx日");
        assert_eq!(input.cursor, 2);
    }
}
