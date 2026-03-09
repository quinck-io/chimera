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

/// Remove a network by name. Logs a warning on failure rather than propagating.
pub async fn remove_network(docker: &Docker, name: &str) {
    if let Err(e) = docker.remove_network(name).await {
        warn!(network = %name, error = %e, "failed to remove network");
    } else {
        debug!(network = %name, "removed network");
    }
}
