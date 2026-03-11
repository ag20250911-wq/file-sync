use regex::Regex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use dotenvy::dotenv;
use tokio::{fs, io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, AsyncSeekExt}, stream};
use std::{collections::HashMap, env, path::PathBuf, path::Path, process::Command, time::{Duration, Instant, SystemTime}};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dir_json::{FileItem,FileList};

use file_sync_proto::{self, DownloadStartResponse, UploadResponse, TransferCommand};

#[derive(Serialize, Clone)]
struct ConnectionStatus{
    connected: bool,
    message: String,
    retry_in: Option<u32>,
}

// フロントエンドへの通知用ペイロード
#[derive(Serialize, Clone)]
pub struct ProgressPayload{
    pub path: String,
    pub total: u64,
    pub current: u64,
    pub percent: u8,
    pub speed: u64, // 1秒当たりのバイト数
}

use chrono::{DateTime, Utc};

// --- ダウンロード状態管理用の構造体 ---
#[derive(Serialize, Deserialize, Debug, Clone)]
struct DownloadStateItem {
    server_hash: String, // ハッシュで管理
}

// 状態ファイルのパス
const STATE_FILENAME: &str = "downloading_state.json";
// ローカルキャッシュ用の構造体 ハッシュなどを持つ
const LOCAL_CACHE_FILENAME: &str = "local_file_cache.json";

#[derive(Serialize, Deserialize, Debug, Clone)]
struct LocalCacheEntry {
    size: u64,
    modified_ts: i64, // UNIXタイムスタンプ(秒)
    hash: String,
}

// キャッシュ読み込み
async fn load_local_cache() -> HashMap<String, LocalCacheEntry> {
    if let Ok(content) = tokio::fs::read_to_string(LOCAL_CACHE_FILENAME).await {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        HashMap::new()
    }
}

// キャッシュ保存
async fn save_local_cache(cache: &HashMap<String, LocalCacheEntry>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = tokio::fs::write(LOCAL_CACHE_FILENAME, json).await;
    }
}

// ローカルファイルの状態を持つ
#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileStatusDetail {
    status: String,             // "synced", "outdated"
    local_hash: Option<String>, // 計算されたハッシュ。計算スキップ時は None
    local_size: u64,            // ローカルファイルのサイズ
}


// ヘルパー関数: SystemTime から UNIX timestamp (i64) を取得
fn to_timestamp(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}


async fn load_download_state() -> HashMap<String, DownloadStateItem> {
    if let Ok(content) = tokio::fs::read_to_string(STATE_FILENAME).await {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        HashMap::new()
    }
}

async fn save_download_state(state: &HashMap<String, DownloadStateItem>) {
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = tokio::fs::write(STATE_FILENAME, json).await;
    }
}

// キャンセルコマンド
#[derive(Clone)]
struct TransferManager {
    cancel_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl Default for TransferManager {
    fn default() -> Self {
        Self {
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// 転送終了時（成功・エラー問わず）にフラグマップから削除するためのガード
struct CancelGuard {
    path: String,
    manager: TransferManager,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Ok(mut flags) = self.manager.cancel_flags.lock() {
            flags.remove(&self.path);
        }
    }
}

// フロントエンドから呼ばれるキャンセルコマンド
#[tauri::command]
fn cancel_transfer(path: String, manager: tauri::State<'_, TransferManager>) -> Result<(), String> {
    if let Ok(flags) = manager.cancel_flags.lock() {
        if let Some(flag) = flags.get(&path) {
            flag.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

// アップロード履歴 アップロード再開のために
const UPLOAD_HISTORY_FILENAME: &str = "upload_history.json";

#[tauri::command]
async fn get_upload_history() -> Result<HashMap<String, String>, String> {
    if let Ok(content) = tokio::fs::read_to_string(UPLOAD_HISTORY_FILENAME).await {
        Ok(serde_json::from_str(&content).unwrap_or_default())
    } else {
        Ok(HashMap::new())
    }
}

#[tauri::command]
async fn save_upload_history(remote_path: String, local_path: String) -> Result<(), String> {
    let mut history = get_upload_history().await.unwrap_or_default();
    history.insert(remote_path, local_path);
    let json = serde_json::to_string_pretty(&history).map_err(|e| e.to_string())?;
    tokio::fs::write(UPLOAD_HISTORY_FILENAME, json).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn remove_upload_history(remote_path: String) -> Result<(), String> {
    let mut history = get_upload_history().await.unwrap_or_default();
    if history.remove(&remote_path).is_some() {
        if let Ok(json) = serde_json::to_string_pretty(&history) {
            let _ = tokio::fs::write(UPLOAD_HISTORY_FILENAME, json).await;
        }
    }
    Ok(())
}

#[tauri::command]
fn check_local_file_exists(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

// --- ドロップアップロード用の構造体とコマンド ---
#[derive(serde::Serialize)]
struct UploadItem {
    local_path: String,
    remote_path: String,
}

#[tauri::command]
async fn prepare_upload_items(paths: Vec<String>, target_dir: String) -> Result<Vec<UploadItem>,String> {
    let mut items = Vec::new();
    let base_remote = target_dir.trim_matches('/');

    for path_str in paths {
        let path = std::path::Path::new(&path_str);
        if !path.exists() { continue; }

        if path.is_file() {
            // ファイル単体の場合
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let remote_path = if base_remote.is_empty() {
                name
            } else {
                format!("{}/{}", base_remote, name)
            };
            items.push(UploadItem {
                local_path: path_str.clone(),
                remote_path,
            });
        } else if path.is_dir() {
            // フォルダがドロップされた場合、中身を再帰的に取得
            if let Some(parent) = path.parent() {
                for entry in walkdir::WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                    let entry_path = entry.path();
                    if entry_path.is_file() {
                        // 親フォルダからの相対パスを計算（例: "folder/sub/a.txt"）
                        if let Ok(rel) = entry_path.strip_prefix(parent) {
                            let rel_str = rel.to_string_lossy().replace("\\", "/");
                            let remote_path = if base_remote.is_empty() {
                                rel_str
                            } else {
                                format!("{}/{}", base_remote, rel_str)
                            };

                            items.push(UploadItem { local_path: entry_path.to_string_lossy().to_string(), remote_path, });
                        }

                    }
                }
            }            
        }
    }
    Ok(items)
}




#[tauri::command]
async fn fetch_ip() -> Result<String,String>{
    dotenv().ok();

    let ip_url = match env::var("SERVER_IP_URL"){
        Ok(url) => url,
        Err(_) => return Ok("w4090".to_string()),
        //Err(_) => return Ok("127.0.0.1".to_string()),
    };

    let html = reqwest::get(&ip_url)
    .await
    .map_err(|e| format!("失敗: {}", e))?
    .text()
    .await
    .map_err(|e| format!("テキスト失敗: {}", e))?;

    let re = Regex::new(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}").unwrap();
    let ip = re.find(&html)
    .map(|m|m.as_str())
    .ok_or_else(|| "err regex")?;

    Ok(ip.to_string())
}

// remember to call `.manage(MyState::default())`
#[tauri::command]
async fn start_tcp_client(app: AppHandle, ip:String) -> Result<(), String> {
  let addr = format!("{}:44444", ip);

  tauri::async_runtime::spawn(async move {
    loop {
        println!("TCP接続を試行中: {}", addr);
        let Ok(mut stream) = tokio::net::TcpStream::connect(&addr).await else {
            println!("接続失敗。再執行します...");

            for i in (1..=10).rev(){
                let _ = app.emit("tcp-status", ConnectionStatus{
                connected:false,
                message: "retrying".into(),
                retry_in: Some(i),
            });
            tokio::time::sleep(Duration::from_secs(1)).await;
            }
            continue;
        }; 

        println!("TCP接続成功");
        let _ = app.emit("tcp-status", ConnectionStatus{
            connected:true,
            message: "connected".into(),
            retry_in: None,
        });
        let mut buf: [u8; 64] = [0;64];

        // 切断監視ループ
        loop {
            let Ok(n) = stream.read(&mut buf).await else{ break;};
            if n == 0 {
                println!("取得バイト数0");
                break;
            } else{
                println!("取得バイト数{:?}", n);
                let msg = String::from_utf8_lossy(&buf[0..n]);
                println!("受信データ: {}", msg);

                if msg.contains("REFRESH"){
                    println!("リフレッシュ要求を検知。フロントエンドへ通知します。");
                    let _ = app.emit("refresh-file-list", ());
                }
            }

        }

        println!("切断されました。再接続へ移行します。");
        let _ = app.emit("tcp-status", ConnectionStatus{
            connected: false,
            message: "disconnected".into(),
            retry_in: None,
        });
        
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
  });
  Ok(())
}

#[tauri::command]
async fn get_file_list(url: String) -> Result<Vec<FileItem>, String>{
    let client = reqwest::Client::new();
    let res = client.get(url)
    .send()
    .await
    .map_err(|e| e.to_string())?;

    let data:FileList =res.json()
    .await
    .map_err(|e|e.to_string())?;
    
    Ok(data.items)
}

#[tauri::command]
async fn tcp_download_folder(app: AppHandle, ip: String, files: Vec<FileItem>, save_dir: String) -> Result<String, String>{
    let total_files = files.len();

    for (i, file_info) in files.into_iter().enumerate() {
        // 進捗状況をコンソールに出す
        println!("処理中 ({}/{}): {}", i + 1, total_files, file_info.path);

        // エラーハンドリングを追加（?演算子でエラーなら即終了してフロントへ通知）
        tcp_download_file(
            app.clone(), 
            ip.clone(), 
            file_info.path.clone(), 
            save_dir.clone(), 
            file_info.hash.clone(), 
            file_info.size,
            false // フォルダDL時は強制上書きしない (同期モード)
        )
            .await
            .map_err(|e| format!("ファイル '{}' でエラー発生: {}", file_info.path, e))?;
            
        // ポート枯渇を防ぐため、ごく短いウェイトを入れる（連続接続エラー対策）
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Ok(format!("フォルダ内の全 {} 個のファイルの処理が完了しました", total_files))
}

// ステータスを返すための構造体やEnumではなく、単純に文字列で返します
// "synced": 完全一致 (サイズ一致 かつ ローカルの方が新しいor同じ)
// "outdated": サーバー側が更新されている (サイズ不一致 または サーバーの方が新しい)
// キーが存在しない: ローカルに無い
#[tauri::command]
async fn check_file_status(
    files: Vec<FileItem>, // 単なるStringではなく詳細情報を受け取る
    save_dir: String,
) -> Result<HashMap<String, FileStatusDetail>, String> {
    let mut status_map = HashMap::new();
    let base_path = PathBuf::from(save_dir);

    // キャッシュをロード
    let mut cache = load_local_cache().await;
    let mut cache_dirty = false;

    // ダウンロード状態(レジューム情報)をロード
    let download_state = load_download_state().await;

    for file_info in files {
        // パス処理
        let relative = file_info.path.trim_start_matches(|c| c == '/' || c == '\\');
        let local_path = base_path.join(relative);
        let abs_path_key = local_path.to_string_lossy().to_string(); // 状態照合用キー

        if file_info.is_dir { 
            if local_path.exists() {
                status_map.insert(file_info.path.clone(), FileStatusDetail {
                    status: "synced".to_string(),
                    local_hash: None,
                    local_size: 0,
                });
            }
            continue; 
        }

        let cache_key = local_path.to_string_lossy().to_string(); // 絶対パスをキーにする

        if local_path.exists() {
            let cache_key = abs_path_key.clone();
            // ファイルの場合、メタデータを取得して比較
            if let Ok(metadata) = tokio::fs::metadata(&local_path).await {
                let local_size = metadata.len();
                let local_mtime = to_timestamp(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));

                // ローカルファイルに変更がなければ(サイズと時刻が一致すれば)、ハッシュを取り出しておく
                let mut resolved_local_hash = None;
                
                // 以前計算した情報がキャッシュにあるか？
                if let Some(entry) = cache.get(&cache_key) {
                    if entry.size == local_size && entry.modified_ts == local_mtime {
                        resolved_local_hash = Some(entry.hash.clone());
                    }
                }

                // サイズ違い
                if local_size != file_info.size {
                    status_map.insert(file_info.path, FileStatusDetail {
                        status: "outdated".to_string(),
                        local_hash: resolved_local_hash,
                        local_size,
                    });
                    continue;
                }

                // サイズが同じ場合サーバーのファイルハッシュと比較
                if let Some(server_hash) = &file_info.hash {
                    let mut local_hash_str = String::new();

                    // キャッシュが存在するかチェック
                    if let Some(h) = resolved_local_hash {
                        local_hash_str = h;
                    } else {
                        // キャッシュがない、または無効の場合再計算
                        if let Some(h) = dir_json::FileList::compute_file_hash(&local_path){
                            local_hash_str = h.clone();
                            cache.insert(cache_key, LocalCacheEntry { size: local_size, modified_ts: local_mtime, hash: h.clone(), });
                            cache_dirty = true;
                        } else {
                            continue;
                        }
                    }

                    // サーバーと比較
                    let status = if &local_hash_str != server_hash {
                        "outdated".to_string()
                    } else {
                        "synced".to_string()
                    };

                    status_map.insert(file_info.path, FileStatusDetail { status, local_hash: Some(local_hash_str), local_size });
                } else {
                    // サーバー側がハッシュ未計算ならサイズ一致で同期とみなす
                    status_map.insert(file_info.path, FileStatusDetail { status:  "synced".to_string(), local_hash: resolved_local_hash, local_size });
                }
            }
        } else {
            // ファイルが存在しない場合一時ファイルがあるかチェック
            // 一時ファイル名の生成
            let mut temp_path_name = local_path.file_name().unwrap().to_os_string();
            temp_path_name.push(".download");
            let temp_path = local_path.with_file_name(temp_path_name);

            if temp_path.exists() {
                // ダウンロード状態ファイルに記録があるか？
                if let Some(state) = download_state.get(&abs_path_key) {
                    // サーバーの現在のハッシュと、ダウンロード開始時のハッシュが一致するか？
                    // (一致する場合のみレジューム可能とみなす)
                    let is_resumable = if let Some(srv_hash) = &file_info.hash {
                        &state.server_hash == srv_hash
                    } else {
                        false
                    };

                    if is_resumable {
                        // サイズを取得して進捗を表示できるようにする
                        if let Ok(meta) = tokio::fs::metadata(&temp_path).await {
                             status_map.insert(file_info.path, FileStatusDetail { 
                                status: "partial".to_string(),
                                local_hash: None, 
                                local_size: meta.len() // ここには途中までのサイズが入る
                            });
                        }
                    }
                }
            }
        }
    }

    if cache_dirty {
        save_local_cache(&cache).await;
    }

    Ok(status_map)
}

// 強制ハッシュ再計算
#[tauri::command]
async fn recalc_file_hash(save_dir: String, file_path: String) -> Result<String, String> {
    let relative = file_path.trim_start_matches(|c| c == '/' || c == '\\');
    let path = PathBuf::from(&save_dir).join(relative);
    let cache_key = path.to_string_lossy().to_string();

    if !path.exists() {
        return Err("ファイルが見つかりません".to_string());
    }

    // ハッシュを計算
    let new_hash = dir_json::FileList::compute_file_hash(&path)
        .ok_or("ハッシュ計算に失敗しました")?;

    // 計算できたのでキャッシュを強制更新
    if let Ok(meta) = tokio::fs::metadata(&path).await {
        let size = meta.len();
        let mtime = to_timestamp(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        
        let mut cache = load_local_cache().await;
        cache.insert(cache_key, LocalCacheEntry {
            size,
            modified_ts: mtime,
            hash: new_hash.clone(),
        });

        save_local_cache(&cache).await;
    }

    Ok(new_hash)
}

#[tauri::command]
async fn tcp_download_file(
    app: AppHandle, 
    ip: String, 
    file_path: String, 
    save_dir: String, 
    server_hash: Option<String>, 
    server_size: u64, 
    force: bool
) -> Result<String, String>{
    let addr = format!("{}:44445", ip);

    // パス生成
    let relative_path_str = file_path.trim_start_matches(|c| c == '/' || c == '\\');
    let final_path = PathBuf::from(&save_dir).join(relative_path_str);

    // 一時ファイルパス .download を付与
    let mut temp_path_name = final_path.file_name().unwrap().to_os_string();
    temp_path_name.push(".download");
    let temp_path = final_path.with_file_name(temp_path_name);

    // キャッシュ用キー
    let abs_path_key = final_path.to_string_lossy().to_string(); 


    // ディレクトリ作成
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).await.map_err(|e| format!("ディレクトリ作成失敗: {}", e))?;
    }

    // 既にサーバー側と一致するファイルがローカル側にあればダウンロードをスキップ
    if final_path.exists() && !force {
        // 既存ファイルのサイズ確認
        if let Ok(meta) = tokio::fs::metadata(&final_path).await {
            // ファイルサイズが一致するか
            if meta.len() == server_size {
                // サーバーのハッシュとローカルキャッシュのハッシュと比較する
                if let Some(srv_h) = &server_hash {
                    let cache = load_local_cache().await;
                    if let Some(entry) = cache.get(&abs_path_key) {
                        if &entry.hash == srv_h && entry.size == server_size {
                            return Ok("スキップ: 同期済み".to_string());
                        }
                    }
                } else {
                    // サーバー側のハッシュがない場合はサイズ一致だけでスキップとみなす
                    return Ok("スキップ: サイズ一致 (ハッシュなし)".to_string());                    
                }
            }
        }
    }


    // --- レジューム判定ロジック ---
    let mut download_state_map = load_download_state().await;

    let mut current_offset = 0u64;
    let mut needs_truncate = true;

    // サーバーからハッシュが提供されている場合のみレジュームを試みる
    if let Some(srv_h) = &server_hash {
        // 一時ファイルが存在し、かつ状態ファイルに記録があるか
        if temp_path.exists() {
            if let Some(saved_state) = download_state_map.get(&abs_path_key) {
                // 保存されたハッシュと、現在のサーバーハッシュが一致するか？
                if &saved_state.server_hash == srv_h {
                    // 一致する場合、一時ファイルのサイズを確認してオフセットにする
                    if let Ok(meta) = tokio::fs::metadata(&temp_path).await {
                        let local_len = meta.len();
                        if local_len < server_size {
                            println!("レジューム有効: {} ({} bytes)", file_path, local_len);
                            current_offset = local_len;
                            needs_truncate = false;
                        } else if local_len == server_size {
                            // 既に完了してる リネームが出来てない
                            current_offset = local_len;
                            needs_truncate = false;
                        }
                    }
                } else {
                    println!("ハッシュ不一致 (Server Changed): 最初からダウンロードします");
                }
            }
        }
    } else {

        println!("サーバーハッシュなし: レジューム不可のため最初からダウンロードします");
    }

    // 新規ダウンロードの場合、ステートを保存
    if needs_truncate && server_hash.is_some() {

        download_state_map.insert(abs_path_key.clone(), DownloadStateItem {
            server_hash: server_hash.clone().unwrap(),
        });
        save_download_state(&download_state_map).await;
    }

    
    // キャンセルフラグのセットとガードの作成
    let manager = app.state::<TransferManager>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut flags = manager.cancel_flags.lock().unwrap();
        flags.insert(file_path.clone(), cancel_flag.clone());
    }
    let _guard = CancelGuard { path: file_path.clone(), manager: manager.inner().clone() };


    // --- ファイルオープン & TCP接続 ---
    let mut file_opts = tokio::fs::OpenOptions::new();
    file_opts.write(true).create(true);
    
    if needs_truncate {
        file_opts.truncate(true); // ファイルを空にする
    } else {
        file_opts.append(true);   // 追記モード
    }

    let mut file = file_opts.open(&temp_path).await.map_err(|e| format!("Temp file open error: {}", e))?;

    let mut stream = tokio::net::TcpStream::connect(&addr).await.map_err(|e| format!("サーバー接続失敗: {}", e))?;
    
    // コマンド送信
    let command = TransferCommand::Download { path: file_path.clone(), offset: current_offset };
    let json_req = serde_json::to_string(&command).map_err(|e|e.to_string())? + "\n";
    
    // ダウンロードするファイルのパスとダウンロード開始位置をサーバーに送る
    stream.write_all(json_req.as_bytes()).await.map_err(|e| e.to_string())?;

    let (reader, _) = stream.split();
    let mut buf_reader = tokio::io::BufReader::new(reader);

    // サーバーからヘッダー受信
    let mut header_line = String::new();
    buf_reader.read_line(&mut header_line).await.map_err(|e| format!("ヘッダー受信エラー: {}", e))?;
    
    // 空レスポンス等のチェック
    if header_line.trim().is_empty() {
        return Err("サーバーから空の応答が返ってきました".to_string());
    }
    
    let header: DownloadStartResponse = serde_json::from_str(&header_line)
        .map_err(|e| format!("ヘッダー解析エラー (受信データ: {}): {}", header_line, e))?;

    if !header.found {
        return Err(format!("サーバー上でファイルが見つかりません: {}", file_path));
    }

    if current_offset >= header.size && header.size > 0 {
        // ダウンロード処理はスキップしてリネームへ
    } else {
        //　ダウンロードループ
        let mut buffer = vec![0u8; 128 *1024]; // 128KB
        let mut downloaded = current_offset;
        let total = header.size;

        // 転送速度計算用の変数
        let mut last_emit = Instant::now();
        let mut last_downloaded = downloaded; // 前回のバイト数
        let mut speed: u64 = 0;

        println!("ダウンロード開始: {} bytes -> Temp: {:?}", total, temp_path);

        loop {
            // キャンセルされたかチェック
            if cancel_flag.load(Ordering::Relaxed) {
                return Err("キャンセルされました".to_string());
            }

            let n = buf_reader.read(&mut buffer).await.map_err(|e| e.to_string())?;
            if n == 0 { break; }

            file.write_all(&buffer[0..n]).await.map_err(|e| e.to_string())?;
            downloaded += n as u64;

            // 進捗通知 (約200msごと)
            let now = Instant::now();
            let elapsed = now.duration_since(last_emit);

            if elapsed.as_millis() > 200 || downloaded == total {
                let bytes_diff = downloaded  - last_downloaded;
                let secs = elapsed.as_secs_f64();

                if secs > 0.0 {
                    speed = (bytes_diff as f64 /secs) as u64;
                }
                
                let _ = app.emit("transfer-progress", ProgressPayload{
                    path: file_path.clone(),
                    total,
                    current: downloaded,
                    percent: if total > 0 { ((downloaded as f64 / total as f64) * 100.0) as u8 } else { 100 },
                    speed,
                });

                last_emit = Instant::now();
                last_downloaded = downloaded;

                // ここでタスクを一旦譲り、Tauriのイベント送信処理が走る隙間を作る
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }

        file.flush().await.map_err(|e| format!("書き込みフラッシュエラー: {}", e))?;

        // サイズチェック (TCP切断などで途中終了した場合の検知)
        let final_temp_size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
        if final_temp_size != total {
            return Err(format!("中断: {}/{} bytes", final_temp_size, total));
        }
    }

    // --- 完了処理 (リネーム & ステート削除) ---
    
    // ファイルハンドルを閉じる (明示的にdropしてロック解放)
    drop(file);

    // リネーム実行 (temp -> final)
    // Windowsでは同名ファイルがあるとrenameが失敗する場合があるので、forceなら先に消す
    if final_path.exists() {
        if force {
            let _ = tokio::fs::remove_file(&final_path).await;
        } else {
            return Err("保存先に同名ファイルが既に存在します".to_string());
        }
    }

    if let Err(e) = tokio::fs::rename(&temp_path, &final_path).await {
        return Err(format!("リネーム失敗 ({:?} -> {:?}): {}", temp_path, final_path, e));
    }
    

    // 正常完了したらステートから削除
    let mut state = load_download_state().await;
    state.remove(&abs_path_key);
    save_download_state(&state).await;

    // ローカルキャッシュを更新
    // サーバーから送られてきたハッシュ、もしくは引数のハッシュを使用
    let hash_to_cache = header.hash.or(server_hash);

    if let Some(hash) = hash_to_cache {
        if let Ok(meta) = tokio::fs::metadata(&final_path).await {
            let mtime = to_timestamp(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
            let size  = meta.len();

            let mut local_cache = load_local_cache().await;
            local_cache.insert(abs_path_key, LocalCacheEntry { size, modified_ts: mtime, hash });
            save_local_cache(&local_cache).await;
        }
    }
    
    Ok("ダウンロード完了".to_string())
}

#[tauri::command]
async fn tcp_upload_file(
    app: AppHandle, 
    ip: String, 
    local_path: String, 
    remote_path: String, 
    auth_key: Option<String>
) -> Result<String, String>{
    let addr = format!("{}:44445", ip);

    // ローカル側のアップロードするファイルを開く
    let mut file = tokio::fs::File::open(&local_path).await
    .map_err(|e| format!("ローカルファイルが開けません: {}", e))?;

    let metadata = file.metadata().await.map_err(|e| e.to_string())?;
    let total_size = metadata.len();

    // サーバーに接続
    let mut stream = tokio::net::TcpStream::connect(&addr).await
    .map_err(|e| format!("サーバー接続失敗: {}", e))?;


    // ハッシュを計算
    let local_hash = dir_json::FileList::compute_file_hash(Path::new(&local_path));

    // サーバーに保存先とファイルサイズとハッシュを送信
    let command = TransferCommand::Upload { path: remote_path.clone(), total_size, auth_key, hash: local_hash, };
    let json_req = serde_json::to_string(&command).map_err(|e| e.to_string())? + "\n";
    stream.write_all(json_req.as_bytes()).await.map_err(|e| e.to_string())?;

    let (reader, mut writer) = stream.split();
    let mut buf_reader = tokio::io::BufReader::new(reader);

    // サーバーからアップロードの開始位置を取得
    let mut response_line = String::new();
    buf_reader.read_line(&mut response_line).await.map_err(|e| format!("サーバー応答エラー: {}", e))?;

    if response_line.trim().is_empty() {
        return  Err("サーバーから空の応答が返ってきました".to_string());
    }

    let response: UploadResponse = serde_json::from_str(&response_line)
    .map_err(|e| format!("レスポンス解析エラー: {}", e))?;

    // 拒否された場合のハンドリング
    if response.status == "denied" {
        return Err(response.message.unwrap_or("Access Denied".to_string()));
    }

    println!("サーバー応答: {:?} (開始位置): {}", response.status, response.start_offset);

    // 既にアップロードされてるかチェック
    if response.start_offset >= total_size {
        // 100%通知を一瞬出して終了
        let _ = app.emit("transfer-progress", ProgressPayload{
            path: remote_path.clone(),
            total: total_size,
            current: total_size,
            percent: 100,
            speed: 0,

        });
        return Ok("既にサーバーに同サイズ以上のファイルが存在します".to_string());
    }

    // ファイルをシーク
    file.seek(tokio::io::SeekFrom::Start(response.start_offset)).await
    .map_err(|e| format!("シークエラー: {}", e))?;

    let mut buffer = vec![0u8; 128 *1024]; // 128KB
    let mut uploaded = response.start_offset;

    // 転送速度計算用の変数
    let mut last_uploaded = uploaded; // 前回通知時のバイト数
    let mut last_emit = Instant::now();

    // キャンセルフラグのセットとガードの作成
    let manager = app.state::<TransferManager>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut flags = manager.cancel_flags.lock().unwrap();
        // アップロードの場合はローカルパスをキーにする
        flags.insert(local_path.clone(), cancel_flag.clone());
    }
        let _guard = CancelGuard { path: local_path.clone(), manager: manager.inner().clone() };

    loop {
        // キャンセルされたかチェック
        if cancel_flag.load(Ordering::Relaxed) {
            return Err("キャンセルされました".to_string());
        }


        let n = file.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if n == 0 { break; }

        writer.write_all(&buffer[0..n]).await.map_err(|e| e.to_string())?;
        uploaded += n as u64;

        // 進捗通知 (200msごと または 完了時)
        let now = Instant::now();
        let elapsed = now.duration_since(last_emit);

        if elapsed.as_millis() > 200 || uploaded == total_size {
            let bytes_diff = uploaded - last_uploaded; // この期間に送った量
            let secs = elapsed.as_secs_f64();          // 経過秒数

            let speed = if secs > 0.0 {
                (bytes_diff as f64 / secs) as u64
            } else {
                0
            };

            let _ = app.emit("transfer-progress", ProgressPayload{
                path: local_path.clone(),
                total: total_size,
                current: uploaded,
                percent: if total_size > 0 { ((uploaded as f64 / total_size as f64) * 100.0) as u8 } else { 100 },
                speed,
            });
            last_emit = Instant::now();
            last_uploaded = uploaded;

            // UIイベントループを回すための微小ウェイト
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    writer.flush().await.map_err(|e| e.to_string())?;
    Ok("アップロード完了".to_string())
}

#[tauri::command]
async fn tcp_create_dir(ip: String, path: String, auth_key: Option<String>) -> Result<String, String> {
    let addr = format!("{}:44445", ip);
    let mut stream = tokio::net::TcpStream::connect(&addr).await
        .map_err(|e| format!("接続失敗: {}", e))?;

    let command = TransferCommand::Mkdir { path, auth_key };
    let json = serde_json::to_string(&command).map_err(|e| e.to_string())? + "\n";
    
    stream.write_all(json.as_bytes()).await.map_err(|e| e.to_string())?;

    // シンプルなテキスト応答待ち
    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
    
    let response = String::from_utf8_lossy(&buf[..n]).to_string();
    
    // "権限エラー" という文字列が含まれていたらエラー扱いにするなどの処理
    if response.contains("権限エラー") {
        return Err(response);
    }

    Ok(response)
}

#[tauri::command]
async fn tcp_remove_item(ip: String, path: String, auth_key: Option<String>) -> Result<String, String> {
    let addr = format!("{}:44445", ip);
    let mut stream = tokio::net::TcpStream::connect(&addr).await
        .map_err(|e| format!("接続失敗: {}", e))?;

    let command = TransferCommand::Remove { path, auth_key };
    let json = serde_json::to_string(&command).map_err(|e| e.to_string())? + "\n";

    stream.write_all(json.as_bytes()).await.map_err(|e| e.to_string())?;

    // シンプルなテキスト応答待ち
    let mut buf = vec![0u8; 1024];
    let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
    
    let response = String::from_utf8_lossy(&buf[..n]).to_string();
    
    // "権限エラー" という文字列が含まれていたらエラー扱いにするなどの処理
    if response.contains("権限エラー") {
        return Err(response);
    }

    Ok(response)
}

#[tauri::command]
async fn delete_local_item(save_dir: String, path: String) -> Result<(), String> {
    let base_path = PathBuf::from(&save_dir);
    // パスの区切り文字をOSに合わせて調整 (\ or /)
    let relative = path.trim_start_matches(|c| c == '/' || c == '\\');
    let target_path = base_path.join(relative);

    if !target_path.exists() {
        return Err("ローカルにファイルが見つかりません".to_string());
    }

    // ディレクトリかファイルかで削除方法を分ける
    let res = if target_path.is_dir() {
        tokio::fs::remove_dir_all(&target_path).await
    } else {
        tokio::fs::remove_file(&target_path).await
    };

    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("削除失敗: {}", e)),        
    }
}

#[tauri::command]
async fn open_local_item(path: String, mode: String) -> Result<(), String> {
    let path_buf = std::path::PathBuf::from(&path);
    if !path_buf.exists() {
        return Err("ファイルが存在しません".to_string());
    }

    // Windows環境での処理
    #[cfg(target_os = "windows")]
    {
        // explorerは / (スラッシュ) だと正しく動かない場合があるため \ に置換
        let os_path = path.replace("/", "\\");

        match mode.as_str() {
            "reveal" => {
                // ファイルを選択した状態でエクスプローラーを起動
                Command::new("explorer")
                    .arg("/select,")
                    .arg(&os_path)
                    .spawn()
                    .map_err(|e| e.to_string())?;
            }
            "open" => {
                // 関連付けられたアプリでファイルを開く
                Command::new("explorer")
                    .arg(os_path)
                    .spawn()
                    .map_err(|e| e.to_string())?;
            }
            "dir" => {
                // ファイルの親フォルダを開く
                let parent = path_buf.parent().unwrap_or(&path_buf);
                Command::new("explorer")
                    .arg(parent)
                    .spawn()
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err("不明なモードです".to_string()),
        }
    }

    // 他のOS..

    Ok(())
}


#[derive(Serialize, Deserialize)]
struct AppConfig {
    save_dir: String,
}

const CONFIG_FILENAME: &str = "config.json";

#[tauri::command]
async fn load_config() -> Result<AppConfig, String> {
    let path = PathBuf::from(CONFIG_FILENAME);
    if path.exists(){
        let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("設定読み込みエラー: {}", e))?;

        let config: AppConfig = serde_json::from_str(&content)
        .unwrap_or(AppConfig { save_dir: ".\\Download".to_string() });

        Ok(config)
    } else {
        Ok(AppConfig { save_dir: ".\\Download".to_string() })
    }
}

#[tauri::command]
async fn save_config(save_dir: String) -> Result<(), String> {
    let config = AppConfig { save_dir };
    let json_str = serde_json::to_string_pretty(&config)
    .map_err(|e| format!("JSON変換エラー: {}", e))?;

    tokio::fs::write(CONFIG_FILENAME, json_str)
    .await
    .map_err(|e| format!("設定保存エラー: {}", e))?;

    Ok(())
}



#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init()) 
        .manage(TransferManager::default())
        .invoke_handler(tauri::generate_handler![
            get_file_list, 
            fetch_ip,
            start_tcp_client,
            tcp_download_folder,
            tcp_download_file,
            tcp_upload_file,
            open_local_item,
            load_config,
            save_config,
            check_file_status,
            recalc_file_hash,
            tcp_create_dir,
            tcp_remove_item,
            delete_local_item,
            prepare_upload_items,
            cancel_transfer,
            get_upload_history,
            save_upload_history,
            remove_upload_history,
            check_local_file_exists,
            ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
