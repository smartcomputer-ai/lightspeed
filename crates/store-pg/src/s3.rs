use std::{fmt, sync::Arc};

use object_store::{ObjectStore, aws::AmazonS3Builder};

#[derive(Clone, PartialEq, Eq)]
pub struct S3ObjectStoreConfig {
    pub bucket: String,
    pub endpoint: Option<String>,
    pub region: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub force_path_style: bool,
}

impl fmt::Debug for S3ObjectStoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3ObjectStoreConfig")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("endpoint_configured", &self.endpoint.is_some())
            .field("credentials_configured", &self.secret_access_key.is_some())
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

impl S3ObjectStoreConfig {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            endpoint: None,
            region: "us-east-1".to_owned(),
            access_key_id: None,
            secret_access_key: None,
            force_path_style: false,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    pub fn with_access_key_id(mut self, access_key_id: impl Into<String>) -> Self {
        self.access_key_id = Some(access_key_id.into());
        self
    }

    pub fn with_secret_access_key(mut self, secret_access_key: impl Into<String>) -> Self {
        self.secret_access_key = Some(secret_access_key.into());
        self
    }

    pub fn with_force_path_style(mut self, force_path_style: bool) -> Self {
        self.force_path_style = force_path_style;
        self
    }
}

pub fn build_s3_object_store(
    config: S3ObjectStoreConfig,
) -> Result<Arc<dyn ObjectStore>, object_store::Error> {
    let mut builder = AmazonS3Builder::new()
        .with_bucket_name(config.bucket)
        .with_region(config.region)
        .with_virtual_hosted_style_request(!config.force_path_style);
    if let Some(endpoint) = config.endpoint {
        let allow_http = endpoint.starts_with("http://");
        builder = builder.with_endpoint(endpoint).with_allow_http(allow_http);
    }
    if let Some(access_key_id) = config.access_key_id {
        builder = builder.with_access_key_id(access_key_id);
    }
    if let Some(secret_access_key) = config.secret_access_key {
        builder = builder.with_secret_access_key(secret_access_key);
    }
    Ok(Arc::new(builder.build()?))
}
