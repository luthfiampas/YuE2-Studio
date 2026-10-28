use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuSnapshot {
    pub name: String,
    #[serde(default)]
    pub vram_used_mb: u64,
    #[serde(default)]
    pub vram_total_mb: u64,
    pub utilization_percent: Option<u8>,
    pub temperature_c: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    #[serde(default)]
    pub cpu_percent: f32,
    #[serde(default)]
    pub ram_used_mb: u64,
    #[serde(default)]
    pub ram_total_mb: u64,
    #[serde(default)]
    pub gpus: Vec<GpuSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemResourcesResponse {
    pub resources: ResourceSnapshot,
}

pub struct RemoteTelemetryClient {
    base_url: String,
    client: reqwest::Client,
}

impl RemoteTelemetryClient {
    pub fn new(local_port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", local_port),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(4))
                .build()
                .unwrap_or_default(),
        }
    }

    pub async fn check_health(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        self.client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    pub async fn get_resources(&self) -> Result<ResourceSnapshot> {
        let url = format!("{}/v1/system/resources", self.base_url);
        let res = self
            .client
            .get(&url)
            .send()
            .await
            .context("Connecting to /v1/system/resources")?;
        let wrapper: SystemResourcesResponse = res.json().await?;
        Ok(wrapper.resources)
    }
}
