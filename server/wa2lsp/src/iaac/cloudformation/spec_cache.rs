use std::{
	fs, io,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, SystemTime},
};

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::iaac::cloudformation::registry_store;

use super::spec_source::{SpecDownload, SpecSource, SpecSourceError};
use super::spec_store::SpecStore;

/// How long a cached spec is considered "fresh".
const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Error)]
pub enum SpecCacheError {
	#[error("I/O error accessing CloudFormation spec cache: {0}")]
	Io(#[from] io::Error),

	#[error("error downloading CloudFormation spec: {0}")]
	Source(#[from] SpecSourceError),

	#[error("error decoding cache metadata JSON: {0}")]
	MetaJson(#[from] serde_json::Error),

	#[error("failed to build SpecStore from cached/downloaded spec: {0}")]
	Build(String),
}

/// On-disk metadata for a cached spec.
#[derive(Debug, Serialize, Deserialize)]
struct SpecCacheMeta {
	/// URL the spec was fetched from.
	url: String,
	/// When we last successfully fetched this spec.
	fetched_at: SystemTime,
	/// ETag from the server, if present.
	etag: Option<String>,
	/// Last-Modified from the server, if present.
	last_modified: Option<String>,
}

/// Manages disk caching of the CloudFormation Resource Specification,
/// and produces a parsed, read-only SpecStore.
///
/// Responsibilities:
/// - Decide whether to use the existing cached JSON or re-download.
/// - Persist the raw JSON and small metadata file.
/// - Build a SpecStore from the JSON and return it as `Arc<SpecStore>`.
#[derive(Clone)]
pub struct SpecCacheManager {
	source: SpecSource,
	cache_dir: PathBuf,
	cache_key: String,
}

impl SpecCacheManager {
	/// Create a new cache manager.
	///
	/// `cache_dir` is a directory where WA2 can store the JSON and
	/// metadata. It will be created if it does not exist.
	pub fn new(source: SpecSource, cache_dir: impl AsRef<Path>) -> Self {
		let cache_dir = cache_dir.as_ref().to_path_buf();
		let cache_key = Self::hash_url(&source_url(&source));

		SpecCacheManager {
			source,
			cache_dir,
			cache_key,
		}
	}

	/// Load a SpecStore from registry schemas, using the on-disk cache where possible.
	///
	/// Policy is the same as load_spec_store() but downloads ZIP of schemas instead.
	pub async fn load_registry_spec_store(&self) -> Result<Arc<SpecStore>, SpecCacheError> {
		fs::create_dir_all(&self.cache_dir)?;

		// 1. Try to use existing cache if it's fresh and parseable.
		if let Some(meta) = self.read_meta().ok().flatten()
			&& !Self::is_expired(meta.fetched_at)
			&& let Ok(store) = self.load_registry_from_cache()
		{
			return Ok(Arc::new(store));
		}

		// 2. Cache missing/expired/corrupt → refresh from network.
		let download = self.source.download_schema_zip().await?;
		let store = self.build_registry_store_from_download(&download)?;

		// Persist to disk; if it fails we still return the in-memory store.
		let _ = self.write_cache(&download);

		Ok(Arc::new(store))
	}

	fn load_registry_from_cache(&self) -> Result<SpecStore, SpecCacheError> {
		let path = self.spec_json_path();
		let bytes = fs::read(path)?;
		registry_store::build_from_zip(&bytes.into())
			.map_err(|e| SpecCacheError::Build(e.to_string()))
	}

	fn build_registry_store_from_download(
		&self,
		download: &SpecDownload,
	) -> Result<SpecStore, SpecCacheError> {
		registry_store::build_from_zip(&download.body)
			.map_err(|e| SpecCacheError::Build(e.to_string()))
	}

	/// Load a SpecStore, using the on-disk cache where possible.
	///
	/// Policy:
	/// - If cached JSON + metadata exists and is younger than
	///   CACHE_MAX_AGE, use it.
	/// - Otherwise, download a fresh spec, update cache, and use it.
	pub async fn load_spec_store(&self) -> Result<Arc<SpecStore>, SpecCacheError> {
		fs::create_dir_all(&self.cache_dir)?;

		// 1. Try to use existing cache if it's fresh and parseable.
		if let Some(meta) = self.read_meta().ok().flatten()
			&& !Self::is_expired(meta.fetched_at)
			&& let Ok(store) = self.load_from_cache()
		{
			return Ok(Arc::new(store));
			// Corrupt cache or build error: fall through to refresh.
		}

		// 2. Cache missing/expired/corrupt → refresh from network.
		let download = self.source.download().await?;
		let store = self.build_store_from_download(&download)?;

		// Persist to disk; if it fails we still return the in-memory store.
		let _ = self.write_cache(&download);

		Ok(Arc::new(store))
	}

	fn spec_json_path(&self) -> PathBuf {
		self.cache_dir
			.join(format!("cfn_spec_{}.json", self.cache_key))
	}

	fn meta_path(&self) -> PathBuf {
		self.cache_dir
			.join(format!("cfn_spec_{}.meta.json", self.cache_key))
	}

	fn hash_url(url: &str) -> String {
		let mut hasher = Hasher::new();
		hasher.update(url.as_bytes());
		let hash = hasher.finalize();
		hash.to_hex().to_string()
	}

	fn is_expired(fetched_at: SystemTime) -> bool {
		match fetched_at.elapsed() {
			Ok(elapsed) => elapsed > CACHE_MAX_AGE,
			Err(_) => false, // clock went backwards; treat as not expired
		}
	}

	fn read_meta(&self) -> Result<Option<SpecCacheMeta>, SpecCacheError> {
		let meta_path = self.meta_path();
		if !meta_path.exists() {
			return Ok(None);
		}

		let bytes = fs::read(&meta_path)?;
		let meta: SpecCacheMeta = serde_json::from_slice(&bytes)?;
		Ok(Some(meta))
	}

	fn load_from_cache(&self) -> Result<SpecStore, SpecCacheError> {
		let path = self.spec_json_path();
		let bytes = fs::read(path)?;
		SpecStore::from_json_bytes(&bytes).map_err(|e| SpecCacheError::Build(e.to_string()))
	}

	fn build_store_from_download(
		&self,
		download: &SpecDownload,
	) -> Result<SpecStore, SpecCacheError> {
		SpecStore::from_json_bytes(&download.body).map_err(|e| SpecCacheError::Build(e.to_string()))
	}

	fn write_cache(&self, download: &SpecDownload) -> Result<(), SpecCacheError> {
		let json_path = self.spec_json_path();
		let meta_path = self.meta_path();

		// Write JSON to a temp file then rename for atomic-ish update.
		let tmp_json_path = json_path.with_extension("json.tmp");
		fs::write(&tmp_json_path, &download.body)?;
		fs::rename(tmp_json_path, &json_path)?;

		let meta = SpecCacheMeta {
			url: download.url.to_string(),
			fetched_at: SystemTime::now(),
			etag: download.etag.clone(),
			last_modified: download.last_modified.clone(),
		};

		let meta_bytes = serde_json::to_vec_pretty(&meta)?;
		let tmp_meta_path = meta_path.with_extension("meta.json.tmp");
		fs::write(&tmp_meta_path, &meta_bytes)?;
		fs::rename(tmp_meta_path, &meta_path)?;

		Ok(())
	}
}

/// Helper to get the URL string from SpecSource without exposing its fields.
fn source_url(source: &SpecSource) -> String {
	source.url().to_string()
}
