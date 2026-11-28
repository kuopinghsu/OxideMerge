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

請先透過 [rustup](https://rustup.rs/) 安裝 Rust 工具鏈。Linux 另需要 C 與 C++ 編譯器（`gcc` 與
`g++`，Fedora 系列為 `gcc-c++`）：讀取 RAR 壓縮檔的 UnRAR 程式庫以 C++ 撰寫。 X11、Wayland、鍵盤與
OpenGL 函式庫會在程式啟動時才載入，因此建置時不需要安裝這些函式庫的開發套件。

SFTP 功能會從原始碼建置 OpenSSL，因此需要 Perl：

- **Windows：** 安裝 [Strawberry Perl](https://strawberryperl.com/)，例如執行
  `winget install StrawberryPerl.StrawberryPerl`，然後開啟新的終端機。若要使用不在 `PATH` 中的
  Perl，請將 `OPENSSL_SRC_PERL` 設為其 `perl.exe`。Git for Windows 附帶的 Perl 無法使用。
- **Linux：** `perl` 及其 `IPC::Cmd` 模組（Fedora 為 `perl-IPC-Cmd`）。
- **macOS：** 系統內建的 Perl 即可。

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

[Makefile](../Makefile) 為常用工作提供簡短的指令，需要 GNU Make。執行 `make help` 可列出所有目標。

### 建置與執行

| 指令 | 用途 |
| --- | --- |
| `make` 或 `make release` | 快速建置最佳化的程式：`target/release/oxidemerge`。 |
| `make opt-size` | 建置最佳化檔案大小的程式，與 CI 打包的相同（`release-size` 設定檔：`opt-level = "s"`、fat LTO、單一 codegen unit），輸出到 `target/release-size/`。建置時間長得多。 |
| `make build` | 建置偵錯版本。 |
| `make run ARGS="a.txt b.txt"` | 以指定的路徑執行偵錯版本。 |
| `make demo`、`make demo-text`、`make demo-3way`、`make demo-3way-merge`、`make demo-3way-folder`、`make demo-conflict` | 開啟範例比較。 |
| `make demo-web`、`make demo-web-text`、`make demo-web-headings`、`make demo-web-tables` | 開啟網頁比較範例。後三者示範「格式化」模式。 |
| `make install`、`make uninstall` | 將程式安裝到 Cargo 的程式資料夾（預設為 `~/.cargo/bin`），或從中移除。 |
| `make shell-register` | 安裝程式，並註冊檔案管理員整合。 |
| `make clean` | 移除建置結果與本機產生的套件。 |

[samples/README.md](../samples/README.md) 列出每個範例及其示範的功能。網頁比較範例請參閱
[samples/web/README.md](../samples/web/README.md)。

### 檢查程式碼

| 指令 | 用途 |
| --- | --- |
| `make fmt-check` | 檢查 Rust 程式碼格式；`make fmt` 會自動修正格式。 |
| `make check` | 依目前的建置設定，對所有目標執行型別檢查。 |
| `make lint` | 執行 Clippy，並將警告視為錯誤。 |
| `make test` | 執行單元測試、介面測試與指令列測試。 |
| `make ci` | 依序執行 `check`、`lint` 與 `test`。 |
| `make gui-smoke` | 建置發行版本、開啟實際視窗，並檢查比較結果。 |
| `make docker-test` | 在 Docker 容器中執行 Linux 檢查，並使用實際的測試伺服器。請參閱 [Docker 測試環境](#docker-測試環境)。 |
| `make check-platforms` | 對 Makefile 列出的 Windows、macOS 與 Linux 目標執行型別檢查。請先以 `rustup target add` 安裝各目標的標準函式庫。 |

### 產生套件

| 指令 | 用途 |
| --- | --- |
| `make dist` | 以 `make opt-size` 建置，並在 `dist/` 產生套件：Windows 為 ZIP 檔，macOS 與 Linux 為 TAR.GZ 檔。 |
| `make appimage` | 依本機處理器架構（x86_64 或 aarch64）產生 Linux AppImage。Docker 可以使用時（Windows 與 macOS 一律如此，Linux 與 WSL 則在 `docker info` 成功時；若不成功，會顯示訊息並改在主機上建置），會如同 CI 在 glibc 2.28 的 AlmaLinux 8 容器中建置，因此 AppImage 可在 RHEL 8 上執行。`make appimage-native`（或 `APPIMAGE_DOCKER=0`）則直接在 Linux 主機上建置：需要 X11、Wayland 與鍵盤函式庫，且產生的 AppImage 需要與主機相同或更新的 glibc。兩者都會下載 `appimagetool`。 |
| `make user-guide` | 在 `dist/` 產生英文與繁體中文 PDF 指南。 |
| `make user-guide-pdf` | 在 `docs/dist/` 產生相同的指南，檔名為 `oxidemerge-user-guide.pdf` 與 `oxidemerge-user-guide.zh-TW.pdf`。這些檔案會提交到儲存庫，每次建置都會取代它們。 |

變更使用者指南或其圖片後，請執行 `make user-guide-pdf`，並將 PDF 檔案與變更一起提交，讓
`docs/dist/` 始終保有最新的指南。

在 Windows 與 macOS 上，這兩個目標都在 Docker 中執行：請啟動 Docker Desktop（在 Windows 上使用 Linux
容器模式），再用 GNU Make 執行任一目標。目標會建置可重複使用的映像檔，其中的 Asciidoctor 版本與 CI
相同，並將 PDF 寫回儲存庫。

在 Linux 上，這兩個目標會直接執行 `scripts/build-user-guide.sh`。在 Linux 或 macOS 上直接執行
此指令碼時，需要 Asciidoctor PDF、`curl`、`shasum`、含 `fontTools` 的 Python 3、fontconfig 與
DejaVu Sans 字型。指令碼會自動下載所需的 Noto Sans CJK 字型。使用 Docker 的目標會在容器內提供
這些相依套件。

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
（結果依讀取或寫入而不同），無法依要求使用的索引鍵或表格則使用 `Problem::of_table`。需要新的對應時，
請在這些函式旁新增對應函式，而不是新增列舉。

這些名稱會以兩種方式呈現給使用者。`--sync-apply` 與 `--sync-recover` 會在 JSON 中以 `problem`
輸出結果，使用上表中的序列化名稱，供指令稿判斷；這屬於相容性介面，變更列舉名稱時必須在發行說明中記載
。`Problem::label()` 提供已翻譯的簡短名稱（`lang/*.json` 中的 `problem.*`），
供會先標示問題類型再說明細節的檢視使用。細節仍沿用各功能自己的用語：分類說明的是發生了哪一類情況，
而不是發生了什麼事。

## 測試

`cargo test` 涵蓋三個部分：

- **核心功能：** 比較、合併、修補檔、資料夾與檔案操作。 `src/core/regress.rs`
  中的隨機測試也會產生差異、合併與修補案例進行驗證。
- **介面操作：** 在沒有顯示器的情況下操作實際的檢視畫面，涵蓋選單、編輯、拖放檔案與儲存結果。
  視窗測試失敗時，會將詳細資訊寫入 `target/gui-tests/<test>.txt`。
- **指令列行為：** `tests/cli.rs` 中的測試會執行程式，檢查結束代碼、報告、修補檔與錯誤訊息。

測試使用獨立的設定資料夾，不會變更您的設定。其他不應影響設定的測試，也可以將 `OXIDEMERGE_CONFIG_DIR`
指向另一個資料夾。部分網頁測試會啟動本機 HTTP 伺服器，因此測試環境必須允許程式監聽本機網路連接埠。

版本控制的測試會使用實際的程式：Git、Subversion 的 `svn` 與 `svnadmin`，以及 Mercurial 的 `hg`。
找不到 Subversion 或 Mercurial 時，相關測試會印出已略過並視為通過，因此請先安裝它們再執行這些測試。
持續整合工作流程不會安裝 Subversion 或 Mercurial，只有執行環境本身已有時才會執行；
[Docker 測試環境](#docker-測試環境)則兩者都會安裝。Perforce 沒有自動化測試。

### 已保存的報告

以固定輸入產生的語意、版本摘要與影響報告，必須與 `tests/fixtures/reports` 中的 JSON 與 XML
報告相同，只有擷取時間可以不同。若您有意變更報告內容，請執行下列指令更新已保存的報告，
再檢查其中的變更：

```sh
OXIDEMERGE_UPDATE_REPORTS=1 cargo test --test cli reports_match
```

### 格式檢查

下列指令只執行格式檢查：

```sh
cargo test --release --locked format_regression -- --nocapture
```

這些檢查確認修補檔會保留位元組順序標記與換行字元、已忽略的文字變更維持忽略，以及在混用換行字元時，
合併、儲存與復原都能正確運作。它們也會確認結果與 Git 簽出及 Git 修補檔相容，因此必須安裝 Git。
持續整合會在 Windows、Linux 與 macOS 上，於完整測試中執行這些檢查。

### 翻譯檢查

若 `lang/` 中的語言檔缺少 `lang/en.json` 的鍵、含有英文沒有的鍵、同一個鍵出現兩次、少了或多了
`{placeholder}`、提到的按鍵與英文不同（`Ctrl+V`、`F5`；德文的 *Strg* 視為 *Ctrl*），或將刪除、
取代或遺失工作的警告留為英文，`i18n` 測試就會讓建置失敗。這些測試也會確認
`docs/user-guide.zh-TW.adoc`、`docs/development.zh-TW.md` 與 `README_zh-TW.md` 的標題、
層級與錨點都與英文文件相同，且使用者指南中的每個 `<<anchor>>` 都存在。另有一個視窗測試會以每種語言，
在 800 × 600 的視窗中開啟復原提示與「選項」對話方塊，若有按鈕超出視窗或與其他按鈕重疊就會失敗。

```sh
cargo test --locked i18n::
```

這些測試無法判斷譯文是否自然，仍需由母語人士審閱。

### 隨機測試

隨機測試失敗時會輸出一個種子值。可用此種子值重新執行相同案例，或執行更多案例：

```sh
OXIDEMERGE_TEST_SEED=<seed> cargo test regress
OXIDEMERGE_TEST_CASES=20000 cargo test --release regress
```

### 視窗測試

若要以實際的程式視窗進行測試，請執行 `make gui-smoke` 或 `scripts/gui-smoke.sh path/to/oxidemerge`。
指令碼會檢查雙檔、三檔與資料夾比較。 Linux 沒有顯示環境時，會自動啟動 Xvfb。設定
`OXIDEMERGE_SMOKE_REPORT=<file>` 後，程式會在比較完成時，將分頁與訊息寫入該檔案，然後關閉。

在 Linux 與 macOS 上，指令碼也會以實際的程序檢查當機復原：先開啟復原功能啟動程式，
等第一份快照寫入後以 `kill -9` 結束程式，再確認下次啟動時會提供兩個檔案的復原，
且在使用者選擇之前保留快照。

若要製作文件圖片，設定 `OXIDEMERGE_SCREENSHOT=<file.png>` 後，程式會在比較完成時，
直接儲存它所繪製的視窗畫面（不需擷取螢幕），然後關閉。將 `OXIDEMERGE_SCREENSHOT_COMMAND`
設為命令的標籤鍵（例如 `cmd.sync_folders`），會先執行該命令。搭配 `OXIDEMERGE_CONFIG_DIR` 與設定
`language` 的 `config.json`，即可擷取其他語言的畫面。

### Docker 測試環境

在任何裝有 Docker 的電腦上（包括 Mac），都可以執行 `scripts/docker-test.sh`（或 `make docker-test`）
來檢查 Linux。容器定義於 [docker/Dockerfile](../docker/Dockerfile)，會執行 Clippy、完整測試、
發行版本建置，以及在 Xvfb 上的指令列與視窗檢查；測試與持續整合相同，以發行模式建置。
測試以一般使用者身分執行。容器也會啟動實際的伺服器（[docker/servers.sh](../docker/servers.sh)），
並安裝 Subversion 與 Mercurial，執行一般 `cargo test` 會略過的測試：

- OpenSSH：以金鑰登入、金鑰密碼、主機憑證、跳板主機與 `ProxyCommand`，以及透過 PAM
  的鍵盤互動式登入。
- vsftpd 與 ProFTPD：FTP、明確與隱含 FTPS、主動模式，以及用戶端憑證。
- Dante 與 tinyproxy：透過 SOCKS5 與 HTTP CONNECT 代理伺服器（需登入與不需登入）使用 SFTP、FTP 與
  FTPS。
- Subversion 與 Mercurial：以實際的工作複本與修訂版本比較。

Cargo 以離線方式執行，使用已下載到 `~/.cargo/registry` 的套件；在新電腦上，請先執行
`cargo fetch --locked`。第一次建置容器時需要網路。建置結果保存在 Docker volume
（`oxidemerge-test-*`）中，之後的執行會比較快。每項檢查的記錄寫在 `out/docker/linux`。若 Docker
可用的記憶體不多，請將 `CARGO_BUILD_JOBS` 設為較小的數字。

### 效能測試

效能測試不包含在 `cargo test` 中。指令、量測方法與參考結果，請參閱 [效能測試說明](benchmarks.md)（英
文）。

## 持續整合

[持續整合工作流程](../.github/workflows/ci.yml)不會在每次推送或合併請求時執行。它每天檢查一次
`main`，並在推送發行標籤與手動執行時執行所有檢查。

| 觸發時機 | 執行內容 |
| --- | --- |
| 每天在 `main` 上，台北時間 17:00（UTC 09:00） | 執行自上次通過的每日執行以來的變更可能影響的檢查（請參閱下一個表格），以及相依套件檢查。 |
| 推送發行標籤 | 在 Windows x64 與 Arm64、Linux x64 與 Arm64，以及 macOS 上執行 Clippy、完整測試、建置發行版本，並檢查指令列與視窗行為，同時產生及驗證所有套件，完成後發布版本。 |
| 手動執行 | 在您選擇的分支上執行與發行標籤相同的檢查。開啟 `packages`（預設開啟）時也會產生並驗證所有套件，但不會發布；開啟 `stress_cases` 或 `benchmarks` 可執行更多隨機測試案例或效能測試。 |

合併請求不會自動檢查。若要檢查某個分支，請從 Actions 分頁手動執行，或使用 GitHub 命令列工具：

```sh
gh workflow run ci.yml --ref <branch>
```

每日執行會將 `main` 與上次通過的每日執行所用的提交比較。某天失敗時，隔天會連同之後的提交再檢查一次。
`changes` 工作會依變更的檔案決定執行內容：

| 變更的檔案 | 執行內容 |
| --- | --- |
| 程式碼：`src/`、`tests/`、`lang/`、`samples/`、`assets/`、`images/`、`build.rs`、`build_info.rs`、`Cargo.toml`、`Cargo.lock`、`scripts/gui-smoke.sh`，以及最後一列未列出的任何檔案 | 在 Linux x64 上執行 Clippy、完整測試、建置發行版本，並檢查指令列與視窗行為。 |
| 只有 `README.md`、`README_zh-TW.md`、`docs/user-guide*.adoc` 或 `docs/development*.md`，沒有程式碼 | 只執行讀取這些檔案的兩項測試。 |
| `.github/workflows/ci.yml` | 以上兩者。 |
| `docs/` 中的其他檔案（例如 PDF）、`CHANGELOG.md`、`SECURITY.md`、`LICENSE`、`Makefile`、`docker/`、`scripts/` 與 `.github/` 中的其他檔案，以及 `samples/` 與 `tests/fixtures/` 中的 README | 除相依套件檢查外不執行任何檢查。 |

沒有新的提交時，只執行相依套件檢查。無法列出變更的檔案時（尚無通過的每日執行、強制推送移除了該次執行的提交，
或變更了 300 個以上的檔案），會執行每日執行的所有檢查。

其他說明：

- Windows 與 Linux 的視窗檢查使用 Mesa 軟體繪圖，Linux 另使用 Xvfb。
- 測試失敗時，輸出內容會保留在工作流程執行結果的 `test-output-*` 檔案中。
- 另有一項 `cargo deny` 工作會檢查相依套件。發現新的安全公告時，手動執行只會發出警告，發行標籤與每日執行則會失敗。
  請參閱 [SECURITY.md](../SECURITY.md)（英文）。
- 額外的隨機測試與效能測試為選用項目，版本發布不受其結果影響。

### 套件

推送發行標籤，以及開啟 `packages` 的手動執行，都會產生並驗證下列套件：

- **Windows x64 與 Arm64：** 只包含 `oxidemerge.exe` 的 ZIP 套件。會檢查每個程式的處理器架構，
  並實際啟動。
- **Linux x64 與 Arm64：** 在 Ubuntu 22.04 上建置的 TAR.GZ 套件。會實際啟動程式，
  並確認不會變更設定。
- **Linux AppImage：** 在使用 glibc 2.28 的 AlmaLinux 8 上建置。套件內含 X11、Wayland 與鍵盤函式庫，
  系統缺少這些函式庫時才會使用。兩種處理器架構都會在 Ubuntu 22.04、AlmaLinux 8 與 AlmaLinux 8.4 上，
  檢查函式庫載入、指令列行為與視窗啟動。AlmaLinux 8.4 取自其封存庫，內含 Mesa 20.3，
  因此在其上的視窗啟動會以軟體繪製器檢查下方的 glutin 修補；若其 Mesa 為 21 或更新版本，
  該工作會失敗。每個 AppImage 都內嵌指向 GitHub 發行版本的更新資訊，其 `.AppImage.zsync`
  檔案也會一併發布，讓 AppImageUpdate 可以更新它；測試會檢查這兩者。正式版本追蹤 `latest`，
  即 GitHub 標示為最新的版本；發行工作只會將版本號最高的正式版本標示為最新（`scripts/release-latest.sh`，
  其案例由 `scripts/release-latest.sh --test` 檢查），因此舊版本線的修補版不會讓使用者退回。
  發布後會再次檢查此標示；若同時執行的另一個發行工作取得了它，會將它移回版本號最高的正式版本。版本號含 `-` 的預覽版本或未加標籤的建置則追蹤 `latest-all`，即最近發布的版本，
  不論正式或預覽：會更新到先發布的下一個預覽版本或正式版本，但在其後發布的舊版本線修補版也會成為更新目標。
  `scripts/package-appimage.sh` 使用 `GITHUB_REPOSITORY` 所指的儲存庫（CI 之外為
  `kuopinghsu/OxideMerge`），`UPDATE_INFORMATION` 可取代更新資訊，設為空字串則不內嵌。
- **macOS：** 以 DMG 套件提供的通用應用程式，由 `scripts/macos-sign.sh` 簽署：
  沒有簽署機密時採用臨時簽署；五個 `MACOS_*` 機密都設定時，則以 Developer ID 簽署、
  公證並裝訂（請參閱 [macOS 簽署](macos-signing.md)）。會驗證 Apple 晶片與 Intel 兩種程式碼，並透過
  Rosetta 測試 Intel 程式。接著 `scripts/macos-install-check.sh` 會像下載的檔案一樣，從 DMG
  安裝應用程式（安裝到獨立的「應用程式」資料夾並加上隔離標記），檢查 Gatekeeper
  的判定（臨時簽署時應被拒絕，Developer ID 簽署時應被接受為已公證），依 README 的說明清除標記，
  執行程式後再移除，並確認沒有留下任何檔案。若該 Mac 已關閉 Gatekeeper，只會發出警告；在 CI
  中會先開啟 Gatekeeper，無法開啟時即失敗（`INSTALL_CHECK_GATEKEEPER=require`）。
- **說明文件：** 英文與繁體中文 PDF 指南。若字型缺少所需字元，文件建置會失敗。

Windows 與 Linux 的 Arm64 套件會在實體 Arm 架構電腦上建置與測試，而非使用模擬環境。

發布前，發行工作會以 `scripts/check-release.sh` 檢查下載的每個套件：壓縮檔必須能列出內容並解壓縮，
AppImage 與 DMG 必須帶有其格式標記，每個 `.zsync` 檔案必須指向旁邊的 AppImage 並列出其大小與
SHA-1，PDF 必須完整。之後會將 `SHA256SUMS` 與套件一併發布。

### 為 Mesa 21 之前的版本修補 glutin

在 Mesa 早於 21 的系統（RHEL 8.4）上使用軟體繪製器時，glutin 呼叫的 `glXSwapIntervalEXT`
會執行到繪製器沒有的函式，視窗在開啟時當掉。截至 2026 年 11 月，glutin 的 `master` 分支仍有此問題。
本儲存庫不保留 glutin 的複本：AppImage 工作會在建置前執行 `scripts/patch-glutin.sh`，由它將
`scripts/patches/glutin-<version>-mesa-swap-interval.patch` 套用到 Cargo 已下載的 glutin。

- 此修補變更 `src/api/glx/surface.rs` 中的 `set_swap_interval`。列出 `GLX_MESA_swap_control`
  時（只有 Mesa 會列出），glutin 只使用 `glXSwapIntervalMESA`。驅動程式無法設定間隔時，Mesa
  會從中傳回錯誤；eframe 會記錄一則警告，視窗照常開啟，只是沒有垂直同步。其他驅動程式（例如 NVIDIA）
  仍使用 `glXSwapIntervalEXT`。
- 指令稿修補的是 `$CARGO_HOME/registry/src`，因此 `Cargo.toml` 與 `Cargo.lock` 維持提交時的內容，
  加上標籤的建置仍會得到發行版本標記。使用相同 `CARGO_HOME` 建置的所有專案都共用這份複本：只在 CI
  與擁有獨立 registry 磁碟區的 AppImage 容器中執行此指令稿。 `make appimage` 在 Docker 中建置時，
  會在該容器中執行它。
- 下載的 crate 檔案變更後，Cargo 不會重新建置它，因此指令稿也會刪除 glutin 在 `PROFILE` （預設為
  `release-size`）下的建置產物。再次執行不會有其他作用。
- 無法設定交換間隔時，視窗就沒有垂直同步。以軟體繪製 OpenGL 時（`GL_RENDERER` 為 llvmpipe、softpipe
  或 swrast）， OxideMerge 會讓每個畫面至少間隔一個 60 Hz 畫面，非由輸入引發的畫面（例如轉圈圖示）則至少
  間隔一個 30 Hz 畫面，因此動畫不會以處理器的最大速度繪製。此時它也會關閉動畫，效果如同「減少動態效果」選項，但不會變更該選項：每個畫面都是由處理器
  繪製整個視窗，經由遠端桌面時還要透過網路傳送，因此跳到遠處的差異時捲動的十幾個畫面可能要花上數秒。
- 在透過網路傳送的 X11 顯示上（如 `ssh -X` 與 `ssh -Y` 所設定：`DISPLAY` 指名主機，例如
  `localhost:10.0`，且未設定 `WAYLAND_DISPLAY`；在會移除 `SSH_CONNECTION` 的 `sudo` 下也是如此；在 WSL
  中則只限於 SSH 工作階段，因為 VcXsrv 等 Windows 上的 X 伺服器是以位址指名），畫面之間至少間隔十分之一
  秒，且動畫關閉。非由輸入引發的畫面（例如轉圈圖示或檢查進行中的工作）至少間隔四分之一秒；回應輸入的畫面只與上一個回應輸入的畫面保持間隔，因此在轉圈圖示的畫面等待時按下的鍵，會在該畫面之後立即繪製。每個畫面都是
  整個視窗，以軟體繪製 OpenGL 時就是數 MB 的像素，因此按鍵的按下、放開與修飾鍵的變更會合併在同一個畫面
  中。`ssh -C`（或 `Compression yes`）幫助很大：畫面大多是單色區塊，壓縮效果很好。在使用者自己的電腦上以 OxideMerge 透過 SFTP 比較檔案，或使用 NoMachine、X2Go、VNC 等遠端桌面，
  會快得多。
- TAR.GZ 套件不會修補。它們需要比 RHEL 8 更新的 glibc，本來就無法在其上執行。直接以 `cargo build`
  建置的程式也不會修補。

`Cargo.lock` 改用其他 glutin 版本時，指令稿會以「No patch for glutin <version>」停止；
修補無法套用時，則以「does not apply」停止。為新版本製作修補檔：

1. 在儲存庫外解開該版本，並保留一份檔案複本：

   ```sh
   cargo fetch --locked
   mkdir -p /tmp/glutin && cd /tmp/glutin
   cp -r ~/.cargo/registry/src/index.crates.io-*/glutin-<version> a
   cp -r a b
   ```

2. 在 `b/src/api/glx/surface.rs` 中做上述變更：在 `set_swap_interval` 裡，將下列區塊放在
   `GLX_EXT_swap_control` 區塊之前，並刪除其後原有的 `GLX_MESA_swap_control` 區塊：

   ```rust
   if self.display.inner.client_extensions.contains("GLX_MESA_swap_control") {
       unsafe {
           applied = extra.SwapIntervalMESA(interval as _) != glx::BAD_CONTEXT as _;
       }
       return if applied { Ok(()) } else { Err(ErrorKind::BadContext.into()) };
   }
   ```

3. 以從 crate 開始的路徑寫出修補檔，並刪除舊的修補檔：

   ```sh
   diff -u a/src/api/glx/surface.rs b/src/api/glx/surface.rs |
       sed '1,2s/\t.*//' > <repository>/scripts/patches/glutin-<version>-mesa-swap-interval.patch
   ```

4. 在 Mesa 20.3 或 RHEL 8.4 本身上，強制使用軟體繪製器檢查視窗。未修補時，
   視窗開啟時會發生記憶體區段錯誤；修補後可正常開啟：

   ```sh
   WAYLAND_DISPLAY= LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe ./oxidemerge-*.AppImage
   ```

若 glutin 本身修正了此問題，請刪除修補檔、指令稿，以及 `.github/workflows/ci.yml` 與
`docker/appimage.Dockerfile` 中執行它的步驟。

## 版本發布

### 發布規則

- 推送 `v0.9.11` 這類標籤時，會將套件發布到 GitHub 的發行頁面。
- 標籤必須與 `Cargo.toml` 中的版本一致。
- `CHANGELOG.md` 必須有該版本的段落。
- 含有 `-` 的標籤（例如 `v1.0.0-rc1`）會發布為預先發行版本。

發行說明取自變更紀錄中對應的段落，後面附上下載說明。工作流程只會讀取變更紀錄，不會修改它。

發布前，工作流程會確認該版本尚未發布，並確認所有預期的套件都已產生，且沒有多餘的檔案。
每個版本都會註明建置時使用的原始碼提交，GitHub 也會列出每個檔案的 SHA-256 檢查碼。以 0.9.11 版為例，
發行版本只包含下列檔案：

```text
oxidemerge-0.9.11-linux-arm64.AppImage
oxidemerge-0.9.11-linux-arm64.AppImage.zsync
oxidemerge-0.9.11-linux-arm64.tar.gz
oxidemerge-0.9.11-linux-x64.AppImage
oxidemerge-0.9.11-linux-x64.AppImage.zsync
oxidemerge-0.9.11-linux-x64.tar.gz
oxidemerge-0.9.11-macos-universal.dmg
oxidemerge-0.9.11-user-guide.pdf
oxidemerge-0.9.11-user-guide.zh-TW.pdf
oxidemerge-0.9.11-windows-arm64.zip
oxidemerge-0.9.11-windows-x64.zip
```

### 發布新版本

以下以 `0.9.11` 版為例，請改為您要發布的版本號。

1. 修改 `Cargo.toml` 中的 `version`，再執行 `cargo build` 更新 `Cargo.lock`。
2. 檢查 `CHANGELOG.md` 中 `0.9.11` 的段落，將標題中的 `Unreleased` 改為 `YYYY-MM-DD`
   格式的發行日期。
3. 執行 `make fmt-check ci`，並以 `scripts/release-notes.sh 0.9.11` 檢查發行說明。執行
   `make user-guide-pdf`，讓 `docs/dist/` 中的指南顯示新版本。
4. 提交並推送到 `main`。在 `main` 上手動執行持續整合工作流程（會檢查所有平台與套件），並等待它通過：

   ```sh
   gh workflow run ci.yml --ref main
   ```

5. 為通過檢查的提交加上標籤，並推送標籤。加上標籤前，請先以 `git log -1 --oneline` 確認 `HEAD`
   就是通過檢查的提交。

   ```sh
   git tag -a v0.9.11 -m "OxideMerge 0.9.11"
   git push origin v0.9.11
   ```

6. 工作流程完成後，檢查 GitHub 上的新發行版本，確認所有套件與 PDF 指南都已提供。
7. 在 `CHANGELOG.md` 中為下一個版本加入 `Unreleased` 段落，之後的變更都記錄在這裡。

### 修正建置失敗的版本

工作流程只會在所有建置與測試工作都通過後才發布版本。因此，若發行標籤的其中一項建置或測試工作失敗，
就不會發布任何內容，您可以從修正後的提交，重新發布同一個版本。以下以 `v0.9.11` 為例，
請改為建置失敗的版本號。

1. 確認 GitHub 上沒有 `v0.9.11` 的發行版本：

   ```sh
   gh release view v0.9.11
   ```

   此指令需要已登入的 GitHub 命令列工具，且必須回覆 `release not found`。
   您也可以到[發行頁面](https://github.com/kuopinghsu/OxideMerge/releases)確認。若該版本已存在，
   即使只有部分檔案，也請勿移動標籤，改依 [修正已發布的版本](#修正已發布的版本)處理。

2. 在 `main` 上修正問題。`Cargo.toml` 與變更紀錄中的版本維持 `0.9.11`；若發行日期改變，
   請更新變更紀錄標題中的日期。執行 `make fmt-check ci`，並以 `scripts/release-notes.sh 0.9.11`
   檢查發行說明，再提交並推送到 `main`。在 `main` 上手動執行持續整合工作流程（`gh workflow run ci.yml --ref main`），
   並等待它通過。

3. 將標籤移到修正後的提交，再重新推送：

   ```sh
   git switch main
   git pull --ff-only origin main
   git log -1 --oneline
   git tag -a -f v0.9.11 -m "OxideMerge 0.9.11"
   git push --force origin v0.9.11
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
2. 以下一個修補版本號（例如 `0.9.11` 之後使用 `0.9.12`）依照[發布新版本](#發布新版本)的步驟操作，
   並在新版本的變更紀錄段落中說明修正內容。

請保留舊標籤與舊版本已發布的檔案。
