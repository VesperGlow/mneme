# 第一版验证记录

验证日期：2026-10-16。环境：Debian 13 amd64、Rootless Podman 5.4.2、无 GPU。核心在 Rust 1.98 本机及 Rust 1.90 容器构建器中均成功编译；运行镜像为 Debian 13。

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --locked --all-targets -- -D warnings` | 通过 |
| `cargo test --locked` | 11 项常规测试通过（含 Console）；真实服务测试默认显式跳过 |
| `cargo test --locked --test real_services -- --ignored --nocapture` | 实际启动固定版本 CPU 模型和 Neo4j 后通过 |
| Python 编译检查、Bash 语法检查 | 通过 |
| Podman 5.4 Quadlet generator dry run | 全部 pod/network/volume/container 单元成功生成 |
| Rust 与模型 Containerfile 构建 | 均通过，生成 `localhost/mneme:0.1.0` 与 `localhost/mneme-models:0.1.0` |
| `python3 scripts/smoke-pod.py` | 最终镜像完整生命周期测试通过 |

常规测试包含：鉴权、会话互斥、请求幂等与重放、任意 UTF-8/SSE 分片、截断流不进入历史或记忆、重启积压任务恢复、跨会话召回、原文证据、去重、版本替代、乱序任务保护、遗忘屏障、衰减遗忘、数据库单实例锁和权限、死信恢复。内容哈希另验证了小数点和内部标点不会被错误消除。

真实服务测试使用 `Qwen/Qwen3-Embedding-0.6B` 和 `Alibaba-NLP/gte-multilingual-reranker-base` 的固定 revision。检查 1024 维归一化向量，确认“我住在哪座城市”的中文问题将住所排在咖啡偏好前面，并验证 Neo4j CJK 全文、向量索引、实体匹配、共享实体两跳扩展及 Rust 混合检索。

完整 Pod 测试在同一 Rootless Pod 中运行真实 Rust 二进制、Neo4j、两个 CPU 模型和仅供测试的兼容 LLM 替身；启用 API/模型只读根文件系统、移除 capabilities、禁止提权。模型从缓存以 `HF_HUB_OFFLINE=1` 加载。写入用户住所后停止并重新启动整个 Pod，确认原始历史仍在、记忆 ID 不变、在新会话中能够正确回答住址，最后在线生成 SQLite 备份并执行 `integrity_check`。测试容器与专用对话/图谱卷已清理，部署镜像与模型缓存保留。

外部真实 LLM 的密钥和端点尚未提供。生成及提取协议通过确定性兼容接口验证；实际提供商的可用性、提取质量、价格及模型窗口需要在填写配置后验收。Quadlet 已做实际 generator 验证，未替用户安装生产 systemd 单元或启动含占位配置的生产服务。

## Mneme Console 验证补充

Console 的界面、CRUD 与检索调试都通过现有 Axum API 读取 SQLite 真实数据。运行时没有增加 Node.js、前端包管理器或容器。

- 11 项常规测试全部通过，包含 5 项 Console 集成测试：受保护接口与静态资源安全头、统计/筛选/排序、重复检测、编辑版本冲突、完整修订链、来源跨自动版本保护、旧提取和向量写回阻断、v1 数据库迁移及旧投影排队。
- 专门用阻塞的 Reranker 复现“精排过程中删除记忆”，包括低于阈值而未入选的候选。删除内容既不返回最终召回，也不在调试候选中回显。
- 真实 CPU 模型与 Neo4j 测试验证人工修改住所后全文、向量、实体关系更新，原实体关系被移除；删除后图节点和无关联实体清除，旧向量写回被拒绝。
- 已用宿主机 Chromium 实际运行 `scripts/console-browser-test.cjs`：1440px 桌面与 390px 手机布局、登录、创建、编辑、修订详情、删除、检索通道分数、系统状态、HTML 转义、不使用持久化浏览器存储、退出/刷新/闲置锁定均通过。浏览器未报告脚本或 CSP 错误。
- `cargo fmt`、严格 Clippy、JavaScript/Python/Bash 语法检查通过。API Containerfile 已加入 web/，资源随二进制构建。

截图位于忽略的 `artifacts/`：`console-desktop.png`、`console-mobile.png`、`console-mobile-library.png`、`console-detail.png`、`console-retrieval.png`、`console-system.png`、`console-login.png`。截图与浏览器测试只使用专用临时数据库和明确标识的测试数据，不使用私人记忆或生产 Token。

完整 Pod 测试还调用 Console 的新增、编辑、修订历史、真实模型检索调试、删除与 reindex，检查异步索引完成标记，并继续验证整个 Pod 的离线重启、跨会话召回和在线备份。使用说明、API 字段及逻辑删除语义见 [Console 文档](console.md)。

最终交付镜像 `localhost/mneme:0.1.0` 已完成构建，镜像 ID 为 `a542fc13e0c9422a4d9b9861a27cbf2fcc6c1f6db373d0371cfad54885d5969b`。该镜像的完整 Pod 验证于 2026-10-16 13:14 UTC 前完成并通过，包含最后的并发删除保护。测试容器和专用对话/图谱数据卷已清理。
