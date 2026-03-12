use anyhow::{Context, Result};
use bollard::Docker;
use bollard::network::CreateNetworkOptions;
use tracing::{debug, warn};

/// Create a bridge network for a job. Returns the network name.
pub async fn create_job_network(
    docker: &Docker,
    runner_name: &str,
    job_id: &str,
) -> Result<String> {
    let name = format!("chimera-{runner_name}-{job_id}");

    let opts = CreateNetworkOptions {
        name: name.as_str(),
        driver: "bridge",
        ..Default::default()
    };

    docker
        .create_network(opts)
        .await
        .with_context(|| format!("creating network {name}"))?;

    debug!(network = %name, "created bridge network");
    Ok(name)
}

/// Get the gateway IP of a Docker network. This is the host IP from the
/// container's perspective, used so containers can reach host-bound services.
pub async fn get_network_gateway(docker: &Docker, network_name: &str) -> Result<String> {
    let network = docker
        .inspect_network::<String>(network_name, None)
        .await
        .with_context(|| format!("inspecting network {network_name}"))?;

    let gateway = network
        .ipam
        .and_then(|ipam| ipam.config)
        .and_then(|configs| configs.into_iter().next())
        .and_then(|config| config.gateway)
        .with_context(|| format!("no gateway found for network {network_name}"))?;

    debug!(network = %network_name, gateway = %gateway, "resolved network gateway");
    Ok(gateway)
}

/// Remove a network by name. Logs a warning on failure rather than propagating.
pub async fn remove_network(docker: &Docker, name: &str) {
    if let Err(e) = docker.remove_network(name).await {
        warn!(network = %name, error = %e, "failed to remove network");
    } else {
        debug!(network = %name, "removed network");
    }
}
