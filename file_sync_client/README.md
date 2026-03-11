# File Sync Client (Tauri + React)

TauriとReact（Vite）を使用したファイル同期クライアントアプリです。

## 開発環境の準備 (Recommended IDE Setup)

快適な開発のために、以下のVS Code拡張機能のインストールを推奨します。

- [VS Code](https://code.visualstudio.com/)
- [Tauri 拡張機能](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode)
- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## ビルド方法 (How to Build)

以下の手順でプロジェクトのビルドを行います。

1.  **ディレクトリへ移動**
    ```bash
    cd file_sync/file_sync_client
    ```

2.  **依存関係のインストール**
    ```bash
    npm install
    ```

3.  **ビルドの実行**
    ```bash
    npm run tauri build
    ```
    ※ ビルドされたバイナリは `src-tauri/target/release` 内に出力されます。

## 開発用コマンド

### 開発モードの起動
コードの変更をリアルタイムで反映しながら開発を行う場合：
```bash
npm run tauri dev
```