use serde::{Deserialize, Serialize};

pub const PACKAGE_STATE_SCHEMA_V1: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledPackageStateV1 {
    pub schema: u32,
    pub app_id: String,
    pub package: String,
    pub current_content_id: String,
    #[serde(default)]
    pub previous_content_id: Option<String>,
    pub version: String,
}
