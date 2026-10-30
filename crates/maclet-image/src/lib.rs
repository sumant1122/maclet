use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use tar::Archive;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReference {
    pub registry: String,
    pub repository: String,
    pub tag: String,
}

impl ImageReference {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(anyhow!("Empty image reference"));
        }

        let (image_part, tag) = match input.rsplit_once(':') {
            Some((name, tag)) if !tag.contains('/') => (name, tag),
            _ => (input, "latest"),
        };

        let (registry, repository) = if let Some((domain, rest)) = image_part.split_once('/') {
            if domain.contains('.') || domain.contains(':') || domain == "localhost" {
                (domain.to_string(), rest.to_string())
            } else {
                ("registry-1.docker.io".to_string(), format!("{}/{}", domain, rest))
            }
        } else {
            ("registry-1.docker.io".to_string(), format!("library/{}", image_part))
        };

        Ok(Self {
            registry,
            repository,
            tag: tag.to_string(),
        })
    }

    pub fn display_name(&self) -> String {
        let repo = self.repository.trim_start_matches("library/");
        if self.registry == "registry-1.docker.io" {
            format!("{}:{}", repo, self.tag)
        } else {
            format!("{}/{}:{}", self.registry, repo, self.tag)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalImage {
    pub id: String,
    pub name: String,
    pub tag: String,
    pub digest: String,
    pub size_bytes: u64,
    pub created_at: u64,
    pub layers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DockerAuthResponse {
    token: Option<String>,
    access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OciDescriptor {
    pub digest: String,
    pub size: Option<u64>,
    #[serde(rename = "mediaType")]
    pub media_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OciPlatform {
    pub architecture: String,
    pub os: String,
}

#[derive(Debug, Deserialize)]
struct OciIndexManifest {
    pub digest: String,
    pub platform: Option<OciPlatform>,
}

#[derive(Debug, Deserialize)]
struct OciIndex {
    pub manifests: Option<Vec<OciIndexManifest>>,
}

#[derive(Debug, Deserialize)]
struct OciManifest {
    pub layers: Option<Vec<OciDescriptor>>,
}

pub struct ImageManager {
    images_dir: PathBuf,
    client: reqwest::Client,
}

impl ImageManager {
    pub fn new(images_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&images_dir)?;
        fs::create_dir_all(images_dir.join("blobs"))?;
        fs::create_dir_all(images_dir.join("layers"))?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let mut mgr = Self { images_dir, client };
        mgr.ensure_builtin_alpine()?;
        Ok(mgr)
    }

    fn index_file(&self) -> PathBuf {
        self.images_dir.join("images.json")
    }

    fn ensure_builtin_alpine(&mut self) -> Result<()> {
        let mut images = self.list_images()?;
        if !images.iter().any(|img| img.name == "alpine" && img.tag == "latest") {
            images.push(LocalImage {
                id: "alpine-3.20".to_string(),
                name: "alpine".to_string(),
                tag: "latest".to_string(),
                digest: "sha256:builtin-alpine-minirootfs".to_string(),
                size_bytes: 3_250_000,
                created_at: 1718700000,
                layers: vec!["builtin".to_string()],
            });
            let data = serde_json::to_string_pretty(&images)?;
            fs::write(self.index_file(), data)?;
        }
        Ok(())
    }

    pub fn list_images(&self) -> Result<Vec<LocalImage>> {
        let p = self.index_file();
        if !p.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(p)?;
        let list: Vec<LocalImage> = serde_json::from_str(&data).unwrap_or_default();
        Ok(list)
    }

    async fn authenticate(&self, img_ref: &ImageReference) -> Result<Option<String>> {
        if img_ref.registry == "registry-1.docker.io" {
            let auth_url = format!(
                "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{}:pull",
                img_ref.repository
            );
            let resp = self.client.get(&auth_url).send().await?;
            if resp.status().is_success() {
                let auth_data: DockerAuthResponse = resp.json().await?;
                return Ok(auth_data.token.or(auth_data.access_token));
            }
        } else if img_ref.registry == "ghcr.io" {
            let auth_url = format!(
                "https://ghcr.io/token?service=ghcr.io&scope=repository:{}:pull",
                img_ref.repository
            );
            let resp = self.client.get(&auth_url).send().await?;
            if resp.status().is_success() {
                let auth_data: DockerAuthResponse = resp.json().await?;
                return Ok(auth_data.token.or(auth_data.access_token));
            }
        }
        Ok(None)
    }

    pub async fn pull(&self, reference: &str) -> Result<LocalImage> {
        let img_ref = ImageReference::parse(reference)?;
        println!("Pulling image {} from {}...", img_ref.display_name(), img_ref.registry);

        let token = self.authenticate(&img_ref).await?;

        // 1. Fetch manifest or manifest list
        let manifest_url = format!(
            "https://{}/v2/{}/manifests/{}",
            img_ref.registry, img_ref.repository, img_ref.tag
        );

        let mut req = self.client.get(&manifest_url);
        req = req.header(
            "Accept",
            "application/vnd.docker.distribution.manifest.list.v2+json, \
             application/vnd.oci.image.index.v1+json, \
             application/vnd.docker.distribution.manifest.v2+json, \
             application/vnd.oci.image.manifest.v1+json",
        );
        if let Some(ref t) = token {
            req = req.header("Authorization", format!("Bearer {}", t));
        }

        let resp = req.send().await.context("Failed to request manifest")?;
        if !resp.status().is_success() {
            return Err(anyhow!("Registry returned error: {}", resp.status()));
        }

        let body_bytes = resp.bytes().await?;
        let mut target_digest = None;

        // Check if manifest list / index
        if let Ok(index) = serde_json::from_slice::<OciIndex>(&body_bytes) {
            if let Some(manifests) = index.manifests {
                // Find arm64 linux manifest, or fallback to first
                let target_arch = if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" };
                for m in manifests {
                    if let Some(ref p) = m.platform {
                        if p.os == "linux" && (p.architecture == target_arch || p.architecture == std::env::consts::ARCH) {
                            target_digest = Some(m.digest.clone());
                            break;
                        }
                    }
                }
            }
        }

        // If it was a list, fetch the platform manifest
        let manifest: OciManifest = if let Some(digest) = target_digest {
            let plat_url = format!(
                "https://{}/v2/{}/manifests/{}",
                img_ref.registry, img_ref.repository, digest
            );
            let mut req2 = self.client.get(&plat_url);
            req2 = req2.header(
                "Accept",
                "application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json",
            );
            if let Some(ref t) = token {
                req2 = req2.header("Authorization", format!("Bearer {}", t));
            }
            let resp2 = req2.send().await?;
            resp2.json().await?
        } else {
            serde_json::from_slice(&body_bytes)?
        };

        let layers = manifest.layers.ok_or_else(|| anyhow!("No layers found in manifest"))?;
        let mut layer_digests = Vec::new();
        let mut total_size = 0u64;

        // 2. Download each layer blob and unpack
        for (i, layer) in layers.iter().enumerate() {
            let digest = &layer.digest;
            layer_digests.push(digest.clone());
            let short_digest = if digest.len() > 19 { &digest[..19] } else { digest };
            println!("  [Layer {}/{}] Downloading {}...", i + 1, layers.len(), short_digest);

            let blob_path = self.images_dir.join("blobs").join(digest.replace(':', "_"));
            let layer_dir = self.images_dir.join("layers").join(digest.replace(':', "_"));

            if !blob_path.exists() {
                let blob_url = format!(
                    "https://{}/v2/{}/blobs/{}",
                    img_ref.registry, img_ref.repository, digest
                );
                let mut blob_req = self.client.get(&blob_url);
                if let Some(ref t) = token {
                    blob_req = blob_req.header("Authorization", format!("Bearer {}", t));
                }

                let blob_resp = blob_req.send().await?;
                if !blob_resp.status().is_success() {
                    return Err(anyhow!("Failed to download blob {}: {}", digest, blob_resp.status()));
                }

                let mut stream = blob_resp.bytes_stream();
                let mut file = File::create(&blob_path)?;
                let mut hasher = Sha256::new();
                let mut size = 0u64;

                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result?;
                    hasher.update(&chunk);
                    file.write_all(&chunk)?;
                    size += chunk.len() as u64;
                }
                file.flush()?;

                // Verify digest
                let hash = format!("sha256:{:x}", hasher.finalize());
                if digest.starts_with("sha256:") && hash != *digest {
                    let _ = fs::remove_file(&blob_path);
                    return Err(anyhow!("Digest mismatch: expected {}, got {}", digest, hash));
                }
                total_size += size;
            } else {
                total_size += fs::metadata(&blob_path)?.len();
            }

            // Unpack layer if not yet unpacked
            if !layer_dir.exists() {
                fs::create_dir_all(&layer_dir)?;
                if let Ok(bf) = File::open(&blob_path) {
                    let gz = GzDecoder::new(bf);
                    let mut arch = Archive::new(gz);
                    let _ = arch.unpack(&layer_dir);
                }
            }
        }

        let repo_clean = img_ref.repository.trim_start_matches("library/").to_string();
        let short_id = if let Some(last) = layer_digests.last() {
            if last.len() > 19 { last[7..19].to_string() } else { last.clone() }
        } else {
            "unknown".to_string()
        };

        let local_img = LocalImage {
            id: short_id,
            name: repo_clean,
            tag: img_ref.tag.clone(),
            digest: layer_digests.first().cloned().unwrap_or_default(),
            size_bytes: total_size,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            layers: layer_digests,
        };

        // Update local image registry
        let mut images = self.list_images()?;
        images.retain(|img| !(img.name == local_img.name && img.tag == local_img.tag));
        images.push(local_img.clone());
        let data = serde_json::to_string_pretty(&images)?;
        fs::write(self.index_file(), data)?;

        println!("Successfully pulled {}", img_ref.display_name());
        Ok(local_img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_reference_parsing() {
        let r1 = ImageReference::parse("ubuntu:24.04").unwrap();
        assert_eq!(r1.registry, "registry-1.docker.io");
        assert_eq!(r1.repository, "library/ubuntu");
        assert_eq!(r1.tag, "24.04");

        let r2 = ImageReference::parse("alpine").unwrap();
        assert_eq!(r2.repository, "library/alpine");
        assert_eq!(r2.tag, "latest");

        let r3 = ImageReference::parse("ghcr.io/linuxcontainers/alpine:3.20").unwrap();
        assert_eq!(r3.registry, "ghcr.io");
        assert_eq!(r3.repository, "linuxcontainers/alpine");
        assert_eq!(r3.tag, "3.20");
    }
}
