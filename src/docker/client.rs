use anyhow::{Context, Result};
use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::image::CreateImageOptions;
use futures::StreamExt;
use tracing::{debug, info};

use super::container::ContainerCredentials;

/// Connect to the Docker daemon via the given socket path (or default).
pub fn connect(socket: Option<&str>) -> Result<Docker> {
    let docker = match socket {
        Some(path) => Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
            .with_context(|| format!("connecting to Docker socket at {path}"))?,
        None => Docker::connect_with_local_defaults()
            .context("connecting to Docker with local defaults")?,
    };
    Ok(docker)
}

/// Verify the Docker daemon is reachable.
pub async fn ping(docker: &Docker) -> Result<()> {
    docker.ping().await.context("pinging Docker daemon")?;
    debug!("Docker daemon is reachable");
    Ok(())
}

/// Ensure a Docker image is available locally; pull it if missing.
/// Optionally uses credentials for private registry authentication.
pub async fn ensure_image(
    docker: &Docker,
    image: &str,
    credentials: Option<&ContainerCredentials>,
) -> Result<()> {
    match docker.inspect_image(image).await {
        Ok(_) => {
            debug!(image, "image already present");
            return Ok(());
        }
        Err(_) => {
            info!(image, "pulling image");
        }
    }

    let (repo, tag) = parse_image_ref(image);
    let opts = CreateImageOptions {
        from_image: repo,
        tag,
        ..Default::default()
    };

    let docker_creds = credentials.map(|c| DockerCredentials {
        username: c.username.clone(),
        password: c.password.clone(),
        ..Default::default()
    });

    let mut stream = docker.create_image(Some(opts), None, docker_creds);
    while let Some(result) = stream.next().await {
        result.with_context(|| format!("pulling image {image}"))?;
    }

    info!(image, "image pulled successfully");
    Ok(())
}

/// Split "image:tag" into ("image", "tag"), defaulting tag to "latest".
fn parse_image_ref(image: &str) -> (&str, &str) {
    // Handle images with registry prefix (e.g., ghcr.io/owner/image:tag)
    // The tag separator is the last colon that's not part of a port/registry
    if let Some(colon_pos) = image.rfind(':') {
        let after_colon = &image[colon_pos + 1..];
        // If there's a slash after the colon, it's part of the registry path, not a tag
        if !after_colon.contains('/') {
            return (&image[..colon_pos], after_colon);
        }
    }
    (image, "latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_image() {
        assert_eq!(parse_image_ref("ubuntu:22.04"), ("ubuntu", "22.04"));
    }

    #[test]
    fn parse_image_no_tag() {
        assert_eq!(parse_image_ref("ubuntu"), ("ubuntu", "latest"));
    }

    #[test]
    fn parse_image_with_registry() {
        assert_eq!(
            parse_image_ref("ghcr.io/owner/image:v1"),
            ("ghcr.io/owner/image", "v1")
        );
    }

    #[test]
    fn parse_image_with_registry_no_tag() {
        assert_eq!(
            parse_image_ref("ghcr.io/owner/image"),
            ("ghcr.io/owner/image", "latest")
        );
    }
}
