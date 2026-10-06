# Changelog

所有值得记录的变更均维护在此文件。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

GitHub Release 自动读取与标签匹配的版本章节，不包含其他版本或 Unreleased 内容。

## [Unreleased]

### 新增

- 第一批字面查询过滤器 `ext:`、`file:`、`folder:`、`path:`，支持与普通文本 AND 组合、Unicode 和未知前缀回退；保留参数绑定、trigram 和短查询回退。
- 修改时间列：解析 EFU Windows FILETIME，按本机时区显示，缺失或无效值留空。
- 名称、大小、修改时间原生列头升降序排序；后台 SQLite ORDER BY 和分页，不在 GTK 中全量加载/排序，未知元数据始终最后，id 稳定去歧义。
- 行右键原生菜单：打开、打开所在目录、复制名称、本机完整路径和 EFU 原始路径；先选中右键行，未映射本机路径禁用，GTK 剪贴板不调用 shell。
- 查询、排序/旧 schema 离线兼容、日期解析、持续增长 EFU、虚拟列表排序代数和 Xvfb 原生菜单/剪贴板测试。

### 改进

- EFU 变化后等待连续两次轮询 SourceStamp 相同才重建；变化/离线/读取失败重置等待，保留旧索引，避免持续写入时反复完整读取。
- 重建前确认已稳定 stamp，保留读取前后 stamp 校验、完整性校验及原子替换。
- 新缓存增加 SQLite 排序键和索引，正确处理 u64 大小及目录/NULL；兼容 v0.1.0 离线缓存，来源稳定后再原子升级。
- 分页查询也使用 generation / progress handler 取消过期 SQLite 排序请求。

## [0.1.0] - 2026-10-06

### 新增

- Rust + GTK4 + libadwaita 原生 Linux 桌面界面，窗口标题显示发布版本。
- 流式读取 UTF-8 EFU CSV，支持中文、日文、Windows、UNC 和 Linux 路径，以及缺失元数据和异常行恢复。
- 本地 SQLite FTS5 trigram 任意子串搜索，按名称完全匹配、名称开头、名称包含和路径包含排序。
- 启动直接复用缓存，后台检查 EFU 更新，构建新索引后原子替换；离线可搜索旧缓存。
- 空搜索可浏览完整索引，虚拟列表按需分页加载，不为全部条目创建 GTK 对象。
- 文件和目录区分显示，文件大小自动格式化，GIO 异步打开文件或所在目录。
- 设置页可选择 EFU，并新增、编辑和删除路径映射；列表显示映射后的本机路径。
- 最长路径前缀映射、独立来源缓存、配置保存备份及旧缓存迁移。
- 输入法优先的快捷键处理，避免提前抢占日语候选确认的 Enter。
- 可移植 XDG 配置，所有可调默认参数集中于 `config.default.toml`，不含个人路径或 NAS 预设。
- Core、配置、后台更新、虚拟列表和快捷键的单元与集成测试。
- GitHub Actions CI 和标签发布：自动校验版本并从本文件生成 Release 说明，附 Linux x86_64 发布包及 SHA-256 校验文件。

### 已知限制

- 仅支持普通文本搜索，尚未实现 `ext:`、`folder:` 等语法。
- 1–2 字符查询使用后台 SQL 回退，百万条目下可能较慢。
- 文本搜索基于 EFU 原始路径；本机路径映射只影响显示和打开。
- 当前预编译包为 Linux x86_64，依赖系统 GTK4 / libadwaita，不是 AppImage 或 Flatpak。
- EfuSeek 不扫描 NAS 或磁盘，也不生成 EFU 索引。

### 许可证

- Mozilla Public License 2.0（MPL-2.0）；依赖保留各自许可证。
