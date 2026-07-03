# AI Gateway — 项目开发规范

## 铁律：没有测试验证，不准说"完成"

每次代码改动，**必须执行以下全部步骤**，缺一不可：

### 1. 编译检查
```bash
cargo check --workspace 2>&1 | tail -5
```
必须 `Finished`，无 error。

### 2. 单元测试 + 集成测试
```bash
cargo test --workspace 2>&1 | tail -20
```
必须全部 `ok`，无 `FAILED`。

### 3. 端到端验证（用户可见的改动必须做）
启动服务器，用 `curl` 实际调用每个受影响的端点：
```bash
# 启动
nohup cargo run --bin gateway-server > /tmp/gateway.log 2>&1 &
sleep 8

# 验证每个场景（根据改动调整）
curl -s -o /dev/null -w "%{http_code}" http://localhost:8080/health        # 期望 200
curl -s -w "\n" -H "Authorization: Bearer <key>" http://localhost:8080/v1/models
# ... 列出所有受影响的端点及其期望状态码/响应

# 清理
pkill -9 -f gateway-server
```

### 4. 汇报前自检
- [ ] `cargo check` 通过
- [ ] `cargo test` 全部通过
- [ ] 端点实际返回期望结果（附 curl 输出截图/粘贴）
- [ ] 对照任务清单逐项打勾

**违反以上流程 = 没有完成。不准向用户报告"已修复"/"已完成"。**

---

## 项目结构

```
crates/
├── gateway-core/     # 类型、trait、配置、错误
├── gateway-server/   # Axum HTTP 服务、路由、中间件、Admin UI
└── providers/        # OpenAI / Anthropic / Gemini / Ollama 适配器
```

## 配置

- 配置文件：`config.toml`（运行时）、`config.example.toml`（模板）
- 环境变量前缀：`AI_GATEWAY__`（双下划线分隔层级）
- 默认端口：8080
- Admin UI：`http://localhost:8080/admin`

## 已知安全问题（MVP 0 待修）

- [ ] API Key 明文存储 → 改为 HMAC-SHA256 哈希
- [ ] 限流中间件未实际生效
- [ ] extra_headers 未透传
- [ ] 环境变量前缀拼写 `AI_GATERARY` → `AI_GATEWAY`
- [ ] dead code 清理
- [ ] Admin UI 认证流程（已完成基础修复，待验证）
