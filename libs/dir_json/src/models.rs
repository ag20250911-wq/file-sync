use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use chrono::{DateTime, Utc};
use std::{collections::HashMap, fs::File, path::{Path, PathBuf}, time::SystemTime};

use serde_json::Result;

// キャッシュ用のエントリ (サイズと更新日時で変更を検知)
#[derive(Clone)]
pub struct FileCacheEntry {
    pub size: u64,
    pub modified_ts: i64, // UNIXタイムスタンプ
    pub hash: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_partial: bool, // 転送中フラグ
    pub size: u64,
    pub modified_at: Option<DateTime<Utc>>,
    pub hash: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct FileList{
    pub items: Vec<FileItem>,
}

impl FileList {

    // 指定フォルダ内の全ファイル、フォルダを取得しメンバに保存
    // キャッシュマップを受け取り、ハッシュ計算を最適化する
    pub fn scan<P: AsRef<Path>>(root: P, cache: &mut HashMap<PathBuf, FileCacheEntry>) -> Self {
        let root = root.as_ref();
        let mut items = Vec::new();

        if !root.exists(){
            return Self {items};
        }

        let walker = WalkDir::new(root)
        .min_depth(1)
        .into_iter();

        for entry_result in walker{
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("Access denied or error: {}", e);
                    continue;
                }                
            };

            let file_name = entry.file_name().to_string_lossy();

            // メタファイルは無視
            if file_name.ends_with(".mysync_meta") {
                continue;
            }

            let mut is_partial = false;

            // 部分ファイルならフラグを立てる
            if file_name.ends_with(".mysync_partial") || file_name.ends_with(".mysync_partial.hash") {
                is_partial = true;
            }

            let path_buf = entry.path().to_path_buf();
            let metadata = entry.metadata().ok();
            let is_dir = entry.file_type().is_dir();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            
            // 更新日時取得
            let modified_system = metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);

            let modified_at: Option<DateTime<Utc>> = Some(modified_system.into());
            let modified_ts = DateTime::<Utc>::from(modified_system).timestamp();

            let mut hash = None;

            // ファイルの場合のみハッシュ計算
            if !is_dir && !is_partial {
                let mut needs_recalc = true;

                // キャッシュチェック
                if let Some(cached) = cache.get(&path_buf) {
                    // サイズと更新日時(秒単位)が一致していればハッシュを再利用
                    if cached.size == size && cached.modified_ts == modified_ts {
                        hash = Some(cached.hash.clone());
                        needs_recalc = false;
                    }
                }

                // 必要ならハッシュ再計算
                if needs_recalc {
                    // 計算に失敗した場合（読み込み不可など）はhash=Noneのまま進む
                    if let Some(new_hash) = Self::compute_file_hash(&path_buf) {
                        hash = Some(new_hash.clone());
                        
                        // キャッシュ更新
                        cache.insert(path_buf, FileCacheEntry {
                            size,
                            modified_ts,
                            hash: new_hash,
                        });
                    }
                }
            }

            // 相対パス生成
            let relative_path = entry.path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace("\\", "/");

            items.push(FileItem{
                name: file_name.to_string(),
                path: relative_path,
                is_dir,
                is_partial,
                size,
                modified_at,
                hash
            });
        }

        items.sort_by(|a,b| a.path.cmp(&b.path));

        Self{items}
    }

    // result<String> のjsonを返す
    pub fn to_json(&self) -> Result<String>{
        serde_json::to_string_pretty(&self)
    }

    // BLAKE3ハッシュ計算用ヘルパー
    pub fn compute_file_hash(path: &Path) -> Option<String> {
        let file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        let len = metadata.len();

        // 0バイトファイルはmmapできないので特別扱い
        if len == 0 {
            let hash = blake3::hash(&[]);
            return Some(hex::encode(hash.as_bytes()));
        }

        let mmap = unsafe {
            match memmap2::MmapOptions::new().map(&file){
                Ok(m) => m,
                Err(_) => return None,
            }
        };

        let hash = blake3::hash(&mmap);

        Some(hex::encode(hash.as_bytes()))
    }

}