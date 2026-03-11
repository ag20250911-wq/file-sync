import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";
import { open } from '@tauri-apps/plugin-dialog'; // npm install @tauri-apps/plugin-dialog

// バイト数を適切な単位に変換する関数
const formatBytes = (bytes, decimals = 1) => {
  if (bytes === 0) return '0 B';

  const k = 1024;
  const dm = decimals < 0 ? 0 : decimals;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];

  const i = Math.floor(Math.log(bytes) / Math.log(k));

  return parseFloat((bytes / Math.pow(k, i)).toFixed(dm)) + ' ' + sizes[i];
};

// jsonの日時を表示用に変換
const formatDate = (isoString) => {
  if (!isoString) return "";
  const date = new Date(isoString);
  
  // 日本の環境で見やすい形式に変換 (YYYY/MM/DD HH:mm)
  return date.toLocaleString('ja-JP', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
};

// 指定したフォルダ配下の全ファイルを再帰的に取得する
const getAllFilesFromNode = (node) => {
  let files = [];
  if (!node.isDir) {
    files.push({ 
      name: node.name, 
      path: node.path, 
      isDir: false,
      size: node.size,
      modifiedAt: node.modifiedAt || "",
      isPartial: node.isPartial || false,
      hash: node.hash || null
    });
  } else if (node.children) {
    node.children.forEach(child => {
      files = [...files, ...getAllFilesFromNode(child)];
    });
  }
  return files;
};

// 平坦なパスのリストをツリー構造に変換するヘルパー関数
const buildFileTree = (items) => {
  const root = [];
  const map = {};

  // 1. まず全てのノードをマップに登録し、children配列を準備する
  items.forEach(item => {
    map[item.path] = { ...item, children: [] };
  });

  // 2. パスを解析して親子関係を構築する
  items.forEach(item => {
    const parts = item.path.split('/');
    if (parts.length === 1) {
      // トップレベル（親がいない）
      root.push(map[item.path]);
    } else {
      // 親のパスを探す
      const parentPath = parts.slice(0, -1).join('/');
      if (map[parentPath]) {
        map[parentPath].children.push(map[item.path]);
      } else {
        // 親が見つからない場合はトップレベルとして扱う
        root.push(map[item.path]);
      }
    }
  });
  return root;
};

// 受信したJsonをTreeに表示
const TreeNode = ({ node, level = 0, selectedPath, onSelect, onContextMenu, fileStatuses, dragTargetDir }) => {
  const [isOpen, setIsOpen] = useState(false);
  const isDirectory = node.isDir;
  const isSelected = selectedPath === node.path;

  // 自分がドロップ対象の「フォルダ」かどうか判定
  const isDragTarget = isDirectory && dragTargetDir === node.path;

  const isPartial = node.isPartial;

  // ローカルに存在するかどうか
  // ステータスを取得 (undefined ならローカルに存在しない)
  const statusDetail = fileStatuses[node.path]; 
  const exists = statusDetail !== undefined;// synced, outdated, partial のいずれか

  // ステータス判定 (オブジェクト内のプロパティを見る)
  const isOutdated = exists && statusDetail.status === "outdated";
  // 途中判定
  const isPartialDownload = exists && statusDetail.status === "partial";

  // ハッシュ表示ロジック
  const serverHash = node.hash || "なし";
  
  // ローカルハッシュ: 計算されていれば表示、なければ「サイズ不一致(未計算)」
  let localHashDisplay = "---";
  if (exists) {
      if (statusDetail.local_hash) {
          localHashDisplay = statusDetail.local_hash;
      } else {
          localHashDisplay = "未計算 (サイズ不一致)";
      }
  }

  // ツールチップ作成
  let tooltipText = node.name;
  if (!node.isDir) {
      const svrHashDisplay = serverHash;
      const locHashDisplay = localHashDisplay;

      if (exists) {
        if (isOutdated) {
            tooltipText += `\n⚠️ 内容不一致`;
            // サイズ情報
            const locSize = statusDetail.local_size ? formatBytes(statusDetail.local_size) : "? B";
            tooltipText += `\n[Size] Svr:${formatBytes(node.size)} / Loc:${locSize}`;
        } else if (isPartialDownload) {
            // 途中経過の表示
            tooltipText += `\n⏸ ダウンロード一時停止中`;
            const pct = Math.floor((statusDetail.local_size / node.size) * 100);
            tooltipText += `\n[Progress] ${pct}% (${formatBytes(statusDetail.local_size)} / ${formatBytes(node.size)})`;
        } else {
            tooltipText += `\n✔ 同期済み`;
        }
        // ハッシュ情報
        tooltipText += `\n[Svr Hash] ${svrHashDisplay}`;
        tooltipText += `\n[Loc Hash] ${locHashDisplay}`;
      } else {
        // ハッシュ情報
        tooltipText += `\n[Svr Hash] ${svrHashDisplay}`;
      }
  }


  return (
    <div>
      <div
        // クラス名に isDragTarget ? "drag-over-target" : "" を追加
        className={`tree-node ${isSelected ? "selected" : ""} ${isDirectory ? "is-dir" : ""} ${isPartial ? "partial-file" : ""} ${isDragTarget ? "drag-over-target" : ""}`}
        style={{ paddingLeft: `${level * 12 + 12}px` }}
        data-path={node.path}
        data-isdir={node.isDir}
        onClick={() => {
          if (isDirectory) setIsOpen(!isOpen);
          onSelect(node.path);
        }}
        onContextMenu={(e) => {
          e.stopPropagation(); // 親(背景)へのイベント伝播を止める
          onContextMenu(e, node);
        }}
        title={tooltipText} // ツールチップ設定
      >
        {/* 矢印アイコンの表示ロジック */}
        <span style={{ fontSize: '10px', width: '12px', color: '#666', marginRight: '4px' }}>
          {isDirectory ? (isOpen ? "▼" : "▶") : ""}
        </span>
        
        {/* アイコンの出し分け */}
        <span className={`icon ${isDirectory ? "icon-folder" : "icon-file"}`}>
          {isDirectory ? (isOpen ? "📂" : "📁") : "📄"}
        </span>

        {/* クラス出し分け: outdated, synced, partial */}
        <span className={`name 
            ${isOutdated ? "status-outdated" : ""} 
            ${!isOutdated && exists && !isPartialDownload ? "status-synced" : ""}
            ${isPartialDownload ? "status-partial" : ""} 
        `}>
          {node.name}
          
          {/* サーバー側で転送中の表示(既存) */}
          {node.isPartial && <span style={{fontSize: "10px", color: "#888", marginLeft: "4px"}}>(転送中...)</span>}
          
          {/* ローカルでダウンロード途中の場合の % 表示 */}
          {isPartialDownload && (
             <span style={{fontSize: "10px", color: "#569cd6", marginLeft: "6px"}}>
                {Math.floor((statusDetail.local_size / node.size) * 100)}%
             </span>
          )}
        </span>
        
        {/* ステータスアイコン */}
        {isOutdated && <span title="サーバー側が更新されています" style={{ fontSize: "10px", marginLeft: "4px" }}>⚠️</span>}
        {!isOutdated && exists && !isPartialDownload && <span style={{ fontSize: "10px", color: "#4ec9b0", marginLeft: "4px" }}>✔</span>}
        
        {/* 一時停止アイコン */}
        {isPartialDownload && <span title="ダウンロード途中" style={{ fontSize: "10px", color: "#569cd6", marginLeft: "4px" }}>⏸</span>}
        {!isDirectory && (
          <div className="file-info">
            <span className="date">{formatDate(node.modifiedAt)}</span>
            <span className="size">{formatBytes(node.size)}</span>
          </div>
        )}
      </div>

      {isDirectory && isOpen && node.children && (
        <div className="children-container"> {/* ここに階層線用のクラスを追加 */}
          {node.children.map((child, i) => (
            <TreeNode
              key={i}
              node={child}
              level={level + 1}
              selectedPath={selectedPath}
              onSelect={onSelect}
              onContextMenu={onContextMenu}
              fileStatuses={fileStatuses} // 再帰的に渡す
            />
          ))}
        </div>
      )}
    </div>
  );
};

// 最後に再生した時刻を記録する変数（モジュールスコープ）
let lastPlayTime = 0; 

// 効果音を鳴らすヘルパー関数
const playUpdateSound = () => {
  const now = Date.now();
  
  // 前回の再生から 5000ms (5秒) 以内なら鳴らさずに終了
  if (now - lastPlayTime < 5000) {
    return;
  }

  // 再生時刻を更新
  lastPlayTime = now; 

  const audio = new Audio("/list_get.mp3"); // publicフォルダに置いたファイルパス
  audio.volume = 0.2; // 音量調整（0.0 〜 1.0）
  audio.play().catch(err => console.error("SE再生失敗:", err));
};

const playDoneSound = () => {
  const audio = new Audio("/done.mp3");
  audio.volume = 0.5;
  audio.play().catch(e => console.error("SE再生エラー:", e));
};

function App() {
  const [ip, setIP] = useState("");
  const [url, setUrl] = useState("");
  const [treeData, setTreeData] = useState([]);
  const [selectedPath, setSelectedPath] = useState(null);
  // 初期値をオブジェクトで統一
  const [connStatus, setConnStatus] = useState({ connected: false, message: "initializing", retryIn: null });
  const [menu, setMenu] = useState(null);
  const [progress, setProgress] = useState(null);
  // パスワード管理
  const [adminPassword, setAdminPassword] = useState("");

  // ドラッグ中フラグ
  const [isDragging, setIsDragging] = useState(false);
  const [dragTargetDir, setDragTargetDir] = useState(null); // 現在ホバー中のアップロード先フォルダ

  // アップロード履歴のステート (例: { "folder/video.mp4": "C:\\Users\\...\\video.mp4" })
  const [uploadHistory, setUploadHistory] = useState({});

  // Setではなくオブジェクト(Map)で管理 { "path/to/file": "synced" | "outdated" }
  const [fileStatuses, setFileStatuses] = useState({});

  // 保存先ディレクトリのStateを追加
  const [saveDir, setSaveDir] = useState(".\\Download");

  const urlRef = useRef(url);

  // 現在の転送モードを記録する (初期値は download としておく)
  const transferMode = useRef("download"); 

  useEffect(() => { urlRef.current = url; }, [url]);

  // ドラッグ＆ドロップ関数から常に最新のStateを参照するためのRef
  const stateRef = useRef({ ip, selectedPath, treeData, menu, adminPassword });
  useEffect(() => {
    stateRef.current = { ip, selectedPath, treeData, menu, adminPassword };
  }, [ip, selectedPath, treeData, menu, adminPassword]);

  // 背景（ルート）での右クリックハンドラ
  const handleBackgroundContextMenu = (e) => {
    e.preventDefault(); // ブラウザのメニューを抑制
    
    // 選択状態を解除（ルート操作であることを明確にするため）
    setSelectedPath(null);

    let x = e.clientX;
    let y = e.clientY;
    
    // 画面端の調整ロジック（既存と同じ）
    const menuWidth = 200;
    const menuHeight = 250;
    if (x + menuWidth > window.innerWidth) x = x - menuWidth;
    if (y + menuHeight > window.innerHeight) {
      y = y - menuHeight;
      if (y < 0) y = 10;
    }

    // node: null としてメニューを表示
    setMenu({ x, y, node: null });
  };




  // リスト取得関数（引数があればそれ優先、なければ現在のurl stateを使用）
  const fetchTree = async (targetUrl) => {
    const activeUrl = targetUrl || urlRef.current;
    if (!activeUrl) return;
    try {
      const data = await invoke("get_file_list", { url: activeUrl });
      setTreeData(buildFileTree(data));

      // --- ここでSEを鳴らす ---
      playUpdateSound();
      console.log("リスト更新成功");
    } catch (err) {
      console.error("Fetch error:", err);
    }
  };

  // 関数: ローカルファイルのステータスチェックを行う
  const checkLocalFiles = async (items) => {
    if (!items || items.length === 0) return;
    
    // Rustに渡すために FileItem 形式の配列を作る
    const flattenItems = (nodes) => {
      let result = [];
      nodes.forEach(node => {
        // RustのFileItem構造体に合うオブジェクトを作成
        result.push({ 
          name: node.name,
          path: node.path, 
          isDir: node.isDir,
          isPartial: node.isPartial,
          size: node.size,
          modifiedAt: node.modifiedAt,
          hash: node.hash
        });
        
        if (node.isDir && node.children) {
          result.push(...flattenItems(node.children));
        }
      });
      return result;
    };

    const flatList = flattenItems(items);
    
    try {
      const statusMap = await invoke("check_file_status", { 
        files: flatList, 
        saveDir: saveDir 
      });
      setFileStatuses(statusMap);
    } catch (e) {
      console.error("ステータスチェック失敗:", e);
    }
  };

  // ハッシュ強制再計算
  const handleRecalculateHash = async () => {
    if (!menu || menu.node.isDir) return;
    const node = menu.node;

    try {
        // Rustの再計算コマンドを呼び出し (戻り値はハッシュ文字列)
        const newHash = await invoke("recalc_file_hash", {
            saveDir: saveDir,
            filePath: node.path
        });

        // 成功したらステートを更新
        setFileStatuses(prev => {
            const next = { ...prev };
            
            // 既存のステータス情報があればサイズなどを引き継ぐ
            const currentDetail = next[node.path] || { local_size: 0 }; // default

            // 新しいハッシュとサーバーハッシュを比較
            let newStatus = "synced";
            if (node.hash && newHash !== node.hash) {
                newStatus = "outdated";
            }

            // 更新 (local_hash に値が入る！)
            next[node.path] = {
                ...currentDetail,
                status: newStatus,
                local_hash: newHash // ★ここでNoneではなく実値が入る
            };
            return next;
        });

        alert(`再計算完了\nLocal: ${newHash}`);

    } catch (e) {
        console.error(e);
        alert("再計算エラー: " + e);
    }
    setMenu(null);
  };

 // useEffectでの呼び出しはそのまま (treeDataやsaveDir変更時に呼ぶ)
  useEffect(() => {
    if (treeData.length > 0) {
      checkLocalFiles(treeData);
    }
  }, [treeData, saveDir]);

  // useEffectの最初（マウント時）に追加
  useEffect(() => {
    const preventDefault = (e) => e.preventDefault();
    document.addEventListener("dragover", preventDefault);
    document.addEventListener("drop", preventDefault);
    return () => {
      document.removeEventListener("dragover", preventDefault);
      document.removeEventListener("drop", preventDefault);
    };
  }, []);


useEffect(() => {
    // クリーンアップ用変数を定義
    let unlistenTcpStatus = null;
    let unlistenRefresh = null;
    let unlistenProgress = null;
    
    // マウント状態管理フラグ
    let isMounted = true;

    const init = async () => {
      try {
        // --- 設定読み込み ---
        try {
            const config = await invoke("load_config");
            // アンマウント済みなら処理中断
            if (!isMounted) return; 
            if (config && config.save_dir) {
                setSaveDir(config.save_dir);
            }

            // アップロード履歴の読み込み
            const history = await invoke("get_upload_history");
            setUploadHistory(history);
        } catch (e) {
            console.error("設定読み込み失敗:", e);
        }

        // --- IP取得 ---
        const fetchedIp = await invoke("fetch_ip");
        if (!isMounted) return; // アンマウント済みなら中断

        setIP(fetchedIp);
        const initialUrl = `http://${fetchedIp}:44448/files`;
        setUrl(initialUrl);

        // --- リスナー登録 ---

        // TCP Status
        const unlistenTcp = await listen("tcp-status", (event) => {
          if (!isMounted) return; // コールバック内でも念のためガード
          const { connected, message, retry_in } = event.payload;
          setConnStatus({ connected, message, retryIn: retry_in });
          if (connected) {
            fetchTree(initialUrl);
          } else {
            setTreeData([]);
          }
        });
        // 登録完了時に既にアンマウントされていたら即解除
        if (!isMounted) {
            unlistenTcp();
            return;
        }
        // 変数に格納 (クリーンアップ用)
        unlistenTcpStatus = unlistenTcp;


        // --- ドラッグ＆ドロップイベントの監視 ---
        // ドラッグ中にマウスが動いている時のイベント
        const unlistenDragOver = await listen("tauri://drag-over", (event) => {
          const position = event.payload?.position;
          if (position) {
            const element = document.elementFromPoint(position.x, position.y);
            let targetPath = ""; // デフォルトはルート(空)
            
            if (element) {
              const treeNodeElement = element.closest('.tree-node');
              if (treeNodeElement) {
                const nodePath = treeNodeElement.getAttribute('data-path');
                const isDir = treeNodeElement.getAttribute('data-isdir') === "true";
                
                // フォルダならそのまま、ファイルなら親フォルダにする
                if (isDir) {
                  targetPath = nodePath;
                } else {
                  const lastSlashIdx = nodePath.lastIndexOf('/');
                  targetPath = lastSlashIdx >= 0 ? nodePath.substring(0, lastSlashIdx) : "";
                }
              }
            }
            
            // Reactの無駄な再描画を防ぐため、値が変わった時だけ更新する
            setDragTargetDir(prev => prev !== targetPath ? targetPath : prev);
          }
        });
        
        const unlistenDragLeave = await listen("tauri://drag-leave", () => {
          setIsDragging(false);
          setDragTargetDir(null); // リセット
        });
        
        const unlistenDragDrop = await listen("tauri://drag-drop", async (event) => {
          setIsDragging(false);
          
          // ドラッグ中に計算していた dragTargetDir を取り出して使う
          // stateRef を使わなくても、Reactの setState は非同期なので
          // このイベントリスナーが呼ばれる直前の state は capture されていますが、
          // 念のため Ref から最新を取るか、ローカル変数で計算し直すのが安全です。
          // （ドロップ座標の再計算は先ほど追加したものをそのまま残します）
          const paths = event.payload?.paths;
          const position = event.payload?.position;

          let dropTargetDir = "";

          if (position) {
            const element = document.elementFromPoint(position.x, position.y);
            if (element) {
              const treeNodeElement = element.closest('.tree-node');
              if (treeNodeElement) {
                const nodePath = treeNodeElement.getAttribute('data-path');
                const isDir = treeNodeElement.getAttribute('data-isdir') === "true";
                if (isDir) {
                  dropTargetDir = nodePath;
                } else {
                  const lastSlashIdx = nodePath.lastIndexOf('/');
                  dropTargetDir = lastSlashIdx >= 0 ? nodePath.substring(0, lastSlashIdx) : "";
                }
              }
            }
          }

          setDragTargetDir(null); // リセット

          if (paths && paths.length > 0) {
            handleDropUpload(paths, dropTargetDir);
          }
        });


        // Refresh
        const unlistenRef = await listen("refresh-file-list", () => {
          if (!isMounted) return;
          console.log("サーバーからの要求によりリストを更新します");
          fetchTree(urlRef.current);
        });
        if (!isMounted) {
            unlistenRef();
            return;
        }
        unlistenRefresh = unlistenRef;


        // Progress
        const unlistenProg = await listen("transfer-progress", (event) => {
          if (!isMounted) return;
          setProgress(event.payload);
          if (event.payload.percent === 100) {
            // 完了時は「synced」としてステータスを更新
            setFileStatuses(prev => {
              const next = { ...prev };
              const filePath = event.payload.path;
              
              // 現在のハッシュ値を維持しつつ status だけ synced にする
              const current = prev[filePath] || {};
              next[filePath] = { 
                  ...current,
                  status: "synced", 
                  local_size: event.payload.total 
              };

              // 親ディレクトリも "synced" にする
              const parts = filePath.split('/');
              let currentPath = "";
              for (let i = 0; i < parts.length - 1; i++) {
                currentPath += (i === 0 ? "" : "/") + parts[i];

                // 現在のハッシュ値を維持しつつ status だけ synced にする
                const current = prev[currentPath] || {};
                next[currentPath]  = { 
                  ...current,
                  status: "synced", 
                  local_size: event.payload.total 
              };
              }
              return next;
            });
            
            // プログレスバー消去タイマー (既存コード)
            setTimeout(() => {
              // 現在のprogressがまだ100%なら消す（次のDLが始まっていないか確認）
              setProgress(prev => (prev?.percent === 100 ? null : prev));
            }, 10000); 
          }
        });
        if (!isMounted) {
            unlistenProg();
            return;
        }
        unlistenProgress = unlistenProg;


        // --- TCPクライアント開始 ---
        await invoke("start_tcp_client", { ip: fetchedIp });

      } catch (err) {
        if (!isMounted) return;
        console.error("Init error:", err);
        setConnStatus({ connected: false, message: "error", retryIn: null });
      }
    };

    init();

    const closeMenu = () => setMenu(null);
    window.addEventListener("click", closeMenu);

    // クリーンアップ関数 (同期的に実行される)
    return () => {
      isMounted = false; // フラグを折る
      window.removeEventListener("click", closeMenu);
      
      // 変数に入っている関数があれば実行
      if (unlistenTcpStatus) unlistenTcpStatus();
      if (unlistenRefresh) unlistenRefresh();
      if (unlistenProgress) unlistenProgress();
    };
  }, []);

  // フォルダ選択と設定保存の関数を追加
  const handleChangeSaveDir = async () => {
    try {
        const selected = await open({
            directory: true,
            multiple: false,
            defaultPath: saveDir, // 現在の設定を開く初期位置にする
        });

        if (selected) {
            setSaveDir(selected);
            // Rust側に保存を依頼
            await invoke("save_config", { saveDir: selected });
        }
    } catch (e) {
        console.error("フォルダ選択エラー:", e);
        alert("フォルダ選択に失敗しました");
    }
  };

  // 保存先を開く
  const handleOpenDownloadFolder = async () => {
    try {
      await invoke("open_local_item", { path: saveDir, mode: "open" });
    } catch (e) {
      console.error(e);
      alert("フォルダを開けませんでした: " + e);
    }
  };


  const handleContextMenu = async (e, node) => {
    e.preventDefault();
    
    let x = e.clientX;
    let y = e.clientY;
    const menuWidth = 200;
    const menuHeight = 250;

    // 右端で切れる場合の調整
    if (x + menuWidth > window.innerWidth) x = x - menuWidth;

    // 下端で切れる場合の調整
    if (y + menuHeight > window.innerHeight) {
      y = y - menuHeight;
      // 画面の一番上よりはみ出さないようにガード
      if (y < 0) y = 10;
    }

    // 選択状態の更新とメニュー表示
    setSelectedPath(node.path);
    setMenu({ x, y, node });

    // 非同期でチェックを走らせる
    checkSingleFileStatus(node); 
  };

  // 関数として切り出し
  const checkSingleFileStatus = async (node) => {

    // 右クリックした瞬間に、そのファイルの存在を再チェックする
    try {
      // RustのFileItem構造体に合うオブジェクトを作成
      const fileItem = { 
          name: node.name,
          path: node.path, 
          isDir: node.isDir,
          isPartial: node.isPartial,
          size: node.size,
          modifiedAt: node.modifiedAt,
          hash: node.hash
      };

      // ハッシュ計算が起きる場合数秒かかる
      const result = await invoke("check_file_status", { 
        files: [fileItem], 
        saveDir: saveDir 
      });

      // 結果に応じてステートを即時更新
      setFileStatuses(prev => {
        const next = { ...prev };
        const status = result[node.path];
        if (status) {
            next[node.path] = status;
        } else {
            // 存在しない場合(戻り値のマップに含まれない場合)はキーを削除
            delete next[node.path];
        }
        return next;
      });
    } catch (err) {
      console.error("存在チェックエラー:", err);
    }
  };


  const handleDownloadFolder = async () => {
    if (!menu || !menu.node.isDir || !ip) return;
    
    const folderNode = menu.node;
    const files = getAllFilesFromNode(folderNode);
    
    if (files.length === 0) {
      alert("フォルダは空です");
      return;
    }

    const confirmDown = await window.confirm(`${folderNode.name} 内の ${files.length} 個のファイルをダウンロードしますか？`);
    if (!confirmDown) return;

    // モードを記録
    transferMode.current = "download";

    try {
      // Rust側のコマンドを呼び出す
      // files: [{path: "...", size: ...}, ...] の配列を渡す
      const res = await invoke("tcp_download_folder", {
        ip: ip,
        files: files,
        saveDir: saveDir
      });
      
      playDoneSound();
      checkLocalFiles(treeData);
    } catch (e) {
      console.error(e);
      alert("フォルダDLエラー: " + e);
    }
    setMenu(null);
  };

    // ローカル保存先を開く
  const handleOpenLocalFile = async () => {
      if (!menu || !menu.node) return;
      try {
          // サーバーからのパス (例: "folder/file.txt") の "/" を "\" に置換 (Windows対応)
          //    ※ Mac/Linux対応も考慮するなら "/" のままでも良い場合が多いですが、
          //      Rust側の open_local_item が Windowsの explorer コマンドを使っているため \ 推奨
          const relativePath = menu.node.path.replace(/\//g, '\\');
          
          // 保存先ディレクトリ (saveDir) の末尾に "\" があれば削除して整形
          const cleanSaveDir = saveDir.endsWith('\\') ? saveDir.slice(0, -1) : saveDir;
          
          // パスを結合してフルパスを作成 (例: "C:\Download\folder\file.txt")
          const fullPath = `${cleanSaveDir}\\${relativePath}`;
          
          // すでにRustに存在する "open_local_item" コマンドを呼び出す
          //    引数は path と mode
          await invoke("open_local_item", { path: fullPath, mode: "reveal" });
          
      } catch (e) {
          console.error(e);
          alert("ファイルを開けません: " + e);
      }
      setMenu(null);
  };

  const handleDownload = async () => {
      if (!selectedPath || !ip) return;

      // モードを記録
      transferMode.current = "download";

      try {
        console.log(`ダウンロード開始: ${selectedPath}`);
        
        const res = await invoke("tcp_download_file", {
          ip: ip,
          filePath: selectedPath, // Rust側の引数名はスネークケース file_path に変換される
          saveDir: saveDir,
          serverHash: menu.node.hash || null, 
          serverSize: menu.node.size,
          force: true // メニューからの実行は常に強制上書き
        });

        if (res === "ダウンロード完了") {
          playDoneSound();
          checkSingleFileStatus(menu.node);
        } else {
          // スキップされた場合などのログ（force:trueなら基本出ないはず）
          console.log(res);
        }

      } catch (e) {
        console.error(e);
        alert("DLエラー: " + e);
      }
      setMenu(null);
    };

  const handleUpload = async () => {
    if (!ip) {
      alert("サーバーに接続されていません");
      return;
    }

    try {
      // 1. アップロードするファイルを選択
      const selectedLocalPath = await open({
        multiple: false,
        directory: false,
      });

      if (!selectedLocalPath) return; // キャンセル時

      // 2. サーバー側の保存先ディレクトリを決定
      let targetDir = "";
      
      if (selectedPath) {
        // 何かを選択している場合
        // menu.node があればそれを使う（右クリックメニューから呼ばれた場合）
        const node = menu ? menu.node : treeData.find(n => n.path === selectedPath); // 簡易検索
        
        if (node && node.isDir) {
          // フォルダを選択中ならその中へ
          targetDir = node.path;
        } else if (node) {
          // ファイルを選択中ならその親フォルダへ
          targetDir = node.path.substring(0, node.path.lastIndexOf('/'));
        }
      } else {
         // 何も選択していなければルートへ
         targetDir = ""; 
      }

      // 3. ローカルのファイル名を取得 (Windows/Mac対応)
      // selectedLocalPath は "C:\Users\foo\bar.txt" のようなフルパス
      const fileName = selectedLocalPath.split(/[\\/]/).pop();

      // 4. リモートパスを結合 (Windowsのパス区切りを / に置換して結合)
      // 例: "folder/sub" + "/" + "bar.txt"
      const remotePath = `${targetDir}/${fileName}`
        .replace(/^\//, '') // 先頭の / を削除
        .replace(/\/+/g, '/'); // 重複する / を削除

      console.log(`UL開始: Local[${selectedLocalPath}] -> Remote[${remotePath}]`);

      // モードを記録
      transferMode.current = "upload";

      // アップロード履歴に保存
      await invoke("save_upload_history", { remotePath, localPath: selectedLocalPath });
      setUploadHistory(prev => ({ ...prev, [remotePath]: selectedLocalPath }));

      // Rustを呼び出す前に「計算中」UIを出す
      setProgress({
        path: fileName,
        percent: 0,
        isCalculating: true
      });

      // Rustコマンド呼び出し
      const res = await invoke("tcp_upload_file", {
        ip: ip,
        localPath: selectedLocalPath,
        remotePath: remotePath,
        authKey: adminPassword || null
      });

      // 完了したら履歴から削除
      await invoke("remove_upload_history", { remotePath });
      setUploadHistory(prev => {
        const next = { ...prev };
        delete next[remotePath];
        return next;
      });


      playDoneSound();
      console.log(res);

    } catch (e) {
      console.error(e);
      alert("アップロードエラー: " + e);
    }
    setMenu(null);
  };

  // --- アップロード再開 (履歴から自動選択 or ダイアログ) ---
  const handleResumeUpload = async () => {
    if (!ip || !menu || !menu.node) return;
    
    // UI上のパスには末尾に ".mysync_partial" または ".mysync_partial.hash" が付いているので除去する
    const actualRemotePath = menu.node.path.replace(/\.mysync_partial(\.hash)?$/, "");
    
    // 履歴からローカルのフルパスを取得
    let localPath = uploadHistory[actualRemotePath];

    // ローカルにファイルが実在するかチェック (Rustのコマンドを利用)
    let fileExists = false;
    if (localPath) {
      fileExists = await invoke("check_local_file_exists", { path: localPath });
    }

    // 履歴がない、またはファイルが移動・削除されている場合はダイアログを出す
    if (!localPath || !fileExists) {
      const msg = localPath 
        ? "記録されていた元のファイルが見つかりません。\n続きをアップロードするファイルを選択してください。"
        : "元のローカルパスが記録されていません。\n続きをアップロードするファイルを選択してください。";
        
      alert(msg); // ユーザーに状況を伝える

      // ファイル選択ダイアログを開く
      const selectedLocalPath = await open({
        multiple: false,
        directory: false,
      });

      if (!selectedLocalPath) {
        setMenu(null);
        return; // キャンセルされたら終了
      }
      
      localPath = selectedLocalPath; // 選択されたパスで上書き
      
      // 新しいパスを履歴に保存し直す
      await invoke("save_upload_history", { remotePath: actualRemotePath, localPath });
      setUploadHistory(prev => ({ ...prev, [actualRemotePath]: localPath }));
    }

    transferMode.current = "upload";
    setMenu(null);

    try {
      console.log(`レジュームUL開始: Local[${localPath}] -> Remote[${actualRemotePath}]`);
      
      // Rustを呼び出す前に「計算中」UIを出す
      setProgress({
        path: localPath,
        percent: 0,
        isCalculating: true
      });      
      
      const res = await invoke("tcp_upload_file", {
        ip: ip,
        localPath: localPath, // 決定したパスを使う
        remotePath: actualRemotePath,
        authKey: adminPassword || null
      });

      playDoneSound();
      console.log(res);

      // 完了したら履歴から削除
      await invoke("remove_upload_history", { remotePath: actualRemotePath });
      setUploadHistory(prev => {
        const next = { ...prev };
        delete next[actualRemotePath];
        return next;
      });

    } catch (e) {
      console.error(e);
      // キャンセル操作でない場合はエラー表示
      if (!(typeof e === "string" && e.includes("キャンセル"))) {
        alert("レジュームエラー: " + e);
      }
    }
  };

  // --- ドラッグ＆ドロップによるアップロード処理 ---
  const handleDropUpload = async (localPaths, targetDir) => {
    // 最新のStateをRefから取り出す 
    const current = stateRef.current;

    if (!current.ip) {
      alert("サーバーに接続されていません");
      return;
    }


    try {
      // Rust側でドロップされたパスを解析し、アップロードリストを作成
      const uploadItems = await invoke("prepare_upload_items", {
        paths: localPaths,
        targetDir: targetDir
      });

      if (uploadItems.length === 0) return;

      // ユーザーに確認
      // const confirmMsg = `${uploadItems.length} 個のファイルをアップロードしますか？\n(保存先: /${targetDir || "ルート"})`;
      // if (!await window.confirm(confirmMsg)) return;

      transferMode.current = "upload";

      // ループして順番にアップロード実行
      for (let i = 0; i < uploadItems.length; i++) {
        const item = uploadItems[i];
        console.log(`UL開始 (${i+1}/${uploadItems.length}): ${item.local_path} -> ${item.remote_path}`);

        try {
          // アップロード履歴に保存
          await invoke("save_upload_history", { remotePath: item.remote_path, localPath: item.local_path });
          setUploadHistory(prev => ({ ...prev, [item.remote_path]: item.local_path }));

          // Rustを呼び出す前に「計算中」UIを出す
          setProgress({
            path: item.local_path,
            percent: 0,
            isCalculating: true
          });

          await invoke("tcp_upload_file", {
            ip: current.ip,
            localPath: item.local_path,
            remotePath: item.remote_path,
            authKey: current.adminPassword || null
          });

          // 完了したら履歴から削除
          await invoke("remove_upload_history", { remotePath: item.remote_path });
          setUploadHistory(prev => {
            const next = { ...prev };
            delete next[item.remote_path];
            return next;
          });
        } catch (e) {
          // キャンセルされた場合はループを抜けて後続のファイルを止める
          if (typeof e === "string" && e.includes("キャンセル")) {
            console.log("転送がキャンセルされました。キューをクリアします。");
            break; 
          } else {
            console.error("アップロードエラー:", e);
            alert(`アップロードエラー (${item.local_path}): ${e}`);
          }
        }
        
        // 連続でポートを使い潰さないためのごく短いウェイト
        await new Promise(resolve => setTimeout(resolve, 50));
      }

      playDoneSound();
      console.log("ドロップアップロード完了");

    } catch (e) {
      console.error(e);
      alert("アップロードエラー: " + e);
    }
  };

  // キャンセルハンドラ
  const handleCancelTransfer = async () => {
    if (progress && progress.path) {
      try {
        await invoke("cancel_transfer", { path: progress.path });
        setProgress(null); // 表示を消す
      } catch (e) {
        console.error("キャンセル失敗:", e);
      }
    }
  };

// --- クリップボードにコピーする関数 ---
  const handleCopyName = async () => {
    if (!menu || !menu.node) return;
    try {
      await navigator.clipboard.writeText(menu.node.name);
      console.log("ファイル名をコピーしました:", menu.node.name);
    } catch (e) {
      console.error("コピー失敗:", e);
      alert("クリップボードへのコピーに失敗しました");
    }
    setMenu(null);
  };

  const handleCopyPath = async () => {
    if (!menu || !menu.node) return;
    try {
      await navigator.clipboard.writeText(menu.node.path);
      console.log("フルパスをコピーしました:", menu.node.path);
    } catch (e) {
      console.error("コピー失敗:", e);
    }
    setMenu(null);
  };

  // パスワード設定ボタンのハンドラ (ツールバーあたりに追加)
  const handleSetPassword = () => {
      const input = window.prompt("管理者パスワードを入力してください (空欄でクリア)", adminPassword);
      if (input !== null) {
          setAdminPassword(input);
      }
  };

  // フォルダ作成ハンドラ
  const handleCreateFolder = async () => {
    if (!ip) return;
    
    // 親パスの決定ロジック
    let parentPath = "";
    if (menu.node) {
        if (menu.node.isDir) {
            parentPath = menu.node.path;
        } else {
            const idx = menu.node.path.lastIndexOf('/');
            parentPath = idx >= 0 ? menu.node.path.substring(0, idx) : "";
        }
    }

    const folderName = window.prompt("新規フォルダ名:");
    if (!folderName) return;

    // パス結合（先頭のスラッシュ対策など）
    const cleanParent = parentPath.replace(/^\/|\/$/g, '');
    const newPath = cleanParent ? `${cleanParent}/${folderName}` : folderName;

    try {
        const res = await invoke("tcp_create_dir", {
            ip: ip,
            path: newPath,
            authKey: adminPassword || null // Rust側は Option<String> なので空文字ならnullを送る
        });
        playDoneSound();
    } catch (e) {
        console.error(e);
        alert("作成失敗: " + e);
    }
    setMenu(null);
  };

  // 削除ハンドラ
  const handleRemove = async () => {
      if (!ip || !menu.node) return;
      const confirmMsg = `「${menu.node.name}」をサーバーから削除しますか？\n(取り消せません)`;
      if (!await window.confirm(confirmMsg)) return;

      try {
          const res = await invoke("tcp_remove_item", {
              ip: ip,
              path: menu.node.path,
              authKey: adminPassword || null
          });
          playDoneSound();
      } catch (e) {
          console.error(e);
          alert("削除失敗: " + e);
      }
      setMenu(null);
  };

  // ローカルファイルの削除ハンドラ
  const handleLocalRemove = async () => {
    if (!menu.node) return;
    
    const confirmMsg = `ローカルディスク上の「${menu.node.name}」を削除しますか？\n(サーバー上のファイルは残ります)`;
    if (!await window.confirm(confirmMsg)) return;

    try {
      await invoke("delete_local_item", {
        saveDir: saveDir,
        path: menu.node.path
      });
      
      playDoneSound();
      
      // 成功したらローカルのステータス情報を更新（削除）する
      setFileStatuses(prev => {
        const next = { ...prev };
        delete next[menu.node.path];
        return next;
      });

    } catch (e) {
      console.error(e);
      alert("ローカル削除失敗: " + e);
    }
    setMenu(null);
  };




  return (
    <div className="app-container">

      {/* --- ドラッグ中のオーバーレイ --- */}
      {isDragging && (
        <div className="drag-overlay">
          <div className="drag-message">
            <div>📂 ここにファイルをドロップしてアップロード</div>
            {/* ドロップ先の表示 */}
            <div style={{ fontSize: '14px', marginTop: '12px', color: '#ffd700', fontWeight: 'normal' }}>
              保存先: / {dragTargetDir ? dragTargetDir : "ルート"}
            </div>
          </div>
        </div>
      )}    

      <div className="toolbar">
        {/* 左側：接続ステータスとURL */}
        <div className="toolbar-group main-group">
          <div className={`status-badge ${connStatus.connected ? "connected" : "retry"}`}>
            <span className="status-dot">●</span>
            {connStatus.connected ? "Connected" : "Disconnected"}
          </div>
          
          <div className="url-input-wrapper">
            <input 
              className="url-input"
              value={url} 
              onChange={(e) => setUrl(e.target.value)} 
              placeholder="http://..."
            />
          </div>
          
          <button className="btn-icon" onClick={() => fetchTree()} title="更新">
            🔄
          </button>
        {/* パスワード設定ボタンを追加 */}
        <button className="btn-secondary" onClick={handleSetPassword} title="管理者パスワード設定">
            🔑
        </button>
      </div>

        {/* 右側：保存先設定 */}
        <div className="toolbar-group settings-group">
          <label className="save-label">Save to:</label>
          <div 
            className="save-path-display" 
            onClick={handleOpenDownloadFolder}
            title={`現在の保存先: ${saveDir}\nクリックで開く`}
          >
            {saveDir}
          </div>
          
          <button className="btn-secondary" onClick={handleChangeSaveDir}>
            変更
          </button>
          <button className="btn-secondary" onClick={handleOpenDownloadFolder}>
            📂 開く
          </button>
        </div>
      </div>

{/* --- 進捗バー (通知) --- */}
      {progress && (
        <div className={`progress-overlay ${progress.percent === 100 ? "finished-toast" : ""}`}>
          <div className="progress-header">
            <span>
               {/* isCalculating フラグがある場合はハッシュ計算中と出す */}
               {progress.isCalculating
                  ? "🧮 ハッシュ計算中..."
                  : progress.percent < 100 
                    ? (transferMode.current === "upload" ? "📤 アップロード中..." : "📥 ダウンロード中...")
                    : "✅ 完了"
               }
            </span>
            {/* 計算中は % を出さない */}
            <span>{progress.isCalculating ? "" : `${progress.percent}%`}</span>
          </div>
          
          <div className="progress-path">
            {progress.path.split(/[\\/]/).pop()}
          </div>
          
          {/* 計算中は value を指定せず、左右に動くアニメーションにする */}
          {progress.isCalculating ? (
             <progress></progress>
          ) : (
             <progress value={progress.percent} max="100"></progress>
          )}

          {progress.percent < 100 && (
            <div className="progress-details" style={{display: 'flex', justifyContent: 'space-between'}}>
              {/* 容量表示 */}
              <span>
                {formatBytes(progress.current)} / {formatBytes(progress.total)}
              </span>

              {/* 速度表示 */}
              <span>
                {formatBytes(progress.speed)}/s
              </span>
            </div>
          )}

          {/* 転送中のキャンセルボタン (100%未満 ＆ 計算中ではない時) */}
          {progress.percent < 100 && (
            <div className="notification-actions">
              <button 
                className="btn-notify" 
                onClick={handleCancelTransfer} 
                style={{ backgroundColor: '#c0392b' }} // 赤系の色
              >
                ⏹ キャンセル
              </button>
            </div>
          )}


          {/* 100%完了時のアクションボタン */}
          {progress.percent === 100 && (
            <div className="notification-actions">
              
              {/* ダウンロードの時だけボタンを表示する */}
              {transferMode.current === "download" && (
                <>
                  <button 
                    className="btn-notify" 
                    onClick={async () => {
                      await invoke("open_local_item", { path: `${saveDir}/${progress.path}`, mode: "open" });
                      setProgress(null);
                    }}
                  >
                    📄 開く
                  </button>
                  <button 
                    className="btn-notify" 
                    onClick={async () => {
                      await invoke("open_local_item", { path: `${saveDir}/${progress.path}`, mode: "reveal" });
                      setProgress(null);
                    }}
                  >
                    📂 場所を表示
                  </button>
                </>
              )}

              {/* 閉じるボタンは共通で表示 */}
              <button className="btn-notify-close" onClick={() => setProgress(null)}>
                ×
              </button>
            </div>
          )}
        </div>
      )}

      {/* --- コンテキストメニュー --- */}
      <div className="main-content" onContextMenu={handleBackgroundContextMenu}>
        {treeData.length > 0 ? (
          treeData.map((node, i) => (
            <TreeNode
              key={i}
              node={node}
              selectedPath={selectedPath}
              onSelect={setSelectedPath}
              onContextMenu={handleContextMenu}
              fileStatuses={fileStatuses} // 既存ファイル情報を渡す
              dragTargetDir={dragTargetDir} // ★追加：現在のドロップ対象パスを渡す
            />
          ))
        ) : (
          <div style={{ padding: "20px", color: "#666" }}>
            {connStatus.connected ? "読み込み中..." : "サーバーに接続してください。"}
          </div>
        )}
      </div>

      {menu && (
        <div className="context-menu" style={{ top: menu.y, left: menu.x }}>
          <div style={{ 
            padding: '8px 16px', 
            fontSize: '11px', 
            color: '#aaa',
            maxWidth: '250px',
            whiteSpace: 'nowrap',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            borderBottom: '1px solid #333', // 区切り線を追加
            marginBottom: '4px'
          }}>
            {/* node が null の場合はルートと表示する */}
            {menu.node ? menu.node.name : "ルート ( / )"}
          </div>

          {/* ファイル詳細情報 (Outdatedの場合) */}
          {menu.node && !menu.node.isDir && fileStatuses[menu.node.path]?.status === "outdated" && (
            <div style={{ padding: '8px 16px', fontSize: '10px', color: '#ccc', backgroundColor: 'rgba(255,0,0,0.1)' }}>
              
               {/* サイズ表示 */}
               <div style={{display:'flex', justifyContent:'space-between', marginBottom:'4px'}}>
                   <span>Svr Size: {formatBytes(menu.node.size)}</span>
                   <span>Loc Size: {formatBytes(fileStatuses[menu.node.path].local_size)}</span>
               </div>

               {/* ハッシュ表示 */}
               <div style={{color: '#aaa'}}>Server Hash:</div>
               <div style={{fontFamily:'monospace', marginBottom:'4px', overflow:'hidden', textOverflow:'ellipsis'}}>
                   {menu.node.hash || "なし"}
               </div>

               <div style={{color: '#aaa'}}>Local Hash:</div>
               <div style={{fontFamily:'monospace', color: '#ff8888', overflow:'hidden', textOverflow:'ellipsis'}}>
                 {/* ここで None なら「未計算」と出す */}
                 {fileStatuses[menu.node.path].local_hash || "(サイズ不一致のため未計算)"}
               </div>
            </div>
          )}

          <div style={{ borderTop: '1px solid #444', margin: '4px 0' }}></div>
          
          {/* 共通またはルートで可能な操作 */}
          {(!menu.node || menu.node.isDir) && (
            <div className="menu-item" onClick={handleCreateFolder}>
                📁 新規フォルダを作成
            </div>
          )}

          <div className="menu-item" onClick={handleUpload}>
             📤 TCPでここにアップロード
          </div>

          <div className="menu-item" onClick={() => { fetchTree(); setMenu(null); }}>
             🔄 最新の情報に更新
          </div>

          {/* ノード選択時のみここまで */}
          {/* ファイル/フォルダ選択時のみ表示する項目 */}
          {menu.node && (
            <>
              <div style={{ borderTop: '1px solid #444', margin: '4px 0' }}></div>

              {/* ダウンロード済みの場合に「場所を開く」を表示 */}
              {fileStatuses[menu.node.path] && (
                 <div className="menu-item" onClick={handleOpenLocalFile}>
                     📂 {menu.node.isDir ? "フォルダ" : "ファイル"}の場所を開く
                 </div>
              )}

              {/* クリップボード操作 */}
              <div className="menu-item" onClick={handleCopyName}>
                 📋 名前をコピー
              </div>
              <div className="menu-item" onClick={handleCopyPath}>
                 🔗 パス(相対)をコピー
              </div>

              <div style={{ borderTop: '1px solid #444', margin: '4px 0' }}></div>

              {/* ダウンロード関連 */}
              {menu.node.isDir ? (
                <div className="menu-item" onClick={handleDownloadFolder}>
                  📥 フォルダごとダウンロード
                </div>
              ) : (
                /* ファイルの場合 */
                <>
                  {/* outdated (更新あり) の場合 */}
                  {fileStatuses[menu.node.path]?.status === "outdated" ? (
                    <div className="menu-item" onClick={handleDownload} style={{ color: '#ffcc00' }}>
                      🔄 サーバーの内容で上書き更新
                    </div>
                  ) : fileStatuses[menu.node.path]?.status === "partial" ? (
                    /* partial (途中) の場合 */
                    <div className="menu-item" onClick={handleDownload} style={{ color: '#569cd6' }}>
                      ⏯ ダウンロードを再開
                    </div>
                  ) : (
                    /* 通常のダウンロード (未ダウンロード または 同期済み) */
                    <div className="menu-item" onClick={handleDownload}>
                      {fileStatuses[menu.node.path]?.status === "synced" ? "📥 TCPで再ダウンロード" : "📥 TCPでダウンロード"}
                    </div>
                  )}
                </>
              )}

              {menu.node.isPartial && (
                <div className="menu-item" onClick={handleResumeUpload} style={{ color: '#569cd6' }}>
                  🔄 アップロードを再開
                </div>
              )}

              {/* ハッシュ再計算 */}
              {!menu.node.isDir && fileStatuses[menu.node.path] && (
                 <div className="menu-item" onClick={handleRecalculateHash}>
                     🧮 ハッシュ値を強制再計算
                 </div>
              )}

              <div style={{ borderTop: '1px solid #444', margin: '4px 0' }}></div>
              
              {/* 削除サブメニュー */}
              <div className="menu-item menu-item-parent">
                <span>🗑️ 削除</span>
                <span className="menu-arrow">▶</span>
                
                {/* サブメニュー */}
                <div className="submenu">

                  {/* ローカル削除 (存在する場合のみ) */}
                  {fileStatuses[menu.node.path] ? (
                    <div className="menu-item" onClick={(e) => { e.stopPropagation(); handleLocalRemove(); }}>
                      💻 ローカルから削除
                    </div>
                  ) : (
                    <div className="menu-item" style={{ color: '#666', cursor: 'default' }}>
                      💻 (ローカルになし)
                    </div>
                  )}

                  {/* サーバー削除 */}
                  <div className="menu-item" onClick={(e) => { e.stopPropagation(); handleRemove(); }} style={{ color: '#ff6b6b' }}>
                      ☁️ サーバーから削除
                  </div>
                </div>
              </div>
              {/* 削除サブメニュー */}
            </>
          )}
          {/* ノード選択時のみここまで */}


        </div>
      )}
    </div>
  );
}

export default App;