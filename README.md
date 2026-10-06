# EfuSeek

[![CI](https://github.com/100pangci/EfuSeek/actions/workflows/ci.yml/badge.svg)](https://github.com/100pangci/EfuSeek/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/100pangci/EfuSeek)](https://github.com/100pangci/EfuSeek/releases)

读取 Everything `.efu` 文件列表的 Linux 原生桌面搜索工具，使用 Rust、GTK4、libadwaita、rusqlite、SQLite FTS5 trigram、csv 和 serde。

**EfuSeek 不负责扫描磁盘或 NAS。**

```text
Everything / NAS 定时任务
        ↓ 生成 .efu
SMB / NFS / Syncthing
        ↓
EfuSeek → 本地 SQLite 高速搜索
        ↓ 打开时才访问真实目标
系统默认应用 / 文件管理器
```

适合家庭服务器和 NAS：不遍历远程目录；搜索不会产生大量 SMB I/O；NAS 离线时仍可查看最后一次索引。后台仅检查 EFU 文件本身的 mtime 和大小。

## 运行

当前版本：**v0.1.0**。窗口标题从 `Cargo.toml` 自动读取版本，显示为 `EfuSeek v0.1.0`。版本变更记录见 [CHANGELOG.md](CHANGELOG.md)。

需要 Rust/Cargo、C 编译器、pkg-config、GTK ≥ 4.12、libadwaita ≥ 1.4。

Fedora：

```bash
sudo dnf install gcc pkgconf-pkg-config gtk4-devel libadwaita-devel
cargo run
# 日常使用建议：
cargo run --release
```

SQLite 使用 bundled 版本，确保包含 FTS5 trigram；依赖版本锁定在 `Cargo.lock`。

首次运行不连接任何预设 NAS，也不加载任何个人路径。点击右上角齿轮，在**设置**中选择 `.efu`，并按需添加路径映射。首次构建在后台进行；后续启动先加载本地缓存，再后台检查来源。

### 下载预编译包

从 [GitHub Releases](https://github.com/100pangci/EfuSeek/releases) 下载 `efuseek-v<VERSION>-linux-x86_64.tar.gz` 和 `SHA256SUMS`。在下载目录校验、解压，执行解压目录下的 `bin/efuseek`：

```bash
sha256sum -c SHA256SUMS
tar -xzf efuseek-v0.1.0-linux-x86_64.tar.gz
./efuseek-v0.1.0-linux-x86_64/bin/efuseek
```

发布包在 Ubuntu 24.04 上构建，适用于兼容的 Linux x86_64 系统（glibc ≥ 2.39、GTK ≥ 4.12、libadwaita ≥ 1.4，以及对应 GLib 运行库）。**不是独立 AppImage**，依赖系统运行库；Fedora 可安装 `gtk4` 和 `libadwaita`，旧发行版请从源码编译。包内包含许可证、依赖 notices、配置示例和 `.desktop` 文件，不含私人配置或 EFU。

可选用户级安装（在解压目录执行）：

```bash
install -Dm755 bin/efuseek "$HOME/.local/bin/efuseek"
install -Dm644 share/applications/io.github.efuseek.EfuSeek.desktop \
  "$HOME/.local/share/applications/io.github.efuseek.EfuSeek.desktop"
```

桌面启动器要求 `~/.local/bin` 在桌面会话的 `PATH` 中；否则直接运行二进制。

## 配置

配置文件：`$XDG_CONFIG_HOME/efuseek/config.toml`，未设置 XDG 时使用 `~/.config/efuseek/config.toml`。

- `config.default.toml`：唯一的内置默认值来源，首次运行原样生成；无个人路径或映射。
- `config.example.toml`：公开的通用配置示例，路径需按自己的机器修改。
- 用户配置在 XDG 目录，不在项目内；不要把个人配置提交到 GitHub。

```toml
efu_path = "" # 空值表示尚未选择；也可填写 /mnt/nas/index/files.efu
path_map = []
cache_dir = "" # 空值使用 XDG 缓存目录
result_limit = 500
poll_seconds = 5
debounce_ms = 75
window_width = 1100
window_height = 720
browse_page_size = 512
browse_cache_pages = 8
```

| 配置项 | 含义 |
| --- | --- |
| `efu_path` | EFU 的本机绝对路径；空值不会建立索引 |
| `path_map` | 路径前缀映射列表 |
| `cache_dir` | 缓存目录绝对路径；空值使用 `$XDG_CACHE_HOME/efuseek` / `~/.cache/efuseek` |
| `result_limit` | 非空搜索结果上限，1–5000 |
| `poll_seconds` | EFU 属性检查间隔，1–86400 秒 |
| `debounce_ms` | 搜索防抖，1–2000 毫秒 |
| `window_width` / `window_height` | 初始窗口尺寸，单位为 GTK 逻辑像素 |
| `browse_page_size` | 全量浏览后台每页读取数，1–1024 |
| `browse_cache_pages` | 全量浏览缓存页数，1–64 |

缺少新配置项时自动使用内置默认值，旧配置无需手工迁移。内部协议标识、数据库 schema 和安全边界属于程序实现，不是个人配置。

### 路径映射

如果索引中的 `Z:\Anime\Test.mkv` 实际对应 `/mnt/nas/Anime/Test.mkv`，删除 `path_map = []`，添加：

```toml
[[path_map]]
from = 'Z:\'
to = '/mnt/nas/'

[[path_map]]
from = '\\NAS\share\'
to = '/mnt/nas/share/'
```

TOML 单引号字符串不用转义反斜线。映射按最长且有目录边界的前缀匹配；Windows / UNC 前缀 ASCII 大小写不敏感。Linux 路径保留合法的字面反斜线。映射后缀拒绝 `..`，目标必须是本机绝对路径。

设置页支持新增、编辑和删除映射，保存后立即用于显示和打开，不重建索引。路径列显示映射后的本机路径；未映射时保留原路径并标注“未映射”。数据库及文本搜索保留 EFU 原始路径，搜索本机映射路径暂未实现。

### 保存、缓存与回滚

设置页保存会保留其他配置项和权限，并先创建 `config.toml.bak-<时间戳>`。TOML 注释会在保存时重新序列化。回滚时关闭应用，将备份复制回配置文件。

设置页修改 EFU / 映射立即生效；手工编辑其他配置后需重启。缓存按 EFU 来源路径分开保存为 `index-*.sqlite`，切换回旧来源可复用。早期版本的 `index.sqlite` 在元数据匹配时自动迁移，不访问 EFU 来确认，也不无条件重建。

## 操作

| 操作 | 功能 |
| --- | --- |
| 空搜索 | 默认按 EFU 原始顺序浏览完整索引，按需分页加载；列头排序同样适用 |
| 输入普通文本 | 任意子串搜索，默认最多 500 条 |
| 点击名称 / 大小 / 修改时间列头 | SQLite 升序排序，再次点击切换降序 |
| 右键结果行 | 先选中该行，再打开原生菜单：打开、打开所在目录、复制文件名、本机完整路径、EFU 原始路径 |
| Ctrl+L / Ctrl+F | 聚焦搜索框 |
| ↑ / ↓ | 选择结果 |
| Enter / 双击 | 打开所选文件或目录 |
| Ctrl+Enter | 打开所在目录 |
| Ctrl+Home / Ctrl+End | 跳至首条 / 末条 |
| Esc | 清空并返回搜索框 |

输入法先处理候选确认、候选方向键和取消键；搜索框通过 GTK 原生激活信号打开结果，避免抢占日语输入法的 Enter。

名称完全匹配、名称开头、名称包含、路径包含依次排序。`erasmus` 可匹配 `Kiriue_no_Erasmus`、`erasmus.exe` 或目录路径里的该片段。百分号、下划线、引号按字面字符处理。目录显示 📁，文件显示 📄，目录不显示大小。

### 查询语法（下一版增量功能，尚未发布）

| 查询 | 语义 |
| --- | --- |
| `ext:mkv` / `ext:.mkv` | 仅文件，扩展名完全匹配，忽略开头的点 |
| `file:test` | 仅文件，名称包含 `test` |
| `folder:anime` | 仅目录，名称包含 `anime` |
| `path:galgame` | EFU 原始完整路径包含 `galgame`（不是本机映射路径） |

可组合，例如 `erasmus ext:zip`、`file:patch path:Galgame`、`folder:Anime`；各条件为 AND，忽略大小写，支持中文和日文。`file:` / `folder:` 可仅限定类型。未识别的 `foo:bar` 当普通文本处理。普通多词搜索仍是一段字面子串；与过滤器组合时，普通文本片段以空格连接。当前不支持引号分组、正则或完整 Everything 语言；引号、`%`、`_`、`\` 均按字面字符处理。所有值通过 SQL 参数绑定传递，长子串保留 FTS5 trigram 加速，1–2 字符条件回退 LIKE。

列表为“名称 | 大小 | 修改时间 | 本机路径”。Date Modified 按 Everything EFU 的 **Windows FILETIME**（从 1601-01-01 UTC 起的 100 纳秒计数）解析，按本机时区显示；缺失或无效时留空。列头排序显式覆盖搜索相关度排序；未点击排序时保留原有相关度和 EFU 浏览顺序。未知大小、目录大小、无效时间在升降序中均排在最后，以 id 作最终稳定 tie-breaker。

排序完全在后台 SQLite 中执行，空搜索仍只请求有界分页，GTK 不安装全量内存排序模型。新缓存具有排序索引；旧 v0.1.0 缓存离线仍能搜索/排序（排序可能较慢），来源恢复并稳定后自动原子重建升级。右键“打开所在目录”对文件夹打开其父目录。复制使用 GTK 剪贴板，不调用 shell；未映射的 Windows / UNC 路径仍可复制原始路径，本机路径复制项禁用并标注未配置映射。

## 实现

```text
src/
├── main.rs / app.rs        原生应用入口
├── config.rs              TOML 默认值、XDG、保存与备份
├── core/
│   ├── entry.rs            元数据、路径拆分、大小和 FILETIME 显示
│   ├── efu.rs              流式 UTF-8 CSV 解析
│   ├── index.rs            SQLite / FTS5、原子缓存替换、分页
│   ├── query.rs            Query Parser 和字面查询转义
│   ├── sort.rs             SQLite 排序字段和方向
│   ├── path_map.rs         独立路径映射
│   ├── opener.rs           GIO 异步检查与默认应用启动
│   └── worker.rs           索引、搜索与分页后台线程
└── ui/
    ├── window.rs           SearchEntry / ColumnView
    ├── virtual_list.rs     全量虚拟列表，有限页缓存
    ├── result_row.rs       可回收行工厂
    ├── settings.rs         EFU 和映射设置
    └── keybindings.rs      输入法优先的快捷键策略
```

Core 解析、搜索及映射不依赖 GTK；可运行 `cargo test --no-default-features`。GUI 不为全部条目创建对象，不执行耗时 SQL 或解析 CSV；文件访问通过 GIO 异步进行，不拼接 shell 命令。

后台在缓存目录创建新数据库，提交并校验后原子 rename；读取前后 EFU 属性变化则拒绝替换。更新失败保留缓存和当前结果。旧读取连接保留旧 inode，更新完成后打开新连接。搜索采用请求代数和 SQLite progress handler 取消过期查询。

发现 EFU 的 mtime / size 与缓存不同后，只检查属性，显示“检测到索引更新，等待文件写入完成…”。**连续两次 `poll_seconds` 轮询得到相同 SourceStamp** 才开始完整读取（默认间隔 5 秒，首次建索引也适用）。变化、离线或属性读取失败会重新开始稳定计数，旧缓存保持可用。开始重建前再确认 stamp 仍等于已稳定值，保留构建前后 stamp 校验、完整性校验和原子替换作为第二道保护。不添加 watcher，不遍历 NAS。

## 限制

- trigram 至少需要 3 个 Unicode 字符；1–2 字符搜索回退后台 SQL，百万条目时较慢，但可取消。
- 只支持普通文本和上述四种简单过滤器，不实现完整 Everything parser。
- 只比较 mtime + 大小；两者不变的内容更新无法检测。SMB 属性缓存可能延迟可见性。生成端应写临时 EFU 后原子替换。
- 索引构建需要旧/新数据库共存的空间；强制退出可能留下 `build-*.sqlite`，关闭应用后可仅清理这些临时文件。
- `Filename` 必须存在；Size、Date Modified、Attributes 可缺省。目录由 Attributes 的 `0x10` 位或末尾分隔符判断，无字段时不访问 NAS 来猜测。
- 坏行、非 UTF-8 行跳过并记录日志；坏数字按未知元数据处理。Date Modified 保留原值，不能解析为 FILETIME 时显示空白。
- Windows 与 Linux 文件名大小写不同；映射不自动修正实际文件名大小写。
- 暂不同时搜索多个 EFU，不生成索引，不调整挂载或网络。

## 检查与发布

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --no-default-features

# 可选 GUI 行菜单 / 剪贴板 / 原生列头测试（隔离显示）
EFUSEEK_GUI_TEST=1 GDK_BACKEND=x11 GTK_A11Y=none GSK_RENDERER=cairo \
  xvfb-run -a cargo test --bin efuseek -- --test-threads=1

# 可选：读取指定 EFU 并输出构建/查询耗时，不访问索引中的目标文件
cargo run --release --example check_index -- \
  /mnt/nas/index/files.efu /path/to/test-cache.sqlite
```

CI 在 `main` 推送和 Pull Request 时执行格式、严格 Clippy、完整测试、无 GTK 的 Core 测试及隔离 Xvfb 原生窗口测试。第三方 Actions 固定到完整提交 SHA；发布权限仅授予最后的上传 job。

### 发布新版本

1. 修改 `Cargo.toml` 的 `version`。
2. 在 `CHANGELOG.md` 添加唯一的 `## [版本] - YYYY-MM-DD` 章节，并填写非空变更内容。
3. 通过 CI 后提交并推送，再推送对应标签：

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "Release v0.1.1"
git push origin main
git tag -a v0.1.1 -m "EfuSeek v0.1.1"
git push origin v0.1.1
```

`release.yml` 会重跑 CI，校验标签与 Cargo 版本一致，再从 `CHANGELOG.md` **只提取对应版本章节**，构建 Linux 包、生成 SHA-256 校验文件并创建 GitHub Release。缺失、空白或重复章节，以及标签版本不一致，都会使发布失败，而不是使用错误说明。带 `-` 的版本标签发布为 prerelease。不要移动或复用已发布标签。

本地可先验证发布说明：

```bash
cargo run --locked --no-default-features --example release_notes -- \
  v0.1.0 /path/to/release-notes.md
```

日志默认显示警告/错误；可设置 `RUST_LOG=info` 排查。日志可能包含私人路径，请先脱敏。`.gitignore` 排除了构建目录、EFU、SQLite 缓存、本地配置和时间戳备份；发布时不要手工添加这些文件。

许可证为 **Mozilla Public License 2.0（MPL-2.0）**，完整条款见 [LICENSE](LICENSE)。依赖保留各自许可证。
