use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;

pub const REMOTE_RUNNER_SCRIPT: &str = include_str!("../../../runpod/remote-runner.sh");

/// The studio's directory on the pod's Network Volume. Must match `STUDIO_DIR`
/// in `runpod/remote-runner.sh`, which sets the matching paths for the daemon.
const REMOTE_STUDIO_DIR: &str = "/workspace/yue2-studio";

/// Upload staging path, kept outside `REMOTE_STUDIO_DIR` so extraction cannot
/// pick the archive up as part of the bundle.
const REMOTE_ARCHIVE: &str = "/workspace/yue2-studio-bin.tar.gz";

/// Port the studio daemon listens on inside the pod. Fixed: the browser and
/// the health check reach it through the tunnel's local port.
pub const REMOTE_PORT: u16 = 8791;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshAuthMethod {
    Agent(PathBuf),
    IdentityFile(PathBuf),
    SystemDefault,
}

pub struct SshAuthDetector;

impl SshAuthDetector {
    /// Detects authentication methods in order of priority:
    /// 1. Configured explicit identity file
    /// 2. Active SSH Agent via `SSH_AUTH_SOCK`
    /// 3. Default standard identity files in ~/.ssh/
    /// 4. Fallback to system default ssh invocation
    pub fn detect(explicit_key: Option<&Path>) -> SshAuthMethod {
        if let Some(key) = explicit_key
            && key.exists()
        {
            return SshAuthMethod::IdentityFile(key.to_path_buf());
        }

        if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
            let p = PathBuf::from(sock);
            if p.exists() {
                return SshAuthMethod::Agent(p);
            }
        }

        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let ssh_dir = Path::new(&home).join(".ssh");
        for candidate in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let key_path = ssh_dir.join(candidate);
            if key_path.exists() {
                return SshAuthMethod::IdentityFile(key_path);
            }
        }

        SshAuthMethod::SystemDefault
    }
}

pub struct SshRemoteOps;

/// Adds the identity file when the detected auth method needs one.
fn apply_auth(cmd: &mut Command, auth: &SshAuthMethod) {
    if let SshAuthMethod::IdentityFile(key) = auth {
        cmd.arg("-i").arg(key);
    }
}

/// Every ssh/scp call to a pod shares these options. The port flag is `-p` for
/// ssh and `-P` for scp, so the program name picks it.
fn pod_command(program: &str, port: u16, auth: &SshAuthMethod) -> Command {
    let mut cmd = Command::new(program);
    if program == "scp" {
        cmd.arg("-q").arg("-C");
    }
    cmd.arg(if program == "scp" { "-P" } else { "-p" })
        .arg(port.to_string())
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_auth(&mut cmd, auth);
    cmd
}

impl SshRemoteOps {
    /// Checks whether the CUDA engine (yue-server) already exists on the remote pod.
    pub async fn check_engine_exists(
        host: &str,
        ssh_port: u16,
        auth: &SshAuthMethod,
    ) -> bool {
        let mut cmd = pod_command("ssh", ssh_port, auth);
        cmd.arg(format!("root@{}", host))
            .arg(format!(
                "test -f {}/bin/yue-server",
                REMOTE_STUDIO_DIR
            ));
        cmd.status().await.map(|s| s.success()).unwrap_or(false)
    }

    /// Uploads binaries to the remote pod and restarts the studio service.
    ///
    /// If `full_bundle` is false and the remote CUDA engine exists, performs a fast
    /// push of the `music-server` binary only. Otherwise, packages and pushes the full
    /// archive including both the engine and server.
    pub async fn push_and_restart(
        host: &str,
        ssh_port: u16,
        auth: &SshAuthMethod,
        tx: Option<&tokio::sync::mpsc::UnboundedSender<crate::app::AsyncEvent>>,
        full_bundle: bool,
    ) -> Result<String> {
        let repo_root = crate::repo_root();
        let package_script = repo_root.join("runpod").join("package.sh");
        let archive = repo_root
            .join("runpod")
            .join("dist")
            .join("yue2-studio-runpod.tar.gz");
        let server_bin = repo_root
            .join("runpod")
            .join("dist")
            .join("bin")
            .join("music-server");
        let target_server = repo_root
            .join("target")
            .join("release")
            .join("music-server");

        // Sync local release build into runpod/dist/bin/music-server if newer
        if target_server.exists() {
            let update_needed = if !server_bin.exists() {
                true
            } else {
                let target_time = std::fs::metadata(&target_server).and_then(|m| m.modified()).ok();
                let server_time = std::fs::metadata(&server_bin).and_then(|m| m.modified()).ok();
                match (target_time, server_time) {
                    (Some(t), Some(s)) => t > s,
                    _ => false,
                }
            };
            if update_needed {
                if let Some(parent) = server_bin.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&target_server, &server_bin);
            }
        }

        let use_fast_push = if !full_bundle {
            if let Some(t) = tx {
                let _ = t.send(crate::app::AsyncEvent::SetBusy(Some(
                    "Checking engine binaries on remote pod...".into(),
                )));
                let _ = t.send(crate::app::AsyncEvent::Log(
                    "🔍 Checking for existing CUDA engine binaries on pod...".into(),
                ));
            }
            let engine_on_pod = Self::check_engine_exists(host, ssh_port, auth).await;
            if engine_on_pod {
                if let Some(t) = tx {
                    let _ = t.send(crate::app::AsyncEvent::Log(
                        "⚡ CUDA engine found on pod. Using fast push (music-server only)...".into(),
                    ));
                }
                true
            } else {
                if let Some(t) = tx {
                    let _ = t.send(crate::app::AsyncEvent::Log(
                        "⚠️ CUDA engine missing on pod. Falling back to full bundle push...".into(),
                    ));
                }
                false
            }
        } else {
            if let Some(t) = tx {
                let _ = t.send(crate::app::AsyncEvent::Log(
                    "📦 Full bundle push requested (music-server + CUDA engine)...".into(),
                ));
            }
            false
        };

        if use_fast_push {
            if !server_bin.exists() {
                bail!("music-server binary not found at runpod/dist/bin/music-server. Run [b] to compile first.");
            }

            if let Some(t) = tx {
                let _ = t.send(crate::app::AsyncEvent::SetBusy(Some(
                    "Uploading music-server via compressed SCP...".into(),
                )));
                let _ = t.send(crate::app::AsyncEvent::Log(
                    "🚀 Uploading music-server binary with SCP compression (-C)...".into(),
                ));
            }

            // Ensure bin directory exists on remote pod
            let mut mkdir_cmd = pod_command("ssh", ssh_port, auth);
            mkdir_cmd.arg(format!("root@{}", host)).arg(format!(
                "mkdir -p {}/bin",
                REMOTE_STUDIO_DIR
            ));
            let _ = mkdir_cmd.output().await;

            let remote_tmp_bin = format!("{}/bin/music-server.tmp", REMOTE_STUDIO_DIR);
            let remote_target_bin = format!("{}/bin/music-server", REMOTE_STUDIO_DIR);

            let mut scp = pod_command("scp", ssh_port, auth);
            scp.arg(&server_bin)
                .arg(format!("root@{}:{}", host, remote_tmp_bin));

            let scp_output = scp
                .output()
                .await
                .context("Failed to run scp for music-server")?;
            if !scp_output.status.success() {
                let err = String::from_utf8_lossy(&scp_output.stderr);
                bail!("SCP failed to upload music-server to pod: {}", err.trim());
            }

            // Atomically replace and mark executable
            let mut finalize_cmd = pod_command("ssh", ssh_port, auth);
            finalize_cmd.arg(format!("root@{}", host)).arg(format!(
                "mv -f {} {} && chmod +x {}",
                remote_tmp_bin, remote_target_bin, remote_target_bin
            ));

            let fin_output = finalize_cmd
                .output()
                .await
                .context("Failed to finalize music-server on remote pod")?;
            if !fin_output.status.success() {
                let err = String::from_utf8_lossy(&fin_output.stderr);
                bail!("Failed to activate music-server on remote pod: {}", err.trim());
            }
        } else {
            if package_script.exists() {
                if let Some(t) = tx {
                    let _ = t.send(crate::app::AsyncEvent::SetBusy(Some(
                        "Packaging latest binaries into distribution archive...".into(),
                    )));
                    let _ = t.send(crate::app::AsyncEvent::Log(
                        "📦 Packaging full distribution bundle via package.sh...".into(),
                    ));
                }
                let packaged = Command::new("bash")
                    .arg(&package_script)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await
                    .context("Failed to run package.sh")?;
                if !packaged.status.success() {
                    let err = String::from_utf8_lossy(&packaged.stderr);
                    let out = String::from_utf8_lossy(&packaged.stdout);
                    bail!("package.sh failed, nothing was pushed: {}\n{}", out.trim(), err.trim());
                }
            }

            if !archive.exists() {
                bail!("Archive not found at runpod/dist/yue2-studio-runpod.tar.gz. Run build first.");
            }

            if server_bin.exists() {
                let archive_time = std::fs::metadata(&archive).and_then(|meta| meta.modified()).ok();
                let server_time = std::fs::metadata(&server_bin).and_then(|meta| meta.modified()).ok();
                if let (Some(archive_time), Some(server_time)) = (archive_time, server_time) {
                    if archive_time < server_time {
                        bail!(
                            "the archive is older than music-server; package.sh did not rebuild it. \
                             Nothing was pushed."
                        );
                    }
                }
            }

            if let Some(t) = tx {
                let _ = t.send(crate::app::AsyncEvent::SetBusy(Some(
                    "Uploading full bundle archive via SCP...".into(),
                )));
                let _ = t.send(crate::app::AsyncEvent::Log(
                    "📦 Uploading yue2-studio-runpod.tar.gz via SCP (-C)...".into(),
                ));
            }

            let remote_tmp_archive = format!("{}.tmp", REMOTE_ARCHIVE);

            let mut scp = pod_command("scp", ssh_port, auth);
            scp.arg(&archive)
                .arg(format!("root@{}:{}", host, remote_tmp_archive));

            let scp_output = scp.output().await.context("Failed to run scp")?;
            if !scp_output.status.success() {
                let err = String::from_utf8_lossy(&scp_output.stderr);
                bail!("SCP failed to upload archive to pod: {}", err.trim());
            }

            if let Some(t) = tx {
                let _ = t.send(crate::app::AsyncEvent::SetBusy(Some(
                    "Extracting binaries on remote pod...".into(),
                )));
                let _ = t.send(crate::app::AsyncEvent::Log(
                    "📂 Extracting binaries on remote pod...".into(),
                ));
            }

            let mut extract_cmd = pod_command("ssh", ssh_port, auth);
            extract_cmd.arg(format!("root@{}", host)).arg(format!(
                "mv -f {tmp_archive} {archive} && mkdir -p {dir} && tar --no-same-owner --no-same-permissions --touch -xzf {archive} -C {dir}/ && rm -f {archive} {dir}/*.sh",
                tmp_archive = remote_tmp_archive,
                dir = REMOTE_STUDIO_DIR,
                archive = REMOTE_ARCHIVE,
            ));

            let ext_output = extract_cmd
                .output()
                .await
                .context("Failed to extract on pod")?;
            if !ext_output.status.success() {
                let err = String::from_utf8_lossy(&ext_output.stderr);
                bail!("Failed to extract archive on remote pod: {}", err.trim());
            }
        }

        if let Some(t) = tx {
            let _ = t.send(crate::app::AsyncEvent::SetBusy(Some(
                "Restarting remote studio daemon...".into(),
            )));
            let _ = t.send(crate::app::AsyncEvent::Log(
                "🔄 Restarting remote studio daemon...".into(),
            ));
        }

        Self::restart_remote(host, ssh_port, auth).await
    }

    /// Restarts remote studio daemon by streaming remote-runner.sh directly into memory over SSH.
    pub async fn restart_remote(host: &str, ssh_port: u16, auth: &SshAuthMethod) -> Result<String> {
        let mut cmd = pod_command("ssh", ssh_port, auth);
        cmd.arg(format!("root@{}", host))
            .arg("bash -s")
            .stdin(Stdio::piped());

        let mut child = cmd
            .spawn()
            .context("Failed to spawn remote runner SSH command")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(REMOTE_RUNNER_SCRIPT.as_bytes()).await?;
        }

        let output = child.wait_with_output().await?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Remote runner failed: {}", err.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    }

    /// Used MB of the pod's `/workspace` volume.
    ///
    /// `df` is useless for this: network volumes are FUSE-backed and report
    /// the backing store's capacity (petabytes), not the volume's own. `du`
    /// reads the files instead, which is slow, so it runs on a long interval.
    pub async fn workspace_usage(host: &str, ssh_port: u16, auth: &SshAuthMethod) -> Result<u64> {
        let mut cmd = pod_command("ssh", ssh_port, auth);
        cmd.arg(format!("root@{}", host))
            .arg("timeout 60 du -sxk /workspace 2>/dev/null | tail -1");

        let output = cmd.output().await.context("Querying /workspace usage")?;
        parse_du(&String::from_utf8_lossy(&output.stdout)).context("Parsing du output")
    }
}

/// One `du -s -k` row: used 1K blocks, then the path. Returns used MB.
fn parse_du(stdout: &str) -> Option<u64> {
    let kb = stdout.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(kb / 1024)
}

pub struct SshTunnel {
    pub host: String,
    pub ssh_port: u16,
    pub local_port: u16,
    pub remote_port: u16,
    pub auth_method: SshAuthMethod,
    pub last_error: Option<String>,
    child: Option<Child>,
}

impl SshTunnel {
    pub fn new(
        host: String,
        ssh_port: u16,
        local_port: u16,
        remote_port: u16,
        auth_method: SshAuthMethod,
    ) -> Self {
        Self {
            host,
            ssh_port,
            local_port,
            remote_port,
            auth_method,
            last_error: None,
            child: None,
        }
    }

    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    self.child = None;
                    if self.last_error.is_none() {
                        self.last_error = Some(format!("Exited with code: {:?}", status.code()));
                    }
                    false
                }
                Err(e) => {
                    self.child = None;
                    self.last_error = Some(format!("Error: {:#}", e));
                    false
                }
            }
        } else {
            false
        }
    }

    pub fn start(&mut self, log_tx: Option<UnboundedSender<crate::app::AsyncEvent>>) -> Result<()> {
        if self.is_running() {
            return Ok(());
        }

        self.last_error = None;

        let mut cmd = Command::new("ssh");
        cmd.arg("-N")
            .arg("-L")
            .arg(format!(
                "{}:127.0.0.1:{}",
                self.local_port, self.remote_port
            ))
            .arg("-p")
            .arg(self.ssh_port.to_string())
            .arg(format!("root@{}", self.host))
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("StrictHostKeyChecking=accept-new")
            .arg("-o")
            .arg("ServerAliveInterval=10")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-o")
            .arg("TCPKeepAlive=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        apply_auth(&mut cmd, &self.auth_method);

        let mut child = cmd
            .spawn()
            .context("Failed to spawn ssh port forwarding process")?;

        // Drain stderr in background to prevent pipe buffer blocks and log errors
        if let Some(stderr) = child.stderr.take() {
            let tx = log_tx;
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trim = line.trim();
                    if !trim.is_empty() {
                        // Suppress transient port forwarding retry errors before service binds
                        if trim.contains("open failed: connect failed: Connection refused") {
                            continue;
                        }
                        if let Some(ref t) = tx {
                            let _ = t.send(crate::app::AsyncEvent::Log(format!("SSH: {}", trim)));
                        }
                    }
                }
            });
        }

        self.child = Some(child);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_du_row() {
        assert_eq!(parse_du("5242880\t/workspace\n"), Some(5_120));
        assert_eq!(parse_du("0\t/workspace"), Some(0));
    }

    #[test]
    fn rejects_output_without_a_row() {
        assert_eq!(parse_du(""), None);
        assert_eq!(parse_du("du: cannot access '/workspace'\n"), None);
    }
}
