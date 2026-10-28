use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkVolume {
    pub id: String,
    pub name: String,
    pub size: u64,
    #[serde(rename = "dataCenterId")]
    pub data_center_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LowestPrice {
    #[serde(rename = "uninterruptablePrice")]
    pub uninterruptable_price: Option<f64>,
    #[serde(rename = "minimumBidPrice")]
    pub minimum_bid_price: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuType {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "memoryInGb")]
    pub memory_in_gb: u32,
    #[serde(rename = "securePrice")]
    pub secure_price: Option<f64>,
    #[serde(rename = "communityPrice")]
    pub community_price: Option<f64>,
    #[serde(rename = "lowestPrice")]
    pub lowest_price: Option<LowestPrice>,
}

impl GpuType {
    /// Returns the exact live price in the selected datacenter if in stock,
    /// otherwise the lowest available on-demand rate across community & secure cloud.
    pub fn lowest_on_demand_price(&self) -> Option<f64> {
        if let Some(ref lp) = self.lowest_price
            && let Some(p) = lp.uninterruptable_price.or(lp.minimum_bid_price)
            && p > 0.0
        {
            return Some(p);
        }

        let mut prices = Vec::new();
        if let Some(p) = self.secure_price
            && p > 0.0
        {
            prices.push(p);
        }
        if let Some(p) = self.community_price
            && p > 0.0
        {
            prices.push(p);
        }
        prices
            .into_iter()
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    pub fn is_in_region_stock(&self) -> bool {
        self.lowest_price
            .as_ref()
            .and_then(|lp| lp.uninterruptable_price.or(lp.minimum_bid_price))
            .is_some()
    }

    pub fn best_cloud_type(&self) -> &'static str {
        match (self.secure_price, self.community_price) {
            (Some(s), Some(c)) if c > 0.0 && c < s => "COMMUNITY",
            (None, Some(c)) if c > 0.0 => "COMMUNITY",
            _ => "SECURE",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPodPort {
    pub ip: Option<String>,
    #[serde(rename = "privatePort")]
    pub private_port: Option<u16>,
    #[serde(rename = "publicPort")]
    pub public_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPodRuntime {
    #[serde(rename = "uptimeInSeconds")]
    pub uptime_in_seconds: Option<u64>,
    pub ports: Option<Vec<RunPodPort>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPodMachine {
    #[serde(rename = "gpuDisplayName")]
    pub gpu_display_name: Option<String>,
}

/// Accepts `null` where a field is declared `String`, so one nameless pod does
/// not fail deserialising the whole list.
fn null_as_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPodItem {
    pub id: String,
    /// Null for pods created through the API without a name, and a missing
    /// field must not fail the whole list.
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(
        rename = "desiredStatus",
        default,
        deserialize_with = "null_as_default"
    )]
    pub desired_status: String,
    #[serde(rename = "costPerHr")]
    pub cost_per_hr: Option<f64>,
    pub runtime: Option<RunPodRuntime>,
    pub machine: Option<RunPodMachine>,
    /// The attached network volume, when there is one; its `size` is the only
    /// trustworthy total for the FUSE-backed `/workspace` mount.
    #[serde(default)]
    pub network_volume: Option<NetworkVolume>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodState {
    Running,
    Initializing,
    Stopped,
    Offline,
}

impl RunPodItem {
    /// The state the UI shows. Colour is the UI's business, so this stays
    /// independent of the theme.
    pub fn state(&self) -> PodState {
        if self.desired_status.eq_ignore_ascii_case("RUNNING") {
            if self.ssh_connection_info().is_some() {
                PodState::Running
            } else {
                PodState::Initializing
            }
        } else if self.desired_status.eq_ignore_ascii_case("EXITED")
            || self.desired_status.eq_ignore_ascii_case("PAUSED")
        {
            PodState::Stopped
        } else {
            PodState::Offline
        }
    }

    pub fn ssh_connection_info(&self) -> Option<(String, u16)> {
        let runtime = self.runtime.as_ref()?;
        let ports = runtime.ports.as_ref()?;
        for p in ports {
            if p.private_port == Some(22)
                && let (Some(ip), Some(pub_port)) = (&p.ip, p.public_port)
            {
                return Some((ip.clone(), pub_port));
            }
        }
        None
    }
}

pub struct RunPodClient {
    api_key: String,
    client: reqwest::Client,
}

impl RunPodClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Posts one GraphQL document and returns the response body, so callers
    /// can walk it with JSON pointers. API and GraphQL errors become `Err`.
    async fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let res = self
            .client
            .post("https://api.runpod.io/graphql")
            .query(&[("api_key", &self.api_key)])
            .json(&serde_json::json!({ "query": query, "variables": variables }))
            .send()
            .await
            .context("Connecting to RunPod API")?;

        if !res.status().is_success() {
            bail!("RunPod API returned HTTP status {}", res.status());
        }

        let body: serde_json::Value = res.json().await.context("Reading the RunPod response")?;
        if let Some(errors) = body.get("errors") {
            bail!("RunPod API error: {}", errors);
        }
        Ok(body)
    }

    pub async fn list_pods_and_volumes(
        &self,
    ) -> Result<(Vec<RunPodItem>, Vec<NetworkVolume>, Option<f64>)> {
        let query = r#"
            query getPodsAndVolumes {
                myself {
                    id
                    clientBalance
                    networkVolumes {
                        id
                        name
                        size
                        dataCenterId
                    }
                    pods {
                        id
                        name
                        desiredStatus
                        costPerHr
                        runtime {
                            uptimeInSeconds
                            ports {
                                ip
                                privatePort
                                publicPort
                            }
                        }
                        machine {
                            gpuDisplayName
                        }
                        networkVolume {
                            id
                            name
                            size
                            dataCenterId
                        }
                    }
                }
            }
        "#;

        let body = self.graphql(query, serde_json::json!({})).await?;

        let pods_json = body
            .pointer("/data/myself/pods")
            .context("Missing pods in GraphQL response")?;
        let pods: Vec<RunPodItem> = serde_json::from_value(pods_json.clone())?;

        let volumes_json = body.pointer("/data/myself/networkVolumes");
        let volumes: Vec<NetworkVolume> = if let Some(v) = volumes_json {
            serde_json::from_value(v.clone()).unwrap_or_default()
        } else {
            Vec::new()
        };

        let balance: Option<f64> = body.pointer("/data/myself/clientBalance").and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        });

        Ok((pods, volumes, balance))
    }

    pub async fn list_gpu_types_for_region(&self, dc_id: Option<&str>) -> Result<Vec<GpuType>> {
        let query = r#"
            query getGpuTypes($input: GpuLowestPriceInput) {
                gpuTypes {
                    id
                    displayName
                    memoryInGb
                    securePrice
                    communityPrice
                    lowestPrice(input: $input) {
                        uninterruptablePrice
                        minimumBidPrice
                    }
                }
            }
        "#;

        let input = if let Some(dc) = dc_id {
            serde_json::json!({ "dataCenterId": dc, "gpuCount": 1 })
        } else {
            serde_json::json!({ "gpuCount": 1 })
        };

        let body = self
            .graphql(query, serde_json::json!({ "input": input }))
            .await?;
        let gpus_json = body
            .pointer("/data/gpuTypes")
            .context("Missing gpuTypes in GraphQL response")?;

        let mut gpus: Vec<GpuType> = serde_json::from_value(gpus_json.clone())?;
        // Keep all modern GPUs >= 12 GB VRAM
        gpus.retain(|g| g.memory_in_gb >= 12 && g.id != "unknown");

        // Sort: in-stock first, then by memory descending
        gpus.sort_by(|a, b| {
            let a_stock = a.is_in_region_stock();
            let b_stock = b.is_in_region_stock();
            match (a_stock, b_stock) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.memory_in_gb.cmp(&a.memory_in_gb),
            }
        });

        Ok(gpus)
    }

    pub async fn deploy_pod(
        &self,
        name: &str,
        image_name: &str,
        gpu: &GpuType,
        network_volume_id: Option<&str>,
    ) -> Result<String> {
        let query = r#"
            mutation deployPod($input: PodFindAndDeployOnDemandInput!) {
                podFindAndDeployOnDemand(input: $input) {
                    id
                    desiredStatus
                }
            }
        "#;

        let mut input = serde_json::json!({
            "name": name,
            "gpuTypeId": gpu.id,
            "gpuCount": 1,
            "imageName": image_name.trim(),
            "volumeInGb": 0,
            "containerDiskInGb": 40,
            "ports": "22/tcp,8791/http",
            "startSsh": true,
        });

        if let Some(vol_id) = network_volume_id {
            input["networkVolumeId"] = serde_json::Value::String(vol_id.to_string());
            input["volumeMountPath"] = serde_json::Value::String("/workspace".to_string());
        } else {
            input["cloudType"] = serde_json::Value::String(gpu.best_cloud_type().to_string());
        }

        let body = self
            .graphql(query, serde_json::json!({ "input": input }))
            .await?;

        let pod_id = body
            .pointer("/data/podFindAndDeployOnDemand/id")
            .and_then(|v| v.as_str())
            .context("No pod id returned from deployment")?;

        Ok(pod_id.to_string())
    }

    pub async fn start_pod(&self, pod_id: &str, gpu_count: i32) -> Result<()> {
        let query = r#"
            mutation podResume($podId: String!, $gpuCount: Int!) {
                podResume(input: { podId: $podId, gpuCount: $gpuCount }) {
                    id
                    desiredStatus
                }
            }
        "#;

        let variables = serde_json::json!({ "podId": pod_id, "gpuCount": gpu_count });
        self.graphql(query, variables).await.map(drop)
    }

    pub async fn stop_pod(&self, pod_id: &str) -> Result<()> {
        let query = r#"
            mutation podStop($podId: String!) {
                podStop(input: { podId: $podId }) {
                    id
                    desiredStatus
                }
            }
        "#;

        self.graphql(query, serde_json::json!({ "podId": pod_id }))
            .await
            .map(drop)
    }

    pub async fn terminate_pod(&self, pod_id: &str) -> Result<()> {
        let query = r#"
            mutation podTerminate($podId: String!) {
                podTerminate(input: { podId: $podId })
            }
        "#;

        self.graphql(query, serde_json::json!({ "podId": pod_id }))
            .await
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(secure: Option<f64>, community: Option<f64>, lowest: Option<f64>) -> GpuType {
        GpuType {
            id: "gpu".to_string(),
            display_name: "GPU".to_string(),
            memory_in_gb: 24,
            secure_price: secure,
            community_price: community,
            lowest_price: lowest.map(|price| LowestPrice {
                uninterruptable_price: Some(price),
                minimum_bid_price: None,
            }),
        }
    }

    #[test]
    fn live_region_price_wins_over_list_prices() {
        assert_eq!(
            gpu(Some(1.0), Some(0.5), Some(0.44)).lowest_on_demand_price(),
            Some(0.44)
        );
        assert!(gpu(Some(1.0), Some(0.5), Some(0.44)).is_in_region_stock());
    }

    #[test]
    fn without_a_region_price_the_cheapest_list_price_is_used() {
        let card = gpu(Some(1.2), Some(0.8), None);
        assert_eq!(card.lowest_on_demand_price(), Some(0.8));
        assert!(!card.is_in_region_stock());
    }

    #[test]
    fn zero_prices_do_not_count() {
        assert_eq!(gpu(Some(0.0), None, None).lowest_on_demand_price(), None);
        assert_eq!(gpu(Some(0.0), None, None).best_cloud_type(), "SECURE");
    }

    #[test]
    fn cloud_type_follows_the_cheaper_price() {
        assert_eq!(
            gpu(Some(1.0), Some(0.5), None).best_cloud_type(),
            "COMMUNITY"
        );
        assert_eq!(gpu(Some(0.5), Some(1.0), None).best_cloud_type(), "SECURE");
        assert_eq!(gpu(Some(0.5), None, None).best_cloud_type(), "SECURE");
    }

    fn pod(desired: &str, ports: serde_json::Value) -> RunPodItem {
        serde_json::from_value(serde_json::json!({
            "id": "pod",
            "name": "pod",
            "desiredStatus": desired,
            "costPerHr": 0.5,
            "runtime": { "uptimeInSeconds": 60, "ports": ports },
        }))
        .expect("pod")
    }

    #[test]
    fn ssh_uses_the_public_port_mapped_to_22() {
        let item = pod(
            "RUNNING",
            serde_json::json!([
                { "ip": "1.2.3.4", "privatePort": 8791, "publicPort": 40000, "type": "tcp" },
                { "ip": "1.2.3.4", "privatePort": 22, "publicPort": 12345, "type": "tcp" }
            ]),
        );
        assert_eq!(
            item.ssh_connection_info(),
            Some(("1.2.3.4".to_string(), 12345))
        );
        assert_eq!(item.state(), PodState::Running);
    }

    #[test]
    fn running_without_an_ssh_port_is_still_initializing() {
        let item = pod("RUNNING", serde_json::json!([]));
        assert_eq!(item.ssh_connection_info(), None);
        assert_eq!(item.state(), PodState::Initializing);
    }

    #[test]
    fn a_null_name_does_not_break_the_list() {
        let item: RunPodItem = serde_json::from_value(serde_json::json!({
            "id": "pod",
            "name": null,
            "desiredStatus": null,
            "costPerHr": null,
            "runtime": null,
            "machine": null,
        }))
        .expect("pods with null fields must still parse");
        assert_eq!(item.name, "");
        assert_eq!(item.state(), PodState::Offline);
    }
}
