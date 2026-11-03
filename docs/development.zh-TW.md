# OxideMerge 開發指南

[English](development.md) | 繁體中文

本指南適用於建置、測試或發布 OxideMerge 的開發人員。若要安裝及使用程式，
請參閱[讀我檔案](../README_zh-TW.md)與[使用者指南](user-guide.zh-TW.adoc)。

## 目錄

- [從原始碼建置](#從原始碼建置)
- [建置指令](#建置指令)
- [結果的命名方式](#結果的命名方式)
- [測試](#測試)
- [持續整合](#持續整合)
- [版本發布](#版本發布)

## 從原始碼建置

請先透過 [rustup](https://rustup.rs/) 安裝 Rust 工具鏈。Linux 另需要 C 編譯器。
X11、Wayland、鍵盤與 OpenGL 函式庫會在程式啟動時才載入，因此建置時不需要安裝這些函式庫的開發套件。

在儲存庫資料夾中執行：

```sh
cargo build --release --locked
cargo test --locked
```

macOS 與 Linux 的程式位於 `target/release/oxidemerge`，Windows 則為
`target/release/oxidemerge.exe`。執行 `make demo` 可開啟範例資料夾比較。

### 版本資訊

「關於」與 `--version` 會顯示目前執行的建置版本：

- 開發版本會顯示 Git 短版修訂編號。若已追蹤的檔案有尚未提交的變更，編號後會加上 `-dirty`。
- 從 `v版本號` 標籤乾淨建置的最佳化版本，會顯示以世界協調時間表示的建置日期。
  若要讓每次建置都得到相同的日期（可重現的建置），請設定 `SOURCE_DATE_EPOCH`。
- 沒有 Git 資訊時，若 `GITHUB_SHA` 有效就使用它，否則顯示 `unknown`。

## 建置指令

[Makefile](../Makefile) 為常用工作提供簡短的指令，需要 GNU Make。
執行 `make help` 可列出所有目標。

### 建置與執行

| 指令 | 用途 |
| --- | --- |
| `make` 或 `make release` | 建置最佳化的程式。 |
| `make build` | 建置偵錯版本。 |
| `make run ARGS="a.txt b.txt"` | 以指定的路徑執行偵錯版本。 |
| `make demo`、`make demo-text`、`make demo-3way`、`make demo-3way-merge`、`make demo-3way-folder`、`make demo-conflict` | 開啟範例比較。 |
| `make demo-web`、`make demo-web-text`、`make demo-web-headings`、`make demo-web-tables` | 開啟網頁比較範例。後三者示範「格式化」模式。 |
| `make install`、`make uninstall` | 將程式安裝到 Cargo 的程式資料夾（預設為 `~/.cargo/bin`），或從中移除。 |
| `make shell-register` | 安裝程式，並註冊檔案管理員整合。 |
| `make clean` | 移除建置結果與本機產生的套件。 |

[samples/README.md](../samples/README.md) 列出每個範例及其示範的功能。
網頁比較範例請參閱 [samples/web/README.md](../samples/web/README.md)。

### 檢查程式碼

| 指令 | 用途 |
| --- | --- |
| `make fmt-check` | 檢查 Rust 程式碼格式；`make fmt` 會自動修正格式。 |
| `make check` | 依目前的建置設定，對所有目標執行型別檢查。 |
| `make lint` | 執行 Clippy，並將警告視為錯誤。 |
| `make test` | 執行單元測試、介面測試與指令列測試。 |
| `make ci` | 依序執行 `check`、`lint` 與 `test`。 |
| `make gui-smoke` | 建置發行版本、開啟實際視窗，並檢查比較結果。 |
| `make check-platforms` | 對 Makefile 列出的 Windows、macOS 與 Linux 目標執行型別檢查。請先以 `rustup target add` 安裝各目標的標準函式庫。 |

### 產生套件

| 指令 | 用途 |
| --- | --- |
| `make dist` | 在 `dist/` 產生套件：Windows 為 ZIP 檔，macOS 與 Linux 為 TAR.GZ 檔。 |
| `make appimage` | 依本機處理器架構（x86_64 或 aarch64）產生 Linux AppImage。需要 X11、Wayland 與鍵盤函式庫，並會下載 `appimagetool`。 |
| `make user-guide` | 在 `dist/` 產生英文與繁體中文 PDF 指南。 |

`make user-guide` 需要 Asciidoctor PDF、`curl`、`shasum`、含 `fontTools` 的 Python 3、
fontconfig 與 DejaVu Sans 字型。建置指令碼會自動下載所需的 Noto Sans CJK 字型。

## 結果的命名方式

凡是可能不是單純成功的功能，都要回報它屬於七種結果中的哪一種。`core::problem::Problem`
就是這套分類，請勿另行建立平行的分類：

| 結果 | 含意 | 來源範例 |
| --- | --- | --- |
| `ambiguous` | 輸入還有同樣可能的另一種解讀；結果仍成立，但需要人工確認 | 無法區分的重複項目：語意與樹狀檢視會自行標示這類列 |
| `stale` | 輸入已不是產生結果或計畫時的樣子 | 同步預覽後檔案有變更 |
| `unsupported` | 輸入或要求屬於此功能拒絕處理的類型 | 不支援的格式、版本或檔案類型；缺少或重複的索引鍵欄位；筆數超過上限的表格 |
| `read_failure` | 讀取失敗或必然失敗 | 沒有權限；其他程式開著該檔案且不允許共用 |
| `write_failure` | 寫入失敗或必然失敗 | 沒有權限；磁碟空間不足 |
| `canceled` | 使用者中止 | 「停止」或「取消」 |
| `recovery_needed` | 變更中途停止且無法完全復原，並保留了復原日誌 | 無法完全還原的回復 |

功能若有自己的結果類型，應對應到上述分類，而非取而代之：I/O 錯誤使用 `Problem::of_io`
（結果依讀取或寫入而不同），無法依要求使用的索引鍵或表格則使用 `Problem::of_table`。
需要新的對應時，請在這些函式旁新增對應函式，而不是新增列舉。

這些名稱會以兩種方式呈現給使用者。`--sync-apply` 與 `--sync-recover` 會在 JSON 中以
`problem` 輸出結果，使用上表中的序列化名稱，供指令稿判斷；這屬於相容性介面，
變更列舉名稱時必須在發行說明中記載。`Problem::label()` 提供已翻譯的簡短名稱
（`lang/*.json` 中的 `problem.*`），供會先標示問題類型再說明細節的檢視使用。
細節仍沿用各功能自己的用語：分類說明的是發生了哪一類情況，而不是發生了什麼事。

## 測試

`cargo test` 涵蓋三個部分：

- **核心功能：** 比較、合併、修補檔、資料夾與檔案操作。`src/core/regress.rs` 中的隨機測試
  也會產生差異、合併與修補案例進行驗證。
- **介面操作：** 在沒有顯示器的情況下操作實際的檢視畫面，涵蓋選單、編輯、拖放檔案與儲存結果。
  視窗測試失敗時，會將詳細資訊寫入 `target/gui-tests/<test>.txt`。
- **指令列行為：** `tests/cli.rs` 中的測試會執行程式，檢查結束代碼、報告、修補檔與錯誤訊息。

測試使用獨立的設定資料夾，不會變更您的設定。其他不應影響設定的測試，
也可以將 `OXIDEMERGE_CONFIG_DIR` 指向另一個資料夾。
部分網頁測試會啟動本機 HTTP 伺服器，因此測試環境必須允許程式監聽本機網路連接埠。

### 已保存的報告

以固定輸入產生的語意、版本摘要與影響報告，必須與 `tests/fixtures/reports` 中的
JSON 與 XML 報告相同，只有擷取時間可以不同。若您有意變更報告內容，
請執行下列指令更新已保存的報告，再檢查其中的變更：

```sh
OXIDEMERGE_UPDATE_REPORTS=1 cargo test --test cli reports_match
```

### 格式檢查

下列指令只執行格式檢查：

```sh
cargo test --release --locked format_regression -- --nocapture
```

這些檢查確認修補檔會保留位元組順序標記與換行字元、已忽略的文字變更維持忽略，
以及在混用換行字元時，合併、儲存與復原都能正確運作。
它們也會確認結果與 Git 簽出及 Git 修補檔相容，因此必須安裝 Git。
持續整合會在 Windows、Linux 與 macOS 上，於完整測試中執行這些檢查。

### 隨機測試

隨機測試失敗時會輸出一個種子值。可用此種子值重新執行相同案例，或執行更多案例：

```sh
OXIDEMERGE_TEST_SEED=<seed> cargo test regress
OXIDEMERGE_TEST_CASES=20000 cargo test --release regress
```

### 視窗測試

若要以實際的程式視窗進行測試，請執行 `make gui-smoke` 或
`scripts/gui-smoke.sh path/to/oxidemerge`。指令碼會檢查雙檔、三檔與資料夾比較。
Linux 沒有顯示環境時，會自動啟動 Xvfb。設定 `OXIDEMERGE_SMOKE_REPORT=<file>` 後，
程式會在比較完成時，將分頁與訊息寫入該檔案，然後關閉。

若要製作文件圖片，設定 `OXIDEMERGE_SCREENSHOT=<file.png>` 後，程式會在比較完成時，
直接儲存它所繪製的視窗畫面（不需擷取螢幕），然後關閉。將 `OXIDEMERGE_SCREENSHOT_COMMAND`
設為命令的標籤鍵（例如 `cmd.sync_folders`），會先執行該命令。搭配 `OXIDEMERGE_CONFIG_DIR`
與設定 `language` 的 `config.json`，即可擷取其他語言的畫面。

### 效能測試

效能測試不包含在 `cargo test` 中。指令、量測方法與參考結果，請參閱[效能測試說明](benchmarks.md)（英文）。

## 持續整合

推送到 `main` 或提出合併請求時，會執行[持續整合工作流程](../.github/workflows/ci.yml)。

| 觸發時機 | 執行內容 |
| --- | --- |
| 合併請求 | 在 Windows x64、Linux x64 與 Apple 晶片的 macOS 上執行 Clippy、完整測試、建置發行版本，並檢查指令列與視窗行為。 |
| 推送到 `main` | 執行合併請求的所有檢查，並產生及驗證所有套件。套件只保留為建置檔案，不會發布。 |
| 推送發行標籤 | 與推送到 `main` 相同，完成後發布版本。 |
| 手動執行 | 執行合併請求的檢查。開啟 `packages` 可產生並驗證所有套件；開啟 `stress_cases` 或 `benchmarks` 可執行更多隨機測試案例或效能測試。 |

其他說明：

- Windows 與 Linux 的視窗檢查使用 Mesa 軟體繪圖，Linux 另使用 Xvfb。
- 測試失敗時，輸出內容會保留在工作流程執行結果的 `test-output-*` 檔案中。
- 另有一項 `cargo deny` 工作會檢查相依套件，請參閱 [SECURITY.md](../SECURITY.md)（英文）。
- 額外的隨機測試與效能測試為選用項目，版本發布不受其結果影響。

### 套件

推送到 `main`、推送發行標籤，以及開啟 `packages` 的手動執行，都會產生並驗證下列套件：

- **Windows x64 與 Arm64：** 只包含 `oxidemerge.exe` 的 ZIP 套件。會檢查每個程式的處理器架構，並實際啟動。
- **Linux x64 與 Arm64：** 在 Ubuntu 22.04 上建置的 TAR.GZ 套件。會實際啟動程式，並確認不會變更設定。
- **Linux AppImage：** 在使用 glibc 2.28 的 AlmaLinux 8 上建置。套件內含 X11、Wayland 與鍵盤函式庫，
  系統缺少這些函式庫時才會使用。兩種處理器架構都會在 Ubuntu 22.04 與 AlmaLinux 8 上，
  檢查函式庫載入、指令列行為與視窗啟動。
- **macOS：** 採用臨時簽署的通用應用程式，以 DMG 套件提供。會驗證 Apple 晶片與 Intel 兩種程式碼，
  並透過 Rosetta 測試 Intel 程式。
- **說明文件：** 英文與繁體中文 PDF 指南。若字型缺少所需字元，文件建置會失敗。

Windows 與 Linux 的 Arm64 套件會在實體 Arm 架構電腦上建置與測試，而非使用模擬環境。

## 版本發布

### 發布規則

- 推送 `v0.9.8` 這類標籤時，會將套件發布到 GitHub 的發行頁面。
- 標籤必須與 `Cargo.toml` 中的版本一致。
- `CHANGELOG.md` 必須有該版本的段落。
- 含有 `-` 的標籤（例如 `v1.0.0-rc1`）會發布為預先發行版本。

發行說明取自變更紀錄中對應的段落，後面附上下載說明。工作流程只會讀取變更紀錄，不會修改它。

發布前，工作流程會確認該版本尚未發布，並確認所有預期的套件都已產生，且沒有多餘的檔案。
每個版本都會註明建置時使用的原始碼提交，GitHub 也會列出每個檔案的 SHA-256 檢查碼。
以 0.9.8 版為例，發行版本只包含下列檔案：

```text
oxidemerge-0.9.8-linux-arm64.AppImage
oxidemerge-0.9.8-linux-arm64.tar.gz
oxidemerge-0.9.8-linux-x64.AppImage
oxidemerge-0.9.8-linux-x64.tar.gz
oxidemerge-0.9.8-macos-universal.dmg
oxidemerge-0.9.8-user-guide.pdf
oxidemerge-0.9.8-user-guide.zh-TW.pdf
oxidemerge-0.9.8-windows-arm64.zip
oxidemerge-0.9.8-windows-x64.zip
```

### 發布新版本

以下以 `0.9.8` 版為例，請改為您要發布的版本號。

1. 修改 `Cargo.toml` 中的 `version`，再執行 `cargo build` 更新 `Cargo.lock`。
2. 檢查 `CHANGELOG.md` 中 `0.9.8` 的段落，將標題中的 `Unreleased` 改為 `YYYY-MM-DD` 格式的發行日期。
3. 執行 `make fmt-check ci`，並以 `scripts/release-notes.sh 0.9.8` 檢查發行說明。
4. 提交並推送到 `main`，等待所有平台的必要持續整合檢查都通過。
5. 為通過檢查的提交加上標籤，並推送標籤。加上標籤前，請先以 `git log -1 --oneline`
   確認 `HEAD` 就是通過檢查的提交。

   ```sh
   git tag -a v0.9.8 -m "OxideMerge 0.9.8"
   git push origin v0.9.8
   ```

6. 工作流程完成後，檢查 GitHub 上的新發行版本，確認所有套件與 PDF 指南都已提供。
7. 在 `CHANGELOG.md` 中為下一個版本加入 `Unreleased` 段落，之後的變更都記錄在這裡。

### 修正建置失敗的版本

工作流程只會在所有建置與測試工作都通過後才發布版本。因此，若發行標籤的其中一項建置或測試工作失敗，
就不會發布任何內容，您可以從修正後的提交，重新發布同一個版本。以下以 `v0.9.8` 為例，請改為建置失敗的版本號。

1. 確認 GitHub 上沒有 `v0.9.8` 的發行版本：

   ```sh
   gh release view v0.9.8
   ```

   此指令需要已登入的 GitHub 命令列工具，且必須回覆 `release not found`。
   您也可以到[發行頁面](https://github.com/kuopinghsu/OxideMerge/releases)確認。
   若該版本已存在，即使只有部分檔案，也請勿移動標籤，改依[修正已發布的版本](#修正已發布的版本)處理。

2. 在 `main` 上修正問題。`Cargo.toml` 與變更紀錄中的版本維持 `0.9.8`；
   若發行日期改變，請更新變更紀錄標題中的日期。執行 `make fmt-check ci`，並以 `scripts/release-notes.sh 0.9.8` 檢查發行說明，
   再提交並推送到 `main`，
   等待所有平台的必要持續整合檢查都通過。

3. 將標籤移到修正後的提交，再重新推送：

   ```sh
   git switch main
   git pull --ff-only origin main
   git log -1 --oneline
   git tag -a -f v0.9.8 -m "OxideMerge 0.9.8"
   git push --force origin v0.9.8
   ```

   `git log` 顯示的必須是通過檢查的提交。推送標籤後，會為修正後的提交重新執行工作流程。

   請勿對舊標籤失敗的執行結果使用「**Re-run jobs**」（重新執行工作）。該次執行會建置舊的提交，
   可能以移動後的標籤發布舊的套件。

4. 工作流程完成後，檢查 GitHub 上的新發行版本，確認所有套件與 PDF 指南都已提供。

若建置再次失敗，請重複步驟 2 到 4。其他複本仍保有舊標籤，可執行 `git fetch --tags --force` 更新。

### 修正已發布的版本

版本發布後，請勿再變更。使用者已下載的套件，必須始終與公布的檢查碼相符；
工作流程也會拒絕發布已存在的版本。請改為發布新的修補版本：

1. 在 `main` 上修正問題。
2. 以下一個修補版本號（例如 `0.9.8` 之後使用 `0.9.9`）依照[發布新版本](#發布新版本)的步驟操作，
   並在新版本的變更紀錄段落中說明修正內容。

請保留舊標籤與舊版本已發布的檔案。
