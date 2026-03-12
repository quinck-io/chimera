use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, RemoveContainerOptions, StopContainerOptions,
};
use bollard::models::{EndpointSettings, HostConfig, PortBinding};
use tracing::{debug, info, warn};

use super::client::ensure_image;
use super::container::{JobContainerSpec, ServiceContainerSpec};
use super::network::{create_job_network, get_network_gateway, remove_network};

const STOP_TIMEOUT_SECS: i64 = 5;

/// Parameters for setting up Docker resources for a job.
pub struct SetupParams<'a> {
    pub runner_name: &'a str,
    pub job_id: &'a str,
    pub job_container: Option<&'a JobContainerSpec>,
    pub services: &'a [ServiceContainerSpec],
    pub workspace_host_path: &'a Path,
    pub workflow_files_host_path: &'a Path,
    pub runner_temp_host_path: &'a Path,
    pub actions_host_path: &'a Path,
    pub tool_cache_host_path: &'a Path,
    pub externals_dir: &'a Path,
}

/// Owns all Docker resources for a single job and guarantees cleanup.
pub struct JobDockerResources {
    docker: Docker,
    network_name: Option<String>,
    job_container_id: Option<String>,
    service_container_ids: Vec<String>,
    service_addresses: HashMap<String, String>,
    /// Host→container path mappings for remapping paths when running inside the container.
    path_mappings: Vec<(PathBuf, String)>,
    /// Path to the node binary inside the container (if mounted from host).
    node_container_path: Option<String>,
    /// Gateway IP of the job network (host IP from container's perspective).
    host_gateway_ip: Option<String>,
    /// Default environment from the job container's Docker image (ENV directives).
    image_env: HashMap<String, String>,
}

impl JobDockerResources {
    pub fn new(docker: Docker) -> Self {
        Self {
            docker,
            network_name: None,
            job_container_id: None,
            service_container_ids: Vec::new(),
            service_addresses: HashMap::new(),
            path_mappings: Vec::new(),
            node_container_path: None,
            host_gateway_ip: None,
            image_env: HashMap::new(),
        }
    }

    /// Set up all Docker resources: network, service containers, and job container.
    pub async fn setup(&mut self, params: &SetupParams<'_>) -> Result<()> {
        let network_name =
            create_job_network(&self.docker, params.runner_name, params.job_id).await?;
        self.network_name = Some(network_name.clone());

        // Resolve gateway IP so containers can reach host-bound services (e.g. cache server)
        match get_network_gateway(&self.docker, &network_name).await {
            Ok(gw) => self.host_gateway_ip = Some(gw),
            Err(e) => warn!(error = %e, "could not resolve network gateway"),
        }

        // Start service containers
        for (i, svc) in params.services.iter().enumerate() {
            ensure_image(&self.docker, &svc.image, svc.credentials.as_ref()).await?;

            let alias = svc.alias.clone().unwrap_or_else(|| format!("svc-{i}"));
            let container_name = format!(
                "chimera-{}-{}-svc-{alias}",
                params.runner_name, params.job_id
            );

            let port_bindings = parse_port_bindings(&svc.ports);
            let env_list: Vec<String> = svc
                .environment
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();

            let mut host_config = HostConfig {
                network_mode: Some(network_name.clone()),
                port_bindings: if port_bindings.is_empty() {
                    None
                } else {
                    Some(port_bindings)
                },
                binds: if svc.volumes.is_empty() {
                    None
                } else {
                    Some(svc.volumes.clone())
                },
                security_opt: Some(vec!["no-new-privileges:true".into()]),
                ..Default::default()
            };
            apply_options(&mut host_config, svc.options.as_deref());

            let config = Config {
                image: Some(svc.image.as_str()),
                env: Some(env_list.iter().map(|s| s.as_str()).collect()),
                host_config: Some(host_config),
                networking_config: Some(bollard::container::NetworkingConfig {
                    endpoints_config: HashMap::from([(
                        network_name.as_str(),
                        EndpointSettings {
                            aliases: Some(vec![alias.clone()]),
                            ..Default::default()
                        },
                    )]),
                }),
                ..Default::default()
            };

            let container = self
                .docker
                .create_container(
                    Some(CreateContainerOptions {
                        name: container_name.as_str(),
                        ..Default::default()
                    }),
                    config,
                )
                .await
                .with_context(|| format!("creating service container {container_name}"))?;

            self.docker
                .start_container::<String>(&container.id, None)
                .await
                .with_context(|| format!("starting service container {container_name}"))?;

            // Get the container's IP on the bridge network
            let inspect = self
                .docker
                .inspect_container(&container.id, None)
                .await
                .with_context(|| format!("inspecting service container {container_name}"))?;

            if let Some(ip) = inspect
                .network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .and_then(|nets| nets.get(&network_name))
                .and_then(|ep| ep.ip_address.as_ref())
                .filter(|ip| !ip.is_empty())
            {
                self.service_addresses.insert(alias.clone(), ip.clone());
            }

            info!(
                container = %container_name,
                image = %svc.image,
                alias = %alias,
                "service container started"
            );

            self.service_container_ids.push(container.id);
        }

        // Start job container (if present)
        if let Some(spec) = params.job_container {
            ensure_image(&self.docker, &spec.image, spec.credentials.as_ref()).await?;

            // Extract default ENV from the image so we can inherit PATH etc.
            if let Ok(inspect) = self.docker.inspect_image(&spec.image).await
                && let Some(config) = inspect.config
                && let Some(env_list) = config.env
            {
                for entry in env_list {
                    if let Some((k, v)) = entry.split_once('=') {
                        self.image_env.insert(k.to_string(), v.to_string());
                    }
                }
            }

            let container_name = format!("chimera-{}-{}-job", params.runner_name, params.job_id);
            let workspace_mount = format!(
                "{}:/github/workspace",
                params.workspace_host_path.to_string_lossy()
            );
            let workflow_mount = format!(
                "{}:/github/workflow",
                params.workflow_files_host_path.to_string_lossy()
            );
            let temp_mount = format!(
                "{}:/github/tmp",
                params.runner_temp_host_path.to_string_lossy()
            );
            let actions_mount = format!(
                "{}:/github/actions",
                params.actions_host_path.to_string_lossy()
            );
            let tool_cache_mount = format!(
                "{}:/github/tool-cache",
                params.tool_cache_host_path.to_string_lossy()
            );

            let mut binds = vec![
                workspace_mount,
                workflow_mount,
                temp_mount,
                actions_mount,
                tool_cache_mount,
            ];

            // Ensure a Linux node binary is available and mount it into the container.
            // The official runner ships node in externals/; we download it on first use.
            let node_bin = crate::node::ensure_linux_node(params.externals_dir).await?;
            // node_bin is e.g. externals/node20-linux-x64/bin/node — mount the grandparent dir
            let node_dir = node_bin
                .parent()
                .and_then(|p| p.parent())
                .context("unexpected node binary path structure")?;
            let node_mount = format!("{}:/github/externals/node:ro", node_dir.display());
            binds.push(node_mount);
            self.node_container_path = Some("/github/externals/node/bin/node".into());

            binds.extend(spec.volumes.clone());

            let env_list: Vec<String> = spec
                .environment
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();

            let port_bindings = parse_port_bindings(&spec.ports);

            let mut host_config = HostConfig {
                network_mode: Some(network_name.clone()),
                binds: Some(binds),
                port_bindings: if port_bindings.is_empty() {
                    None
                } else {
                    Some(port_bindings)
                },
                security_opt: Some(vec!["no-new-privileges:true".into()]),
                ..Default::default()
            };
            apply_options(&mut host_config, spec.options.as_deref());

            let config = Config {
                image: Some(spec.image.as_str()),
                cmd: Some(vec!["tail", "-f", "/dev/null"]),
                env: Some(env_list.iter().map(|s| s.as_str()).collect()),
                host_config: Some(host_config),
                working_dir: Some("/github/workspace"),
                networking_config: Some(bollard::container::NetworkingConfig {
                    endpoints_config: HashMap::from([(
                        network_name.as_str(),
                        EndpointSettings::default(),
                    )]),
                }),
                ..Default::default()
            };

            let container = self
                .docker
                .create_container(
                    Some(CreateContainerOptions {
                        name: container_name.as_str(),
                        ..Default::default()
                    }),
                    config,
                )
                .await
                .with_context(|| format!("creating job container {container_name}"))?;

            self.docker
                .start_container::<String>(&container.id, None)
                .await
                .with_context(|| format!("starting job container {container_name}"))?;

            info!(
                container = %container_name,
                image = %spec.image,
                "job container started"
            );

            self.job_container_id = Some(container.id);

            // Store path mappings for host→container remapping
            self.path_mappings = vec![
                (
                    params.workspace_host_path.to_path_buf(),
                    "/github/workspace".into(),
                ),
                (
                    params.workflow_files_host_path.to_path_buf(),
                    "/github/workflow".into(),
                ),
                (
                    params.runner_temp_host_path.to_path_buf(),
                    "/github/tmp".into(),
                ),
                (
                    params.actions_host_path.to_path_buf(),
                    "/github/actions".into(),
                ),
                (
                    params.tool_cache_host_path.to_path_buf(),
                    "/github/tool-cache".into(),
                ),
            ];
        }

        Ok(())
    }

    /// Clean up all Docker resources. Never panics — all errors are logged as warnings.
    pub async fn cleanup(&mut self) {
        // Stop and remove job container
        if let Some(id) = self.job_container_id.take() {
            stop_and_remove(&self.docker, &id, "job container").await;
        }

        // Stop and remove service containers (reverse order)
        let service_ids: Vec<String> = self.service_container_ids.drain(..).rev().collect();
        for id in &service_ids {
            stop_and_remove(&self.docker, id, "service container").await;
        }

        // Remove the network
        if let Some(name) = self.network_name.take() {
            remove_network(&self.docker, &name).await;
        }

        self.service_addresses.clear();
    }

    pub fn job_container_id(&self) -> Option<&str> {
        self.job_container_id.as_deref()
    }

    pub fn service_addresses(&self) -> &HashMap<String, String> {
        &self.service_addresses
    }

    pub fn docker(&self) -> &Docker {
        &self.docker
    }

    pub fn network_name(&self) -> Option<&str> {
        self.network_name.as_deref()
    }

    /// Gateway IP of the job network — the host address from the container's perspective.
    pub fn host_gateway_ip(&self) -> Option<&str> {
        self.host_gateway_ip.as_deref()
    }

    /// Path to the node binary inside the container.
    /// Falls back to "node" (relying on container PATH) if not mounted from host.
    pub fn node_path(&self) -> &str {
        self.node_container_path.as_deref().unwrap_or("node")
    }

    /// Default environment variables from the job container's Docker image.
    pub fn image_env(&self) -> &HashMap<String, String> {
        &self.image_env
    }

    /// Remap a host path to its container-internal equivalent using bind mount mappings.
    pub fn remap_to_container_path(&self, host_path: &Path) -> Option<String> {
        for (host_prefix, container_prefix) in &self.path_mappings {
            if let Ok(suffix) = host_path.strip_prefix(host_prefix) {
                if suffix.as_os_str().is_empty() {
                    return Some(container_prefix.clone());
                }
                return Some(format!("{}/{}", container_prefix, suffix.display()));
            }
        }
        None
    }
}

/// Stop a container (SIGTERM -> timeout -> SIGKILL) and remove it with volumes.
pub(crate) async fn stop_and_remove(docker: &Docker, container_id: &str, label: &str) {
    let stop_opts = StopContainerOptions {
        t: STOP_TIMEOUT_SECS,
    };
    if let Err(e) = docker.stop_container(container_id, Some(stop_opts)).await {
        debug!(container = %container_id, error = %e, "{label}: stop failed (may already be stopped)");
    }

    let remove_opts = RemoveContainerOptions {
        force: true,
        v: true,
        ..Default::default()
    };
    if let Err(e) = docker
        .remove_container(container_id, Some(remove_opts))
        .await
    {
        warn!(container = %container_id, error = %e, "{label}: remove failed");
    } else {
        debug!(container = %container_id, "{label}: removed");
    }
}

/// Apply user-specified `--options` string to host config.
/// Supports common flags like `--privileged`, `--cap-add`, `--cpus`, `--memory`.
fn apply_options(host_config: &mut HostConfig, options: Option<&str>) {
    let options = match options {
        Some(o) if !o.is_empty() => o,
        _ => return,
    };

    let parts: Vec<&str> = options.split_whitespace().collect();
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "--privileged" => {
                host_config.privileged = Some(true);
            }
            "--cap-add" => {
                if i + 1 < parts.len() {
                    i += 1;
                    host_config
                        .cap_add
                        .get_or_insert_with(Vec::new)
                        .push(parts[i].to_string());
                }
            }
            "--cap-drop" => {
                if i + 1 < parts.len() {
                    i += 1;
                    host_config
                        .cap_drop
                        .get_or_insert_with(Vec::new)
                        .push(parts[i].to_string());
                }
            }
            _ => {
                debug!(option = parts[i], "ignoring unrecognized container option");
            }
        }
        i += 1;
    }
}

/// Parse port binding strings like "8080:8080" or "8080:8080/tcp" into bollard format.
fn parse_port_bindings(ports: &[String]) -> HashMap<String, Option<Vec<PortBinding>>> {
    let mut bindings = HashMap::new();
    for port_spec in ports {
        let parts: Vec<&str> = port_spec.split(':').collect();
        if parts.len() == 2 {
            let host_port = parts[0].to_string();
            let container_port = if parts[1].contains('/') {
                parts[1].to_string()
            } else {
                format!("{}/tcp", parts[1])
            };

            bindings.insert(
                container_port,
                Some(vec![PortBinding {
                    host_ip: Some("0.0.0.0".into()),
                    host_port: Some(host_port),
                }]),
            );
        }
    }
    bindings
}

#[cfg(test)]
#[path = "resources_test.rs"]
mod resources_test;
