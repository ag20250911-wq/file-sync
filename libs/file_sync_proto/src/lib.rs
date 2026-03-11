use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "cmd", content = "args")]
pub enum TransferCommand{
    #[serde(rename = "download")]
    Download {
        path: String,
        offset: u64,
    },
    #[serde(rename = "upload")]
    Upload{
        path: String,
        total_size: u64,
        auth_key: Option<String>,
        hash: Option<String>,
    },
    #[serde(rename = "mkdir")]
    Mkdir {
        path: String,
        auth_key: Option<String>,
    },
    #[serde(rename = "remove")]
    Remove {
        path: String,
        auth_key: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
pub struct UploadResponse{
    pub status: String, // "ready" or "denied" 等
    pub start_offset: u64,
    pub message: Option<String>,
}

// ダウンロード要求があればまずこれを返す
#[derive(Serialize, Deserialize)]
pub struct DownloadStartResponse{
    pub found: bool,
    pub size: u64,
    pub hash: Option<String>,
}
