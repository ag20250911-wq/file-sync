use notify_debouncer_mini::new_debouncer;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::time::sleep;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use std::fs::{self, canonicalize};
use std::path::{Component, Components, Path, PathBuf};


use serde_json;
use file_sync_proto::{self, DownloadStartResponse, TransferCommand, UploadResponse};
use dir_json::FileCacheEntry;

use dotenvy::dotenv;
use std::env;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::get;

// 時刻
use chrono::Local;

// ヘルパー関数: 現在時刻の文字列を取得
fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// --- IPブロック機能 ---
// 環境変数: DENY_IPS=80.94.95.0/24,192.168.1.0/24

struct IpFilter {
    rules: Vec<(u32, u32)>, // (network_addr, mask)
}

impl IpFilter {
    fn from_env() -> Self {
        let rules = std::env::var("DENY_IPS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|entry| Self::parse_cidr(entry.trim()))
            .collect();        
        Self { rules }
    }

    fn parse_cidr(cidr: &str) -> Option<(u32, u32)> {
        if cidr.is_empty() { return None; }
        let (ip_str, prefix_len) = if let Some((a, b)) = cidr.split_once('/') {
            (a, b.parse::<u32>().ok()?)
        } else {
            (cidr, 32) // /32 扱い (単一IP)
        };

        let ip: std::net::Ipv4Addr = ip_str.parse().ok()?;
        let mask = if prefix_len == 0 { 0u32 } else { !0u32 << (32 - prefix_len) };
        let network = u32::from(ip) & mask;
        Some((network, mask))
    }

    fn is_blocked(&self, addr: &SocketAddr) -> bool {
        let ipv4 = match addr.ip() {
            std::net::IpAddr::V4(v4) => v4,
            std::net::IpAddr::V6(v6) => {
                // ::ffff:x.x.x.x 形式のIPv4マップアドレスを変換
                match v6.to_ipv4_mapped() {
                    Some(v4) => v4,
                    None => return false,
                }
            }
        };
        let ip_int = u32::from(ipv4);
        self.rules.iter().any(|(network, mask)| ip_int & mask == *network)
    }
}

/*
// ネットワークディスクやRamdiskで使えない
/// クライアントが指定したパスを解析し、シンボリックリンクを含む
/// あらゆるパストラバーサルを防御した安全なパスを生成する。
fn resolve_safe_path_sec(base_dir: &Path, user_path: &str) -> Result<PathBuf, &'static str> {
    // 基準となる base_dir の「本当の絶対パス」を取得する
    let canonical_base = base_dir
        .canonicalize()
        .map_err(|_| "ベースディレクトリの解決に失敗しました")?;

    let mut resolved_path = canonical_base.clone();

    // ユーザー入力をOSのパス構成要素（コンポーネント）に分解して処理
    for component in Path::new(user_path).components() {
        match component {
            // Linuxの "/" や Windowsの "C:\" などの絶対パス指定は無視する（絶対パス攻撃の防御）
            Component::Prefix(_) | Component::RootDir => continue,

            // カレントディレクトリ "./" は何もしない
            Component::CurDir => continue,

            // 親ディレクトリ "../" が来た場合の処理（パストラバーサルの防御）
            Component::ParentDir => {
                if resolved_path == canonical_base {
                    return Err("パストラバーサル攻撃を検知しました");
                }
                resolved_path.pop();
            }

            // 通常のファイル名・フォルダ名
            Component::Normal(name) => {
                resolved_path.push(name);
                // シンボリックリンクの検証（ファイルシステムへの実際の確認）
                // symlink_metadata を使うことで、リンク切れの悪意あるリンクも検知可能。
                if fs::symlink_metadata(&resolved_path).is_ok() {
                    // 対象が存在する場合、それがシンボリックリンクならリンク先を解決する
                    match resolved_path.canonicalize() {
                        Ok(real_path) => {
                            // 解決された「実体パス」が base_dir の外に飛び出していないか確認
                            if !real_path.starts_with(&canonical_base) {
                                return Err("シンボリックリンクによる不正なアクセスを検知しました");
                            }
                            // パスを、リンク名ではなく「安全確認済みの実体パス」に置き換える
                            resolved_path = real_path;
                        }
                        Err(_) => {
                            // リンク先が存在しない壊れたシンボリックリンクなど
                            return Err("無効または危険なシンボリックリンクが含まれています");
                        }                        
                    }
                }
                // 存在しない場合 (is_err) は、これから Upload や Mkdir で作成される
                // 新規ファイル/フォルダとみなす。まだ存在しないのでリンク攻撃の踏み台にはならない。
            }
            
        }
    }

    // 解決された絶対パスから canonical_base 部分を取り除き、
    // 元の `base_dir` に安全な相対パスを結合して返す。
    let relative_to_base = resolved_path
    .strip_prefix(&canonical_base)
    .map_err(|_| "安全なパスの生成に失敗しました")?;

    Ok(base_dir.join(relative_to_base))
}
*/

/// クライアントが指定したパスを解析し、パストラバーサルと
/// シンボリックリンク攻撃を防御した安全なパスを生成する。
fn resolve_safe_path(base_dir: &Path, user_path: &str) -> Result<PathBuf, &'static str> {
    let mut resolved_path = base_dir.to_path_buf();

    // ユーザー入力をOSのパス構成要素（コンポーネント）に分解して処理
    for component in Path::new(user_path).components() {
        match component {
            // 絶対パス攻撃の防御（"C:\" や "/" などの指定は無視する）
            Component::Prefix(_) | Component::RootDir => continue,
            
            // カレントディレクトリ "./" は何もしない
            Component::CurDir => continue,
            
            // 親ディレクトリ "../" が来た場合の処理（パストラバーサルの防御）
            Component::ParentDir => {
                // ベースディレクトリより上に行こうとした場合はハッキングとみなして弾く
                if resolved_path == base_dir {
                    return Err("パストラバーサル攻撃を検知しました");
                }
                resolved_path.pop();
            }
            
            // 通常のファイル名・フォルダ名
            Component::Normal(name) => {
                resolved_path.push(name);

                // シンボリックリンク攻撃の防御
                // ファイルまたはフォルダが実在する場合のみチェック
                if let Ok(meta) = std::fs::symlink_metadata(&resolved_path) {
                    // もしそれがシンボリックリンク（またはジャンクション）だった場合
                    if meta.file_type().is_symlink() {
                        return Err("シンボリックリンクを経由したアクセスはセキュリティのため禁止されています");
                    }
                }
            }
        }
    }

    Ok(resolved_path)
}


struct AccessControl;

impl AccessControl {
    // 書き込み・削除権限のチェック
    // true: 許可, false: 拒否
    fn check(resolved_relative_path: &Path, client_key: Option<&String>) -> bool {
        // 環境変数から管理者パスワードを取得
        // 設定されていなければ「全許可」モードとする（セキュリティレベル低）
        let admin_pass = match std::env::var("ADMIN_PASSWORD") {
            Ok(p) if  !p.is_empty() => p,
            _ => return true,            
        };

        // Pathを文字列化してスラッシュ区切りに統一
        let path_str = resolved_relative_path.to_string_lossy().replace("\\", "/");

        // パスベースの例外判定 (例: Publicフォルダは誰でもOK)
        if path_str.starts_with("Public/") || path_str == "Public" {
            return true
        }

        // パスワード照合
        if let Some(key) = client_key {
            return key == &admin_pass;
        }

        // パスワード不一致、かつ例外フォルダでもない
        false
    }
}

// --- 使用する環境変数 ---
// TARGET_DIR: 監視対象ディレクトリ (デフォルト: ".")
// ADMIN_PASSWORD: ファイル、フォルダの作成 削除 できる権限 設定されてない場合はクライアントが全てのコマンド実行可能  (デフォルト: "")
// BIND_ADDR: サーバーが待機するIPアドレス (デフォルト: "0.0.0.0")
// HTTP_PORT: HTTP JSON API用のポート (デフォルト: 44448)
// HTTP_PATH: HTTP JSON APIのパス (デフォルト: "/files")
// NOTIFY_PORT: 更新通知TCP用のポート (デフォルト: 44444)
// TRANSFER_PORT: ファイル転送TCP用のポート (デフォルト: 44445)
// DEBOUNCE_MS: ファイル変更検知のデバウンス時間(ミリ秒) (デフォルト: 1000)


#[tokio::main]
async fn main(){
    dotenv().ok();

    // -- 環境変数の読み込み .envを読み取る --- 
    let target_dir_str = env::var("TARGET_DIR").unwrap_or_else(|_|".".to_string());
    let bind_addr_str = env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0".to_string());
    let http_port: u16 = env::var("HTTP_PORT").unwrap_or_else(|_| "44448".to_string()).parse().expect("HTTP_PORT must be a number");
    let http_path = env::var("HTTP_PATH").unwrap_or_else(|_| "/files".to_string());
    let notify_port: u16 = env::var("NOTIFY_PORT").unwrap_or_else(|_| "44444".to_string()).parse().expect("NOTIFY_PORT must be a number");
    let transfer_port: u16 = env::var("TRANSFER_PORT").unwrap_or_else(|_| "44445".to_string()).parse().expect("TRANSFER_PORT must be a number");
    let debounce_ms: u64 = env::var("DEBOUNCE_MS").unwrap_or_else(|_| "1000".to_string()).parse().expect("DEBOUNCE_MS must be a number");
 
    // ブロック対象のIPアドレス 内部で.env DENY_IPS を読み取る Arc で共有
    let ip_filter = Arc::new(IpFilter::from_env());
    
    // HTTPパスが / で始まっていない場合は補完
    let http_path = if http_path.starts_with('/'){ http_path } else { format!("/{}", http_path) };

    let target_dir_path = PathBuf::from(target_dir_str);
    // RAMディスク対応のため、canonicalizeを使わずに絶対パス化する
    let absolute_target_dir = if target_dir_path.is_absolute() {
        target_dir_path
    } else {
        std::env::current_dir().expect("カレントディレクトリを取得できません").join(target_dir_path)
    };

    // パス内の "." や ".." を文字列レベルで整理（正規化）する
    let mut target_dir = PathBuf::new();
    for component in absolute_target_dir.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => { target_dir.pop(); }
            _ => target_dir.push(component.as_os_str()),
        }
    }

    if !target_dir.exists() {
        println!("警告: 監視フォルダが存在しないため作成します: {:?}", target_dir);
        std::fs::create_dir_all(&target_dir).expect("監視フォルダの作成に失敗しました");
    }

    let scan_target = target_dir.clone();

    // カレントディレクト表示
    if let Ok(dir) = env::current_dir() {
        println!("実行フォルダ: {}", dir.display());
    }
    println!("監視フォルダ: {}", scan_target.display());

    // ブロードキャストチャンネルの作成
    // 16はキューサイズ 
    let (tx, _rx) = broadcast::channel::<String>(16);
    let tx_for_debouncer = tx.clone();

    // キャッシュとJSONを共有するためのArc
    // キャッシュはスキャン間で保持し続ける必要がある
    let cache_init: HashMap<std::path::PathBuf, FileCacheEntry> = HashMap::new();
    let cache_arc = Arc::new(Mutex::new(cache_init));
    let cache_arc2 = cache_arc.clone();
    
    // JSON保持用 変数で共有するため Arcを作る    
    let json_arc = Arc::new(Mutex::new(String::new())); // 初期値は空文字にしておくか、下記で初回スキャン

    let json_for_debouncer = Arc::clone(&json_arc);

    let scan_target_clone = scan_target.clone();
    let cache_clone = Arc::clone(&cache_arc);
    let json_clone = Arc::clone(&json_arc);

    // 初回スキャン
    {
        let mut cache = cache_clone.lock().unwrap();
        // ここでキャッシュを渡してスキャン
        let list = dir_json::FileList::scan(&scan_target_clone, &mut *cache);
        let mut json_store = json_clone.lock().unwrap();
        *json_store = list.to_json().unwrap();
    }


    // ファイルの監視しJSONを書き換える
    let mut debouncer = new_debouncer(
        Duration::from_millis(debounce_ms), // 1000ミリ秒ごとに通知を受け取る
        move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>|{
        
        let Ok(r) = res.inspect_err(|e| println!("{:?}", e)) else {
            return;
        };
        r.iter().for_each(|f| println!("{:?}", f.path));
        let path_for_scan = scan_target.clone();
        let cache_inner = Arc::clone(&cache_arc); // スレッド内で使うためにClone
        let json_inner = Arc::clone(&json_for_debouncer);   // スレッド内で使うためにClone
        let tx_inner = tx_for_debouncer.clone();

        std::thread::spawn(move || {
            let mut cache = cache_inner.lock().unwrap();
            
            // 通知が来たらキャッシュを使って高速スキャン
            let list = dir_json::FileList::scan(&path_for_scan, &mut *cache);
            
            let mut json_store = json_inner.lock().unwrap();
            *json_store = list.to_json().unwrap();

            // 更新通知を送る
            let _ = tx_inner.send("updated".to_string());            
        });

        }
	).unwrap();
    
    debouncer.watcher().watch(&target_dir, notify::RecursiveMode::Recursive).unwrap();

    // http json公開
    let json_arc_clone_http = Arc::clone(&json_arc); 
    let http_bind_addr = format!("{}:{}", bind_addr_str, http_port);
    let http_display_path = http_path.clone();

    tokio::spawn(async move {
        let app = Router::new()
        .route(&http_path, get(move || async move{            
            let cc = Arc::clone(&json_arc_clone_http);
            cc.lock().unwrap().clone()
        }));

        let listener = tokio::net::TcpListener::bind(&http_bind_addr).await.unwrap();
        println!("[{}] HTTP JSONサーバー起動: http://{}{}", now_str(), http_bind_addr, http_display_path);
        axum::serve(listener, app).await.unwrap();
    });


    // tcpリスナー クライアントの生存チェック json更新をクライアントに通知
    let tx_for_listener = tx.clone();
    let notify_bind_addr = format!("{}:{}", bind_addr_str, notify_port);

    let ip_filter_notify = Arc::clone(&ip_filter);
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(&notify_bind_addr).await.unwrap();
        println!("[{}] 更新通知TCPサーバー起動: {}", now_str(), notify_bind_addr);

        loop {
            let (mut stream, sockaddr) = listener.accept().await.unwrap();
            if ip_filter_notify.is_blocked(&sockaddr) {
                println!("[{}] 接続拒否: {:?}", now_str(), sockaddr);
                continue;
            }
            let mut rx = tx_for_listener.subscribe();

            // 接続してきたソケットを新しいスレッド実行
            tokio::spawn(async move{
                println!("[{}] 通知クライアント接続: {:?}", now_str(), sockaddr);

                // 連続送信の防止
                let mut is_cooldown = false;
                let mut pending_update = false;
                let mut timer = Box::pin(sleep(Duration::from_secs(0)));

                loop{
                    let mut b: [u8; 1] = [0;1];
                    tokio::select! {
                        // ファイルが更新されたので クライアントの通知を送る
                        Ok(_) = rx.recv() =>{
                            if is_cooldown{
                                println!("skip");
                                pending_update = true;
                                // timer リセット
                                timer = Box::pin(sleep(Duration::from_secs(2)));
                                continue;
                            }

                            // 書き込み
                            let Ok(_) = stream.write_all("REFRESH".as_bytes()).await else{ break; };
                            println!("[{}] REFRESH送信実行: {:?}", now_str(), sockaddr);
                            is_cooldown = true;
                            timer = Box::pin(sleep(Duration::from_secs(2)));
                        }

                        // Timerが完了したときにリフレッシュを送信
                        _ = &mut timer, if is_cooldown =>{
                            is_cooldown = false;
                            if !pending_update { continue; }

                            if let Err(_) = stream.write_all("REFRESH".as_bytes()).await { break;}
                            println!("[{}] REFRESH遅延送信実行: {:?}", now_str(), sockaddr);
                            pending_update = false;
                        }

                        // TCPが切断された
                        result = stream.read(&mut b) =>{
                            if matches!(result, Ok(0) | Err(_)){     
                                println!("[{}] 通知クライアント切断: {:?}", now_str(), sockaddr);                           
                                break;
                            }
                        }
                    }
                }
            });
        }
    });


    // TCPコマンド を処理
    let transfer_target_dir = target_dir.clone();
    let transfer_bind_addr = format!("{}:{}", bind_addr_str, transfer_port);

    // キャッシュを参照できるようにクローンを作成
    let transfer_cache_arc = Arc::clone(&cache_arc2);

    tokio::spawn(async move{
        let listener = TcpListener::bind(&transfer_bind_addr).await.unwrap();
        println!("[{}] 転送用TCPサーバー起動: {}", now_str(), transfer_bind_addr);

        loop{
            let (mut stream, sockaddr) = listener.accept().await.unwrap();
            let base_dir = transfer_target_dir.clone();

            println!("[{}] 接続受付: {:?}", now_str(), sockaddr);

            // 個別の接続スレッドにもキャッシュを渡す
            let cache_inner = Arc::clone(&transfer_cache_arc);
            
            let ip_filter_transfer = Arc::clone(&ip_filter);
            tokio::spawn(async move{
                let (reader, mut writer) = stream.split();
                if ip_filter_transfer.is_blocked(&sockaddr) {
                    println!("[{}] 接続拒否: {:?}", now_str(), sockaddr);
                    return;
                }
                
                let mut buf_reader = BufReader::new(reader);
                let mut line = String::new();
                
                // 1行目はコマンド
                // 読み込み上限を1MB (1048576バイト) に設定して読み込む
                let limit = 1048576;
                let Ok(n) = (&mut buf_reader).take(limit).read_line(&mut line).await else { return; };
                
                if n == 0 {
                    return; // クライアントが何も送らずに切断した
                }

                // 2. 1MB読み込んでも改行(\n)に到達しなかった場合の防御 (DoS攻撃対策)
                if !line.ends_with('\n') {
                    println!("[{}] エラー: コマンド行が長すぎます (1MB超過)。送信元: {:?}", now_str(), sockaddr);
                    return;
                }
                
                let command: Result<TransferCommand, _> = serde_json::from_str(&line);

                match command {
                    Ok(TransferCommand::Download { path, offset }) => {
                        // 安全なパスの生成
                        let full_path = match resolve_safe_path(&base_dir, &path) {
                            Ok(p) => p,
                            Err(e) => {
                                println!("[{}] Download拒否 (無効なパス): {} - {}", now_str(), path, e);
                                let header = DownloadStartResponse{ found: false, size: 0 , hash: None };
                                let header_json = serde_json::to_string(&header).unwrap() + "\n";
                                writer.write_all(header_json.as_bytes()).await.unwrap();
                                return;
                            }
                        };

                        println!("[{}] ダウンロード要求: {:?} (from {:?})", now_str(), full_path, sockaddr);

                        // 指定パスのファイルを開く
                        if let Ok(mut file) = tokio::fs::File::open(&full_path).await{
                            let metadata = file.metadata().await.unwrap();
                            let total_size = metadata.len();
                            
                            // ファイルサイズ
                            println!("[{}] ファイル情報: size={}, offset={}", now_str(), total_size, offset);

                            // キャッシュからハッシュを取得する
                            let server_hash = {
                                let lock = cache_inner.lock().unwrap();
                                lock.get(&full_path).map(|entry| entry.hash.clone())
                            };

                            // ファイルを受け取った位置にシーク
                            if let Err(_) = file.seek(std::io::SeekFrom::Start(offset)).await {
                                let header = DownloadStartResponse{ found: false, size: 0 , hash: None };
                                let header_json = serde_json::to_string(&header).unwrap() + "\n";
                                writer.write_all(header_json.as_bytes()).await.unwrap();
                                return;
                            };

                            // 最初にファイルサイズを送る
                            let header = DownloadStartResponse{
                                found: true,
                                size: total_size,
                                hash: server_hash,
                            };
                            let header_json = serde_json::to_string(&header).unwrap() + "\n";
                            writer.write_all(header_json.as_bytes()).await.unwrap();


                            // ファイル送信
                            match tokio::io::copy(&mut file, &mut writer).await {
                                Ok(bytes_sent) => {
                                    let expected_size = total_size - offset;
                                    if bytes_sent == expected_size {
                                        println!(
                                            "[{}] ダウンロード完了: {:?} (計 {} bytes 送信)", 
                                            now_str(), full_path, bytes_sent
                                        );
                                    } else {
                                        // 基本的に Ok であれば最後まで送っていますが、念のため
                                        println!(
                                            "[{}] ダウンロードが不完全です: {:?} ({} / {} bytes 送信)", 
                                            now_str(), full_path, bytes_sent, expected_size
                                        );
                                    }
                                }
                                Err(e) => {
                                    // クライアントが途中で切断した場合、BrokenPipe などのエラーがここに来ます
                                    println!(
                                        "[{}] ダウンロード中断: {:?} (理由: {}, 送信元: {:?})", 
                                        now_str(), full_path, e, sockaddr
                                    );
                                }
                            }                        
                        } else {
                            println!("[{}] ファイルが見つかりません: {:?}", now_str(), full_path);
                            // 指定のファイルが見つからなかった
                            let header = DownloadStartResponse { found: false, size: 0, hash: None };
                            let header_json = serde_json::to_string(&header).unwrap() + "\n";
                            writer.write_all(header_json.as_bytes()).await.unwrap();
                        }
                    }
                    Ok(TransferCommand::Upload { path, total_size, auth_key, hash }) =>{
                        // 安全なパスの生成
                        let final_path = match resolve_safe_path(&base_dir, &path) {
                            Ok(p) => p,
                            Err(e) => {
                                println!("[{}] Upload拒否 (無効なパス): {}", now_str(), e);
                                return;
                            }
                        };

                        // 権限チェック
                        // base_dir からの相対パスを取得
                        let relative_path = final_path.strip_prefix(&base_dir).unwrap_or(Path::new(""));
                        
                        if !AccessControl::check(relative_path, auth_key.as_ref()) {
                            println!("[{}] Upload拒否 (Auth Fail): {}", now_str(), relative_path.display());

                            // クライアントへ拒否を通知
                            let resp = UploadResponse {
                                status: "denied".to_string(),
                                start_offset: 0,
                                message: Some("権限がありません(パスワード不一致)".to_string()),
                            };
                            let json = serde_json::to_string(&resp).unwrap() + "\n";
                            let _ = writer.write_all(json.as_bytes()).await;
                            return; 
                        }


                        // 一時ファイルとハッシュ記録ファイルのパス
                        let mut part_path_name = final_path.file_name().unwrap().to_os_string();
                        part_path_name.push(".mysync_partial"); 
                        let part_path = final_path.with_file_name(&part_path_name);

                        let mut hash_path_name = part_path_name.clone();
                        hash_path_name.push(".hash");
                        let hash_path = final_path.with_file_name(hash_path_name);

                        println!("[{}] アップロード要求: {:?} (Size: {})", now_str(), final_path, total_size);


                        // ディレクトリ作成
                        if let Some(parent) = final_path.parent() {
                            if let Err(e) = tokio::fs::create_dir_all(parent).await{
                                return;
                            }
                        }

                        // ハッシュの不一致チェック
                        // 送られてきたハッシュがあり、かつサーバーに保存されたハッシュファイルが存在する場合
                        let mut reset_required = false;
                        if let Some(client_hash) = &hash {
                            if hash_path.exists() {
                                if let Ok(saved_hash) = tokio::fs::read_to_string(&hash_path).await {
                                    if saved_hash.trim() != client_hash.trim() {
                                        println!("[{}] ハッシュ不一致 (ファイル変更検知)。一時ファイルを破棄します。", now_str());
                                        reset_required = true;
                                    }
                                }
                            }
                        }

                        // ハッシュが違う、または強制リセットフラグが立っている場合は一時ファイルを消す
                        if reset_required {
                            let _ = tokio::fs::remove_file(&part_path).await;
                            let _ = tokio::fs::remove_file(&hash_path).await;
                        }

                        // 新しいハッシュが送られてきたらファイルに保存・更新しておく
                        if let Some(client_hash) = &hash {
                            let _ = tokio::fs::write(&hash_path, client_hash).await;
                        }


                        // ファイルオープン
                        let file = tokio::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open(&part_path).await;

                        let mut file = match file {
                            Ok(f) => f,
                            Err(e) => {
                                println!("ファイルオープン失敗: {}", e);
                                return;
                            }
                        };

                        // レジューム判定
                        let metadata = file.metadata().await.unwrap();
                        let mut current_size = metadata.len();

                        // もし .mysync_partial が想定サイズより大きければ破損とみなしてリセット
                        if current_size > total_size {
                            file.set_len(0).await.unwrap();
                            file.seek(std::io::SeekFrom::Start(0)).await.unwrap();
                            current_size = 0;
                        } else {
                            file.seek(std::io::SeekFrom::Start(current_size)).await.unwrap();
                        }

                        // クライアントに開始位置を通知
                        let resp = UploadResponse{
                            status: "ready".to_string(),
                            start_offset: current_size,
                            message: None,
                        };
                        let json_resp = serde_json::to_string(&resp).unwrap() + "\n";
                        if let Err(_) = writer.write_all(json_resp.as_bytes()).await { return; }
                        
                        // データ受信
                        if let Err(e) = tokio::io::copy(&mut buf_reader, &mut file).await {
                            println!("転送エラー: {}", e);
                            return;
                        }

                        // 完了確認
                        file.flush().await.unwrap();
                        let final_size = file.metadata().await.unwrap().len();

                        if final_size == total_size {
                            println!("[{}] アップロード完了。リネーム: {:?}", now_str(), final_path);

                            if let Err(e) = tokio::fs::rename(&part_path, &final_path).await {
                                println!("リネーム失敗 (使用中などの可能性): {}", e);
                            } else {
                                // リネーム成功時、用済みのハッシュファイルを消去する
                                let _ = tokio::fs::remove_file(&hash_path).await;
                            }
                        } else {
                            println!("中断されました。一時ファイルを保持します: {}/{}", final_size, total_size);
                        }
                    }
                    Ok(TransferCommand::Mkdir { path, auth_key }) => {
                        // 安全なパスの生成
                        let full_path = match resolve_safe_path(&base_dir, &path) {
                            Ok(p) => p,
                            Err(e) => {
                                println!("[{}] Mkdir拒否 (無効なパス): {}", now_str(), e);
                                return;
                            }
                        };

                        // 権限チェック
                        // base_dir からの相対パスを取得
                        let relative_path = full_path.strip_prefix(&base_dir).unwrap_or(Path::new(""));
                        
                        if !AccessControl::check(relative_path, auth_key.as_ref()) {
                            let msg = "権限エラー: パスワードが違います";
                            println!("[{}] Mkdir拒否: {}", now_str(), relative_path.display());
                            let _ = writer.write_all(msg.as_bytes()).await;
                            return;
                        }

                        println!("[{}] Mkdir要求: {:?}", now_str(), full_path);

                        match tokio::fs::create_dir_all(&full_path).await {
                            Ok(_) => {
                                let _ = writer.write_all("フォルダ作成完了".as_bytes()).await;
                            }
                            Err(e) => {
                                let msg = format!("作成エラー: {}", e);
                                let _ = writer.write_all(msg.as_bytes()).await;
                            }                            
                        }
                    }

                    Ok(TransferCommand::Remove { path, auth_key }) => {
                        // 安全なパスの生成
                        let full_path = match resolve_safe_path(&base_dir, &path) {
                            Ok(p) => p,
                            Err(e) => {
                                println!("[{}] Remove拒否 (無効なパス): {}", now_str(), e);
                                return;
                            }
                        };

                        // 権限チェック
                        // base_dir からの相対パスを取得
                        let relative_path = full_path.strip_prefix(&base_dir).unwrap_or(Path::new(""));
                        
                        if !AccessControl::check(relative_path, auth_key.as_ref()) {
                            let msg = "権限エラー: パスワードが違います";
                            println!("[{}] Remove拒否: {}", now_str(), relative_path.display());
                            let _ = writer.write_all(msg.as_bytes()).await;
                            return;
                        }

                        println!("[{}] Remove要求: {:?}", now_str(), full_path);

                        if !full_path.exists() {
                            let _ = writer.write_all("ファイルが見つかりません".as_bytes()).await;
                            return;
                        }

                        let res = if full_path.is_dir() {
                            tokio::fs::remove_dir_all(&full_path).await
                        } else {
                            tokio::fs::remove_file(&full_path).await
                        };

                        match res {
                            Ok(_) => {
                                let _ = writer.write_all("削除完了".as_bytes()).await;
                            }
                            Err(e) => {
                                let msg = format!("削除エラー: {}", e);
                                let _ = writer.write_all(msg.as_bytes()).await;
                            }
                        }
                    }
                    Err(e) => {
                        // 末尾の改行コードを消して見やすくする
                        let safe_str = line.trim_end(); 
                        
                        // {:?} を使えば "\x1b[31m" のように安全な文字としてエスケープ出力されるため、ログポイズニングを防げます。
                        println!(
                            "[{}] JSONパースエラー: {} | 送信元: {:?} | 受信データ: {:?}", 
                            now_str(), e, sockaddr, safe_str
                        );
                    }
                }
            });
        }
    });

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
