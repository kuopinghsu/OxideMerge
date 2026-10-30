# OxideMerge

[English](README.md) | 繁體中文

![Windows、macOS 與 Linux 上的 OxideMerge](images/banner.jpg)

OxideMerge 是免費、開放原始碼的檔案與資料夾比較及合併工具，支援 **Windows、macOS 與 Linux**。
透過圖形介面，你可以檢視差異、整合修改內容，並解決合併衝突。

OxideMerge 以 [WinMerge](https://github.com/WinMerge/winmerge) 為設計參考，使用 Rust 開發。
除了文字與資料夾合併，也支援二進位檔、影像與網頁比較。

[下載](https://github.com/kuopinghsu/OxideMerge/releases) ·
[使用者指南](docs/user-guide.zh-TW.adoc) ·
[版本紀錄](CHANGELOG.md)

![OxideMerge 起始頁](images/start.png)

## 目錄

- [主要功能](#主要功能)
- [安裝](#安裝)
- [快速入門](#快速入門)
- [指令列使用方式](#指令列使用方式)
- [Git 與檔案管理員整合](#git-與檔案管理員整合)
- [設定](#設定)
- [說明文件](#說明文件)
- [使用限制](#使用限制)
- [開發](#開發)
- [版本發布](#版本發布)
- [參與貢獻](#參與貢獻)
- [安全性](#安全性)
- [授權條款](#授權條款)

## 主要功能

### 比較檔案與資料夾

- 比較兩個版本，或以共同的原始版本為基準進行三方比較。
- 掃描資料夾與子資料夾、依檔名篩選檔案，並在資料夾之間複製選取的項目。
- 在「**所有差異**」檢視中，集中檢查有變更的文字檔。
- 將支援的壓縮檔當作資料夾比較，包括 ZIP、TAR 與 7z。
- 將檔案或資料夾與 Git 中的分支、標籤或提交版本比較。

### 檢視與合併文字

- 並排顯示差異，包括同一行內的變更與移動過的文字區塊。
- 在版本之間複製個別變更、編輯文字，以及復原或重做編輯操作。
- 在三方比較中自動合併不衝突的變更，再檢查剩餘的衝突。
- 開啟含有版本控制衝突標記的檔案，透過圖形介面解決衝突。
- 忽略空白字元、註解或符合指定模式的內容，聚焦於需要檢查的變更。
  可重複使用的比較規則與數值容許誤差設定，也適合用來比較自動產生的檔案與量測資料。
- 提供語法醒目提示、尋找與取代、書籤，以及 CSV／TSV 表格檢視。
- 可設定同步點，讓比較配錯的行重新對齊。

### 比較其他內容

- **二進位檔：** 以並排的十六進位檢視檢查位元組差異。
- **影像：** 並排比較，或使用疊合、滑動分隔線、閃爍與差異檢視。
- **網頁：** 比較網頁執行指令碼後顯示的文字、在網頁上醒目提示文字差異的整頁截圖、HTML 原始碼或下載的資源。

### 儲存與分享成果

- 匯出 HTML 報告供瀏覽器檢閱、一次顯示一個差異的 HTML 投影片、供其他程式讀取的 XML 報告，或將資料夾比較摘要匯出為 CSV 與純文字。
- 產生修補檔，或套用統一格式的修補檔，檢查結果後再儲存。
- 將比較設定儲存為專案檔，將文字比較連同書籤與註記一起儲存，或建立包含檔案內容、比較設定與註解的可攜式資料夾審閱。
- 搭配 Git 使用，或透過指令列與系統的檔案管理員啟動比較。

介面提供淺色與深色主題，並內建 10 種語言：英文、繁體中文、簡體中文、日文、韓文、德文、法文、
西班牙文、俄文與巴西葡萄牙文。也可以透過語言檔載入其他翻譯。

### 介面與螢幕截圖

**資料夾比較**

![資料夾比較](images/folder.png)

**文字比較**

![文字比較](images/text.png)

可攜式資料夾審閱（`.oxmreview`）可保存完整內容、比較設定、書籤與註解；CSV/TSV 表格支援多行記錄、儲存格編輯與合併。文件轉換預設需另行安裝 PDF 或 Office 轉換工具。

## 安裝

請從 [GitHub Releases](https://github.com/kuopinghsu/OxideMerge/releases) 下載適合系統的套件。
每個發行版本也提供英文與繁體中文 PDF 使用者指南；發行頁面會列出每個下載檔案的 SHA-256 摘要，可用來驗證檔案。

### Windows

支援 Windows 10 與 11，提供 x64 與 Arm64 版本。

依處理器架構下載檔名結尾為 `windows-x64.zip` 或 `windows-arm64.zip` 的套件，解壓縮後執行
`oxidemerge.exe` 即可，不必透過安裝程式。完整檔名包含版本號，例如
`oxidemerge-<version>-windows-x64.zip`。

### macOS

支援 macOS 11 以上版本，適用於 Apple 晶片與 Intel Mac。

開啟檔名結尾為 `macos-universal.dmg` 的套件，將 **OxideMerge** 拖移至「**應用程式**」。

應用程式採用臨時簽署（ad hoc signing），尚未經過 Apple 公證。若 macOS 阻擋首次啟動，
請先嘗試開啟一次，再到「**系統設定 → 隱私權與安全性**」選擇「**強制打開**」。
詳情請參閱 [macOS 簽署說明](docs/macos-signing.md)（英文）。

### Linux

支援使用 X11 或 Wayland 的 x64 與 Arm64 系統。

- **AppImage：** 可在 Ubuntu 20.04、RHEL 8、Debian 10 及更新版本執行。
  先以 `chmod +x <filename>.AppImage` 設為可執行，再啟動檔案。
  若系統無法使用 FUSE，啟動時請加上 `--appimage-extract-and-run`。
- **TAR 壓縮檔：** 內含獨立執行檔、桌面項目檔與圖示，適合希望直接使用執行檔的使用者。

檔案對話框使用 `xdg-desktop-portal`，一般桌面環境通常已提供。
若需要顯示中文、日文與韓文，請安裝 Noto Sans CJK 等適用字型。

## 快速入門

啟動 OxideMerge 後會顯示起始頁。你可以選擇要比較的檔案或資料夾、將項目拖放到視窗中，
或重新開啟最近的比較紀錄。

### 比較兩個檔案或資料夾

開啟兩個檔案，即可並排檢視差異。使用「**比較**」選單切換差異，透過「**合併**」
將變更複製到另一側，確認後再儲存。

比較資料夾時，可視需要啟用子資料夾比較。選取項目後可預覽內容，或在獨立的比較分頁中開啟。
使用「**所有差異**」可集中檢查有變更的文字檔。

### 合併三個版本

依序開啟：**你的版本、共同祖先版本、另一份版本**。
共同祖先版本是另外兩個版本修改前的原始版本，會顯示在中間窗格。

選擇「**合併 → 自動合併**」，整合不衝突的變更，再檢查剩餘衝突並儲存結果。
也可以直接開啟含有衝突標記的單一檔案。

三方資料夾合併會將結果寫入中間的資料夾。「**合併 → 自動合併資料夾...**」會先列出預計複製、
刪除與合併的項目，待你確認後才執行。

## 指令列使用方式

以下範例假設 `oxidemerge` 已加入 `PATH` 環境變數。若尚未加入，請改用執行檔的完整路徑。
macOS 的執行檔通常位於 `/Applications/OxideMerge.app/Contents/MacOS/oxidemerge`。

```sh
# 比較兩個文字檔
oxidemerge old.txt new.txt

# 比較資料夾，包含子資料夾
oxidemerge -r old-folder new-folder

# 合併三個檔案，將中間窗格儲存到另一個輸出檔案
oxidemerge mine.txt base.txt theirs.txt -o merged.txt

# 開啟含有合併衝突標記的檔案
oxidemerge conflict.txt

# 匯出包含文字差異的資料夾報告，不開啟視窗
oxidemerge -r old-folder new-folder --report report.html --report-diffs
```

撰寫指令碼時，可使用 `--cli` 在不開啟視窗的情況下執行。結束代碼 `0` 表示內容相同、`1` 表示有差異、
`2` 表示發生錯誤。執行 `oxidemerge --help` 可查看完整的指令列選項。
常用選項包括：

- `-r`：包含子資料夾；`-f "*.rs;!target/"`：篩選資料夾內容。
- `-t text`、`-t binary`、`-t image`、`-t web`：指定比較類型。
- `-o <file>`：將合併結果儲存到另一個檔案。
- `--ignore-case`、`--ignore-eol`、`--ignore-comments`：忽略指定類型的變更。
- `--rules <name-or-path>`：使用比較規則檔。
- `--git-rev <revision>`：將單一檔案或資料夾與 Git 修訂版本比較。
- `--report <file>`：匯出 `.html` 或 `.xml` 報告，資料夾比較也可匯出 `.csv` 或 `.txt`；加上 `--report-diffs`
  可在 HTML 資料夾報告中包含文字差異，加上 `--slideshow` 則匯出一次顯示一個差異的 HTML 投影片。
- `--patch <file>`：從雙向檔案或資料夾比較產生修補檔。

`--cli`、`--report` 與 `--patch` 都不會開啟視窗。Windows 也接受 `/r`、`/f`、`/o` 等支援的
WinMerge 風格參數。完整選項與指令碼範例請參閱[使用者指南](docs/user-guide.zh-TW.adoc)。

## Git 與檔案管理員整合

### Git

OxideMerge 可作為 Git 的圖形化比較與合併工具。將 `oxidemerge` 加入 `PATH` 後，
在 Bash 或 Git Bash 等相容於 POSIX 的命令殼層中執行以下設定：

```sh
git config --global diff.tool oxidemerge
git config --global difftool.oxidemerge.cmd 'oxidemerge "$LOCAL" "$REMOTE"'
git config --global merge.tool oxidemerge
git config --global mergetool.oxidemerge.cmd \
  'oxidemerge "$LOCAL" "$BASE" "$REMOTE" -o "$MERGED" --dl Local --dr Remote --wl --wr'
git config --global mergetool.oxidemerge.trustExitCode false
```

使用 `git difftool` 比較個別檔案，或以 `git difftool --dir-diff` 開啟資料夾比較。
使用 `git mergetool` 解決衝突時，請檢視三個版本、將變更合併至中間窗格，再儲存。
結果會寫入 Git 指定的合併輸出檔案，不會修改祖先版本。

若執行檔未加入 `PATH`，請在設定指令中使用完整路徑。
詳情請參閱[使用者指南](docs/user-guide.zh-TW.adoc)。

### 檔案管理員

從「**工具**」選單啟用整合，或使用已安裝的執行檔執行 `oxidemerge --register-shell`。
若要移除整合，請執行 `oxidemerge --unregister-shell`。

- **Windows 檔案總管：** 透過右鍵選單比較選取的檔案或資料夾。
  Windows 11 的項目位於「**顯示其他選項**」中。
- **macOS Finder：** 使用 OxideMerge 的「快速動作」與「服務」選單項目。
- **Linux：** 使用 Nautilus、Nemo 或 Dolphin 提供的選單項目。

可以立即比較選取的項目，也可以先用「**選為左側**」或「**選為中間**」記住項目，稍後再選擇其他項目比較。
選單項目會指向註冊時使用的執行檔；若搬移程式，請重新註冊。

## 設定

使用「**選項**」調整比較方式、外觀與語言。設定檔儲存在使用者設定資料夾中：

- **Windows：** `%APPDATA%\OxideMerge\config.json`
- **macOS：** `~/Library/Application Support/OxideMerge/config.json`
- **Linux：** `~/.config/OxideMerge/config.json`

若要以可攜式方式使用，請在執行檔所在的資料夾中建立空白的 `oxidemerge.json`。
若要使用獨立的設定資料夾，請將 `OXIDEMERGE_CONFIG_DIR` 設為指定路徑。

### 篩選器與比較規則

檔案篩選器決定資料夾比較中要顯示哪些項目。以 `;` 分隔多個條件，使用 `!` 排除符合的項目，
並在結尾加上 `/` 或 `\` 表示資料夾。例如 `*.rs;*.toml;!target/` 會包含 Rust 與 TOML 檔案，
並排除 `target` 資料夾。也可以使用設定資料夾內 `Filters` 子資料夾中的 WinMerge `.flt` 檔案。

比較規則用來指定要忽略哪些內容差異。透過「**選項 → 比較規則**」管理 `.oxrules` 檔案，
設定要忽略的行、文字取代與要忽略的區塊。規則可匯入與匯出，方便跨專案或團隊共用。
每個資料夾比較也可以選用自己的規則檔。

### 語言

內建翻譯位於 [lang/](lang)。若要新增或覆寫翻譯，請將 `lang/en.json` 複製為 `<code>.json`，
翻譯其中的值，並保留所有 `{placeholder}` 名稱。
將檔案放在執行檔旁或使用者設定資料夾內的 `Languages` 子資料夾中。
缺少的訊息，或預留位置不符的翻譯，會改以英文顯示。

## 說明文件

- [繁體中文使用者指南](docs/user-guide.zh-TW.adoc)：操作流程、比較選項、快速鍵、Git 整合與疑難排解。
- [英文使用者指南](docs/user-guide.adoc)。
- [效能測試](docs/benchmarks.md)：量測結果與執行方式（英文）。
- [變更紀錄](CHANGELOG.md)：各版本的更新內容（英文）。

按 **F1** 或點選程式中的 **?** 按鈕，即可開啟「說明與快速鍵」。對話框會列出鍵盤與滑鼠操作，
並顯示目前使用的設定檔位置。macOS 的多數快速鍵以 Command 對應 Windows 與 Linux 的 Ctrl，
介面會依平台顯示適用的按鍵。

## 使用限制

- 壓縮檔以唯讀方式比較，無法將變更寫回壓縮檔。不支援 RAR。
- 啟用自動格式化時，JSON 與 XML 檔案為唯讀。若要編輯或合併，請關閉此功能。
- 網頁螢幕截圖需要已安裝的 Chromium 系列瀏覽器。沒有瀏覽器時，文字與 HTML 模式不會執行 JavaScript，
  因此可能無法取得動態載入的內容。不支援需要登入驗證的瀏覽情境；這類網頁可能只顯示登入畫面。
- 文字編輯以行為單位，主要用於檢查與合併變更。
- 支援 WinMerge 專案檔與篩選器檔案，但不支援 WinMerge 外掛 DLL 與指令碼。
  解譯器外掛程式（Unpacker plugins）使用外部指令。
- 由機器輔助完成的翻譯尚未經母語人士審閱。

## 開發

### 從原始碼建置

請透過 [rustup](https://rustup.rs/) 安裝 Rust 工具鏈。Linux 另需 C 編譯器。
X11、Wayland、鍵盤與 OpenGL 函式庫會在執行時載入，建置時不需要安裝這些函式庫的開發套件。

在儲存庫根目錄執行：

```sh
cargo build --release --locked
cargo test --locked
```

macOS 與 Linux 的執行檔位於 `target/release/oxidemerge`，Windows 則為
`target/release/oxidemerge.exe`。執行 `make demo` 可試用範例資料夾比較。

### 建置與維護指令

[Makefile](Makefile) 提供常用工作的簡便指令，需要 GNU Make。
執行 `make help` 可列出所有目標。

**建置與執行**

- `make` 或 `make release`：建置最佳化的執行檔。
- `make build`：建置偵錯版本。
- `make run ARGS="a.txt b.txt"`：以指定的比較路徑執行偵錯版本。
- `make demo`、`make demo-text`、`make demo-3way`、`make demo-conflict`：開啟各類範例比較。網頁比較請參閱 `samples/web/README.md`。
- `make install` 或 `make uninstall`：將程式安裝至 Cargo 的執行檔目錄，或從該目錄移除；
  預設目錄為 `~/.cargo/bin`。
- `make shell-register`：安裝執行檔，並註冊檔案管理員整合。
- `make clean`：移除建置結果與本機產生的發布套件。

**程式碼品質檢查**

- `make fmt-check`：檢查 Rust 程式碼格式；`make fmt` 可自動套用格式。
- `make check`：依目前的建置設定，對所有目標執行型別檢查。
- `make lint`：執行 Clippy，並將警告視為錯誤。
- `make test`：執行單元測試、介面測試與指令列測試。
- `make ci`：執行 `check`、`lint` 與 `test`。
- `make gui-smoke`：建置發行版本、開啟實際視窗，並驗證比較結果。
- `make check-platforms`：對 Makefile 列出的 Windows、macOS 與 Linux 目標執行型別檢查。
  請先使用 `rustup target add` 安裝各目標的標準函式庫。

**產生發布檔案**

- `make dist`：在 `dist/` 產生發布套件；Windows 為 ZIP，macOS 與 Linux 為 TAR.GZ。
- `make appimage`：依本機架構（x86_64 或 aarch64）產生 Linux AppImage。
  需要 X11、Wayland 與鍵盤函式庫，並會下載 `appimagetool`。
- `make user-guide`：在 `dist/` 產生英文與繁體中文 PDF 指南。
  需要 Asciidoctor PDF、`curl`、`shasum`、含 `fontTools` 的 Python 3、fontconfig 與 DejaVu Sans 字型。
  建置指令碼會下載所需的 Noto Sans CJK 字型。

### 測試

`cargo test` 涵蓋三個面向：

- **核心功能：** 比較、合併、修補檔、資料夾與檔案操作。`src/core/regress.rs` 的隨機測試
  也會產生差異、合併與修補案例，驗證處理結果。
- **介面操作：** 在不需要顯示器的情況下操作實際檢視，包括選單、編輯、拖放檔案與儲存結果。
  視窗測試失敗時，會將診斷資訊寫入 `target/gui-tests/<test>.txt`。
- **指令列行為：** `tests/cli.rs` 會執行程式，驗證結束代碼、報告、修補檔與錯誤訊息。

測試使用獨立的設定資料夾，不會變更你的應用程式設定。其他需要隔離設定的執行情境，
也可以將 `OXIDEMERGE_CONFIG_DIR` 指向專用資料夾。
部分網頁測試會啟動本機 HTTP 伺服器，因此測試環境必須允許建立本機監聽通訊端。

隨機測試失敗時會輸出種子值，可用來重現測試案例：

```sh
OXIDEMERGE_TEST_SEED=<seed> cargo test regress
OXIDEMERGE_TEST_CASES=20000 cargo test --release regress
```

若要使用實際應用程式視窗進行檢查，請執行 `make gui-smoke` 或
`scripts/gui-smoke.sh path/to/oxidemerge`。指令碼會檢查雙檔、三檔與資料夾比較。
Linux 沒有可用顯示環境時，會啟動 Xvfb。設定 `OXIDEMERGE_SMOKE_REPORT=<file>` 後，
程式會在比較完成時記錄分頁與訊息，然後關閉。

效能測試另外執行。指令、量測方法與基準結果請參閱[效能測試說明](docs/benchmarks.md)（英文）。

### 持續整合

推送至 `main` 或提出 Pull Request 時，會執行 [CI 工作流程](.github/workflows/ci.yml)。
Pull Request 會在 Windows x64、Linux x64 與 macOS Apple 晶片上執行 Clippy 檢查、完整測試、建置發行版本，
並驗證指令列與視窗行為。Windows 與 Linux 的視窗檢查使用 Mesa 軟體渲染，Linux 另使用 Xvfb。
失敗的測試輸出會保留為 `test-output-*` 成品。獨立的 `cargo deny` 工作會檢查相依套件，
詳見[安全性說明](SECURITY.md)。

每次推送至 `main` 或推送發行標籤，都會驗證完整套件，包括 Windows/Linux Arm64 原生測試、
Intel macOS 建置、AppImage 與 PDF 指南。推送至 `main` 只產生經驗證的建置成品，不發布版本；
只有推送發行標籤才會發布 GitHub Release。Pull Request 會略過套件製作，三個預設平台仍執行完整測試。
手動執行時也可勾選 **packages**，驗證完整套件。

手動執行時也可啟用額外的隨機測試與效能測試，版本發布不依賴這些選用測試的結果。

推送至 `main`、發行標籤與手動套件流程會產生並驗證以下發布檔案：

- **Windows x64 與 Arm64：** 只包含 `oxidemerge.exe` 的 ZIP 套件，並檢查執行檔的處理器架構與能否執行。
- **Linux x64 與 Arm64：** 在 Ubuntu 22.04 建置的 TAR.GZ 套件，會實際執行其中的程式，並確認不變更應用程式設定。
- **Linux AppImage：** 在使用 glibc 2.28 的 AlmaLinux 8 上建置。
  套件內含必要的 X11、Wayland 與鍵盤函式庫，系統缺少時才使用隨附版本。
  兩種架構都會在 Ubuntu 22.04 與 AlmaLinux 8 上檢查函式庫相依性、指令列行為與視窗啟動。
- **macOS：** 採臨時簽署的通用應用程式，提供 DMG 套件。
  會驗證 Apple 晶片與 Intel 兩種架構，並透過 Rosetta 測試 Intel 執行檔。
- **說明文件：** 英文與繁體中文 PDF 指南；若字型無法涵蓋所需字元，文件建置會失敗。

Windows 與 Linux 的 Arm64 套件會在原生 Arm 執行環境中建置與測試。

## 版本發布

### 發布條件

推送 `v0.9.6` 這類標籤時，會觸發 GitHub Releases 套件發布。
標籤必須與 `Cargo.toml` 的版本一致，且 `CHANGELOG.md` 必須包含對應版本的段落。
含有 `-` 的標籤，例如 `v1.0.0-rc1`，會發布為預先發行版本。

發行說明取自對應的變更紀錄段落，後面再附上下載說明。工作流程只會讀取變更紀錄，不會修改內容。
發布前會確認預期的套件均已產生、沒有其他多餘檔案，且該版本尚未發布。每個版本都會附上 PDF 指南，
並註明建置時使用的原始碼提交版本；GitHub 會列出每個檔案的 SHA-256 摘要。發行版本只包含以下檔案（以 0.9.6 為例）：

```text
oxidemerge-0.9.6-linux-arm64.AppImage
oxidemerge-0.9.6-linux-arm64.tar.gz
oxidemerge-0.9.6-linux-x64.AppImage
oxidemerge-0.9.6-linux-x64.tar.gz
oxidemerge-0.9.6-macos-universal.dmg
oxidemerge-0.9.6-user-guide.pdf
oxidemerge-0.9.6-user-guide.zh-TW.pdf
oxidemerge-0.9.6-windows-arm64.zip
oxidemerge-0.9.6-windows-x64.zip
```

### 發布步驟

以下以 `0.9.6` 為例，請替換為預計發布的版本號。

1. 更新 `Cargo.toml` 中的 `version`，執行 `cargo build` 以更新 `Cargo.lock`。
2. 檢查 `CHANGELOG.md` 中對應版本的段落，將標題中的 `Unreleased` 改為 `YYYY-MM-DD` 格式的發行日期。
3. 執行 `make fmt-check ci`，並以 `scripts/release-notes.sh 0.9.6` 預覽發行說明。
4. 提交並推送至 `main`，等待所有平台的必要 CI 檢查通過。
5. 為已驗證的提交加上標籤並推送：

   ```sh
   git tag -a v0.9.6 -m "OxideMerge 0.9.6"
   git push origin v0.9.6
   ```

6. 在 `CHANGELOG.md` 加入下一個版本的 `Unreleased` 段落，記錄後續變更。

請勿移動或重複使用已推送的發行標籤，確保已下載的套件持續符合公布的檢查碼。
若發行建置失敗，或已發布的版本需要修正，請先在 `main` 修正，再發布新的修補版本。
建置失敗的標籤也應保留；發布流程會等待所有必要的套件工作完成，因此不會發布不完整的版本。

### 發行失敗或已發布版本的修補流程

例如 `v0.9.5` 建置失敗，或發布後需要修正，請改以 `v0.9.6` 發布修補內容。
請使用尚未使用過的新修補版本號，保留原標籤與已發布的檔案。

1. 確認工作目錄沒有未提交的變更，再更新 `main`。若有本機修改，請先提交，
   或使用 `git stash` 暫存後再切換分支。

   ```sh
   git status --short
   git switch main
   git pull --ff-only origin main
   ```

2. 在 `main` 修正原始碼、套件建置或工作流程。將 `Cargo.toml` 的 `version` 更新為 `0.9.6`。
   在 `CHANGELOG.md` 新增或更新 `0.9.6` 段落，說明修正內容，並將 `Unreleased` 改為發行日期。
   保留 `0.9.5` 的歷史紀錄。

3. 更新鎖定檔、執行檢查，並確認新版本的發行說明：

   ```sh
   cargo build
   make fmt-check ci
   scripts/release-notes.sh 0.9.6
   git diff --check
   git diff
   ```

4. 將修正內容與版本檔案加入暫存區，檢查後提交並推送。
   `git add -p` 可逐項選取本次修補的變更；新增的檔案請另外以 `git add <path>` 加入。

   ```sh
   git add -p
   git add Cargo.toml Cargo.lock CHANGELOG.md
   git diff --cached
   git commit -m "Fix release issues and prepare 0.9.6"
   git push origin main
   git rev-parse HEAD
   ```

5. 等待上一步顯示的提交版本通過所有平台的必要 CI 檢查。
   加上標籤前，確認 `HEAD` 仍指向該已驗證的提交，不要將標籤加在較新、尚未通過檢查的提交上。

   ```sh
   git log -1 --oneline
   git tag -a v0.9.6 -m "OxideMerge 0.9.6"
   git push origin v0.9.6
   ```

6. 檢查新標籤的工作流程與 GitHub 發行頁面，確認預期的套件與 PDF 指南均已提供，
   且發行說明包含本次修正。接著在 `CHANGELOG.md` 加入下一個版本的 `Unreleased` 段落。

若新版本的發行建置仍失敗，請使用另一個尚未使用過的修補版本號，重複以上流程。
不要刪除或強制推送舊標籤，也不要替換舊版本已發布的檔案。

## 參與貢獻

歡迎透過 [GitHub Issues](https://github.com/kuopinghsu/OxideMerge/issues) 與 Pull Request
回報問題、提出功能建議、改善文件或協助翻譯。
修改程式碼時，請依照[開發說明](#開發)操作，並在提出 Pull Request 前執行相關檢查。

## 安全性

漏洞回報方式、不受信任輸入的處理原則與相依套件檢查，請參閱 [SECURITY.md](SECURITY.md)（英文）。

## 授權條款

OxideMerge 採用 [MIT 授權條款](LICENSE)。

## 選擇性審閱與分析

這些操作均為唯讀，需自行啟用。在三向文字或資料夾比較中展開「合併驗證」，可在記憶體中預覽合併，
或檢查已儲存結果的實際位元組。「文字／語意」檢視會比較 JSON 與 XML 路徑，不會編輯或重新
序列化原檔。重複的 JSON 鍵與 XML DTD／實體會被拒絕；YAML／TOML 請使用原有文字檢視。

資料夾比較新增「審閱工作區」，支援檔案／差異區塊狀態、註解、理由、標籤及下一個未審閱項目。
儲存為 `.oxmreview` 後可在其他電腦繼續。第 1 版套件仍可開啟；新第 2 版套件加入有大小限制的
審閱資訊及擷取內容的 SHA-256 摘要。更新來源會標記過期決定，不會自動接受已變更的區塊。

「比較基準／版本摘要」接受資料夾、固定 Git 提交及審閱快照的側別。內容完全相同且唯一對應的
移動會與新增／刪除分開列出。選擇性的「潛在影響」會以具體原始碼證據列出路徑／設定鍵名稱的
文字出現位置；這些參照均為啟發式判斷，不代表已驗證執行行為或建置／測試結果。
Git 來源與原有版本比較相同，會使用受信任的 Git 設定。

```sh
oxidemerge --merge-plan left.txt base.txt right.txt --report merge-plan.json
oxidemerge --validate-merge merged.txt left.txt base.txt right.txt --report validation.json
oxidemerge --semantic --format json old.json new.json --report semantic.html
oxidemerge --review-summary review.oxmreview --report review-summary.json
oxidemerge --baseline-left git:v1.0:/path/to/repo --baseline-right git:v1.1:/path/to/repo --release-report release.html
oxidemerge --impact old-folder new-folder --report impact.json
```

分析報告使用獨立、有版本編號的格式（JSON、HTML 或 XML），不變更原有報告格式。分析不會覆寫
輸入，也不允許將報告匯出至選取的輸入資料夾內。結束代碼維持 0／1／2（相同或成功／不同或警告／
錯誤）；`--validate-merge` 另以 **3** 表示驗證失敗。無法讀取或不支援的基準路徑會明確列出並傳回 2；依篩選規則排除的項目則列為範圍外。規則、限制與尚未支援的行為請參閱[分析指南](docs/analysis-guide.md)。

文字比較提供「緊密排列」（預設，不保留新增／刪除的空白）與「對齊差異」（留白以對齊相應行）。
可在「選項 → 一般」或「檢視 → 文字比較」變更「文字排列」。文字與二進位比較的位置窗格都在右側。

「關於」與 `--version` 會以 Git 短雜湊識別開發版本；已追蹤檔案或暫存區有變更時加上 `-dirty`。
對應 `v版本號` 標籤的乾淨最佳化建置則顯示 UTC 建置日期；仍可透過 `SOURCE_DATE_EPOCH` 固定日期以重現建置。
若無 Git 資訊，會使用有效的 `GITHUB_SHA`；兩者皆無時顯示 `unknown`。
