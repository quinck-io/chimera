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
    // A digest goes in the tag field, and the tag before it adds nothing to the pull.
    if let Some((name, digest)) = image.split_once('@') {
        return (strip_tag(name), digest);
    }
    match tag_separator(image) {
        Some(pos) => (&image[..pos], &image[pos + 1..]),
        None => (image, "latest"),
    }
}

fn tag_separator(name: &str) -> Option<usize> {
    let pos = name.rfind(':')?;
    (!name[pos + 1..].contains('/')).then_some(pos)
}

fn strip_tag(name: &str) -> &str {
    tag_separator(name).map_or(name, |pos| &name[..pos])
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

    #[test]
    fn parse_image_with_tag_and_digest() {
        assert_eq!(
            parse_image_ref("ghcr.io/owner/image:v1@sha256:57e8ce5b"),
            ("ghcr.io/owner/image", "sha256:57e8ce5b")
        );
    }

    #[test]
    fn parse_image_with_digest_only() {
        assert_eq!(
            parse_image_ref("node@sha256:57e8ce5b"),
            ("node", "sha256:57e8ce5b")
        );
    }

    #[test]
    fn parse_image_with_registry_port() {
        assert_eq!(
            parse_image_ref("localhost:5000/image:v1"),
            ("localhost:5000/image", "v1")
        );
        assert_eq!(
            parse_image_ref("localhost:5000/image"),
            ("localhost:5000/image", "latest")
        );
    }

    #[tokio::test]
    #[ignore]
    async fn ensure_image_pulls_a_tag_pinned_to_a_digest() {
        let docker = connect(None).unwrap();
        let image =
            "alpine:3.20@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc";
        let _ = docker.remove_image(image, None, None).await;

        ensure_image(&docker, image, None).await.unwrap();

        assert!(docker.inspect_image(image).await.is_ok());
    }
}
