// server/wa2lsp/src/spec_source.rs

use std::io::Read;
use std::time::Duration;

use bytes::Bytes;
use flate2::read::GzDecoder;
use reqwest::{Client, StatusCode, header};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum SpecSourceError {
	#[error("unsupported CloudFormation region '{0}'")]
	UnsupportedRegion(String),

	#[error("invalid spec URL: {0}")]
	Url(#[from] url::ParseError),

	#[error("HTTP error fetching CloudFormation spec: {0}")]
	Http(#[from] reqwest::Error),

	#[error("unexpected HTTP status {status} from {url}")]
	UnexpectedStatus { status: StatusCode, url: Url },

	#[error("failed to decompress CloudFormation spec: {0}")]
	Decompression(#[from] std::io::Error),

	#[error("failed to extract ZIP archive: {0}")]
	ZipExtraction(#[from] zip::result::ZipError),
}

/// Raw download of the CloudFormation resource specification.
#[derive(Debug, Clone)]
pub struct SpecDownload {
	/// Raw JSON bytes of the specification.
	pub body: Bytes,
	/// Value of the ETag header, if present.
	pub etag: Option<String>,
	/// Value of the Last-Modified header, if present.
	pub last_modified: Option<String>,
	/// Value of the Content-Length header, if present.
	pub content_length: Option<u64>,
	/// The URL the spec was fetched from.
	pub url: Url,
}

/// Responsible for downloading the AWS CloudFormation Resource
/// Specification JSON from AWS.
#[derive(Clone, Debug)]
pub struct SpecSource {
	client: Client,
	url: Url,
}

impl SpecSource {
	/// Create a SpecSource for downloading registry schemas (ZIP format).
	/// Uses the CloudFormation schema registry URL for the given region.
	pub fn for_region_schemas(region: &str) -> Result<Self, SpecSourceError> {
		// Schema registry URLs follow this pattern for all regions
		let url_str = format!(
			"https://schema.cloudformation.{}.amazonaws.com/CloudformationSchema.zip",
			region
		);
		let url = Url::parse(&url_str)?;
		Ok(Self::from_url(url))
	}

	/// Download the registry schema ZIP file.
	/// Returns the raw bytes that can be extracted as a ZIP archive.
	pub async fn download_schema_zip(&self) -> Result<SpecDownload, SpecSourceError> {
		// Use existing download() method - ZIP files aren't gzipped
		self.download().await
	}

	/// Create a SpecSource for a given region, using AWS's published
	/// resource specification URL for that region.
	pub fn for_region(region: &str) -> Result<Self, SpecSourceError> {
		let host = match region {
			// US East (N. Virginia)
			"us-east-1" => "d1uauaxba7bl26.cloudfront.net",
			// Europe (Ireland)
			"eu-west-1" => "d3teyb21fexa9r.cloudfront.net",
			// Europe (London)
			"eu-west-2" => "d1742qcu2c1ncx.cloudfront.net",
			// Europe (Frankfurt)
			"eu-central-1" => "d1mta8qj7i28i2.cloudfront.net",
			other => return Err(SpecSourceError::UnsupportedRegion(other.to_string())),
		};

		let url_str =
			format!("https://{host}/latest/gzip/CloudFormationResourceSpecification.json");
		let url = Url::parse(&url_str)?;

		Ok(Self::from_url(url))
	}

	/// Create a SpecSource from an explicit URL, bypassing any region
	/// lookup. Useful for testing or custom mirrors.
	pub fn from_url(url: Url) -> Self {
		let client = Client::builder()
			.user_agent("wa2lsp/0.1 (WA2 CloudFormation LSP)")
			.timeout(Duration::from_secs(15))
			.build()
			// safe unwrap: configuration above cannot fail in practice
			.expect("failed to build reqwest::Client for SpecSource");

		SpecSource { client, url }
	}

	/// Download the latest CloudFormation resource specification JSON.
	///
	/// This performs a plain GET and returns the raw bytes plus basic
	/// HTTP metadata.
	pub async fn download(&self) -> Result<SpecDownload, SpecSourceError> {
		let resp = self.client.get(self.url.clone()).send().await?;

		let status = resp.status();
		if !status.is_success() {
			return Err(SpecSourceError::UnexpectedStatus {
				status,
				url: self.url.clone(),
			});
		}

		let headers = resp.headers().clone();
		let raw_body = resp.bytes().await?;

		// transparently gunzip if this looks like a gzipped file.
		let body = if is_gzip_magic(&raw_body) {
			let mut decoder = GzDecoder::new(&raw_body[..]);
			let mut out = Vec::new();
			decoder.read_to_end(&mut out)?;
			Bytes::from(out)
		} else {
			raw_body
		};

		let etag = headers
			.get(header::ETAG)
			.and_then(|v| v.to_str().ok())
			.map(|s| s.to_owned());

		let last_modified = headers
			.get(header::LAST_MODIFIED)
			.and_then(|v| v.to_str().ok())
			.map(|s| s.to_owned());

		let content_length = headers
			.get(header::CONTENT_LENGTH)
			.and_then(|v| v.to_str().ok())
			.and_then(|s| s.parse::<u64>().ok());

		Ok(SpecDownload {
			body,
			etag,
			last_modified,
			content_length,
			url: self.url.clone(),
		})
	}

	/// Expose the underlying URL for hashing / logging / testing.
	pub fn url(&self) -> &Url {
		&self.url
	}
}

/// Check whether the body begins with the gzip magic bytes 0x1f 0x8b.
fn is_gzip_magic(body: &Bytes) -> bool {
	body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b
}
