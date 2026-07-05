# AI Gateway 架构设计分析

## 1. 项目概述

这是一个用 Rust 编写的多 LLM 供应商统一网关，提供 **OpenAI 兼容的 REST API**。核心价值：
- 统一接口：客户端只需使用 OpenAI 格式
- 多供应商支持：OpenAI、Anthropic、Gemini、Ollama + 任意 OpenAI 兼容端点
- 企业级功能：认证、限流、配额、审计、计量、熔断器等

## 2. 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                    AI Gateway (Axum)                        │
├─────────────────────────────────────────────────────────────┤
│  ┌──────────┐  ┌─────────┐  ┌──────────┐  ┌─────────────┐ │
│  │Auth      │  │Rate     │  │Quota     │  │X-Request-ID│ │
│  │Middleware│  │Limit    │  │Middleware│  │Middleware  │ │
│  └────┬─────┘  └────┬────┘  └────┬─────┘  └──────┬──────┘ │
│       │             │             │                │        │
│       └─────────────┴──────┬──────┴────────────────┘        │
│                            ▼                                 │
│                    ┌─────────────┐                          │
│                    │  Router     │                          │
│                    │  (Model→    │                          │
│                    │   Provider) │                          │
│                    └──────┬──────┘                          │
│                           │                                 │
│          ┌────────┬───────┼───────┬────────┬────────┐      │
│          ▼        ▼       ▼       ▼        ▼        ▼      │
│    ┌────────┐ ┌────────┐ ┌───────┐ ┌────────┐ ┌─────────┐ │
│    │ OpenAI │ │Anthropi│ │Gemini │ │ Ollama │ │OpenAI   │ │
│    │Provider│ │cProvider││Provider│ │Provider│ │Compat   │ │
│    └────┬───┘ └────┬───┘ └───┬───┘ └────┬───┘ └────┬────┘ │
│         │          │         │          │          │      │
│         └──────────┴─────────┴──────────┴──────────┘      │
│                           │                                 │
│                    ┌──────┴────────────────────┐          │
│                    │  Retry + Circuit Breaker  │          │
│                    │  + Fallback Chain         │          │
│                    └─────────────┬─────────────┘          │
│                                  ▼                         │
│                        ┌──────────────────┐               │
│                        │   Persistence    │               │
│                        │  (Metering Store)│               │
│                        └──────────────────┘               │
└─────────────────────────────────────────────────────────────┘
```

## 3. Crate 结构

### 3.1 `gateway-core` - 核心抽象层

**职责**：定义核心类型、trait、配置和错误

| 文件 | 职责 |
|------|------|
| `lib.rs` | 公开模块和类型 |
| `config.rs` | TOML 配置结构 + 环境变量覆盖 |
| `auth_key.rs` | API Key HMAC-SHA256 + 随机 salt 存储 |
| `tenant.rs` | 租户 / 角色 / TenantContext |
| `metering.rs` | PricingTable / RateCard / 成本计算 |
| `provider.rs` | `LLMProvider` trait（非流/流式接口） |
| `types.rs` | OpenAI 兼容的请求/响应类型 |
| `error.rs` | 统一错误 `GatewayError` |
| `audit.rs` | 审计日志 writer trait + JSONL 实现 |

**关键设计**：
- **不可变配置**：`AppConfig` 是只读结构，运行时更新通过 `AppState` 的 `RwLock` 实现
- **安全存储**：API Key 从不存储明文，启动时即哈希
- **配置优先级**：TOML < 环境变量（`AI_GATEWAY__SECTION__KEY`）

### 3.2 `providers` - 供应商适配器层

**职责**：实现 `LLMProvider` trait，适配各供应商的 API

| 供应商 | 特点 |
|--------|------|
| `openai.rs` | 原生 OpenAI 格式 |
| `anthropic.rs` | OpenAI ↔ Claude 格式转换 |
| `gemini.rs` | OpenAI ↔ Gemini 格式转换 |
| `ollama.rs` | OpenAI ↔ Ollama 格式转换 |
| `openai_compat.rs` | 通用适配器，支持任意 OpenAI 兼容端点（Mistral/Groq/DeepSeek等）+ 自定义字段覆盖 |

**关键设计**：
- **统一接口**：所有供应商都实现 `LLMProvider` trait
- **格式转换**：各供应商内部完成 OpenAI 格式 ↔ 自家格式的双向转换
- **扩展性**：`OpenAICompatProvider` 通过 `FieldOverrides` 支持供应商特有字段

### 3.3 `gateway-server` - HTTP 服务层

**职责**：Axum 服务器、路由、中间件、状态管理

| 文件/模块 | 职责 |
|-----------|------|
| `main.rs` | 启动入口、tracing 初始化、优雅关闭 |
| `routes.rs` | 主路由 + `/v1/chat/completions` + `/v1/models` + Admin API |
| `state.rs` | `AppState` - 共享状态（config, providers, auth, metrics 等） |
| `admin.rs` | `/api/admin/*` REST API（20+ 端点）|
| `retry.rs` | 重试逻辑 + 跨供应商降级链 |
| `circuit_breaker.rs` | 每供应商熔断器 |
| `persistence.rs` | 计量事件 JSONL 落盘 + 回放 |
| `json_logger.rs` | 结构化 JSON 日志 |
| `log_buffer.rs` | 内存日志环形缓冲（Admin UI 用）|
| `static_files.rs` | Admin UI 静态文件嵌入 |
| `metrics/` | 计量 / 配额 / Prometheus |
| `middleware/` | 认证、限流、配额、RBAC、X-Request-ID |

## 4. 请求处理流程详解

### 4.1 非流式请求（带缓存）

```
客户端请求
    │
    ▼
[1] x_request_id_middleware（生成/传播请求 ID）
    │
    ▼
[2] auth_middleware
    ├─ 验证 Bearer Token（HMAC 常时比较）
    ├─ 从 `ApiKeyStore` 查找匹配项
    └─ 注入 `TenantContext`（tenant_id, role, key_id）
    │
    ▼
[3] rate_limit_middleware
    ├─ 按 tenant_id 获取/创建 `TokenBucket`
    ├─ 消耗一个 token（µ-令牌实现，原子操作）
    └─ 超限时返回 429 + Retry-After
    │
    ▼
[4] quota_middleware
    ├─ 检查租户配额（RPM/RPD/TPM/TPD）
    └─ 预先记录请求（轻量级）
    │
    ▼
[5] route → chat_completions handler
    │
    ├─ 检查缓存是否启用
    │   ├─ 否：跳过缓存逻辑
    │   └─ 是：
    │       ├─ 计算缓存键（model + messages + 参数哈希）
    │       ├─ 缓存命中 → 直接返回响应
    │       └─ 缓存未命中 → 继续
    │
    ▼
[6] chat_completion_with_retry
    │
    ├─ build_fallback_chain：按优先级排序可用供应商
    │   └─ 内置供应商（openai/anthropic/gemini/ollama）优先
    │
    ├─ 对每个供应商（主供应商 → 备选）：
    │   ├─ circuit_breaker.allow_request(?) → 跳过开路供应商
    │   │
    │   ├─ 获取供应商实例（`state.get_provider_by_name`）
    │   │
    │   ├─ 重试循环（max_retries = 2）：
    │   │   ├─ 超时包装：120s 绑定上游调用
    │   │   ├─ 调用 `provider.chat_completion(request)`
    │   │   │
    │   │   ├─ 成功 → 记录成功、返回响应
    │   │   │
    │   │   └─ 失败 → 
    │   │       ├─ 判断是否可重试（`is_retryable`）
    │   │       │   ├─ 408/429/5xx + transport 错误 → 可重试
    │   │       │   └─ 其他 4xx → 不重试
    │   │       │
    │   │       ├─ 可重试且未达重试次数 → 
    │   │       │   ├─ 指数退避（1s → 2s → 4s ...）
    │   │       │   ├─ 全抖动（随机化，防同步重试）
    │   │       │   └─ 下次尝试
    │   │       │
    │   │       └─ 不可重试 或 已达重试次数 →
    │   │           ├─ 记录失败到熔断器
    │   │           └─ 尝试下一个供应商
    │   │
    └─ 所有供应商失败 → 返回错误
    │
    ▼
[7] 响应处理
    │
    ├─ 提取用量（usage）
    ├─ 计算成本（`config.pricing.estimate_cost`）
    │
    ├─ 记录计量（`record_metering`）：
    │   ├─ 创建 `MeteringEvent`
    │   ├─ 检查成本告警阈值（触发一次/计费周期）
    │   ├─ 写入内存聚合（`TenantUsage`）
    │   ├─ 追加到细节事件队列（`VecDeque`，上限 10,000）
    │   └─ 异步持久化到 JSONL（如配置了 `metering_path`）
    │
    ├─ 记录 Prometheus 指标（request_total 等）
    │
    ├─ 缓存响应（如启用）
    │
    └─ 返回 JSON 响应
```

### 4.2 流式请求（SSE）

```
客户端请求（stream: true）
    │
    ▼
[1-4] 同非流式：x_request_id → auth → rate_limit → quota
    │
    ▼
[5] chat_completions handler
    │
    ├─ 估算提示词令牌数（按字符数 / 4）
    │
    ├─ 调用 chat_completion_stream_with_retry
    │   │
    │   ├─ build_fallback_chain（同非流式）
    │   │
    │   ├─ 对每个供应商：
    │   │   ├─ circuit_breaker 检查
    │   │   │
    │   │   ├─ 超时包装：30s 绑定握手（connect + 首响应）
    │   │   │
    │   │   ├─ 调用 `provider.chat_completion_stream(request)`
    │   │   │   └─ 返回 `BoxStream<Result<ChatCompletionChunk, ...>>`
    │   │   │
    │   │   ├─ 成功 → 记录成功、返回流
    │   │   │
    │   │   └─ 失败 → 记录失败、立即尝试下一个供应商
    │   │       （流式不做重试，只做供应商降级）
    │   │
    │   └─ 所有供应商失败 → 返回错误
    │
    ▼
[6] 创建 SSE 流
    │
    ├─ 记录计量（使用估算的提示词令牌数）
    │   └─ 完成令牌数暂为 0（需等最终 usage chunk）
    │
    ├─ 映射响应流到 SSE Event
    │   ├─ Ok(chunk) → Event::data(serde_json::to_string(chunk))
    │   └─ Err(e) → Event::data(ErrorResponse)
    │
    └─ 返回 `Sse` 响应（Axum 自动处理流式发送）
```

## 5. 核心组件深度解析

### 5.1 认证与租户管理（`middleware/auth.rs` + `gateway-core/auth_key.rs`）

**数据结构**：
```rust
struct ApiKeyEntry {
    hash: String,          // HMAC-SHA256(salt, key) 十六进制
    salt: Salt,           // 16 字节随机盐
    tenant_id: String,    // 租户 ID
    role: String,         // 角色（admin/developer）
    key_id: String,       // 指纹（key_ + hash 前 8 字符）
}
```

**流程**：
1. **启动时**：配置中的明文密钥被哈希成 `ApiKeyEntry`，存入 `ApiKeyStore`
2. **运行时**：
   - 提取 `Authorization: Bearer <key>` 中的 `<key>`
   - 遍历 `ApiKeyStore.entries`，对每个条目：
     - `HMAC(salt, 输入密钥)` 
     - 常时比较结果与存储的哈希
   - 匹配成功 → 注入 `TenantContext`（包含 `tenant_id`, `role`, `key_id`）
   - 失败 → 返回 401

**安全措施**：
- 密钥永不以明文存储或记录
- 常时比较（`subtle::ConstantTimeEq`）防时序攻击
- 弱密钥启动时告警（`test-key`, `my-secret-key` 等）
- 热路径原子检查 `auth_enabled`（避免每次加锁）

### 5.2 限流（`middleware/rate_limit.rs`）

**设计**：
- **每租户独立桶**：不同租户互不影响
- **µ-令牌实现**：1 逻辑令牌 = 1000 µ-令牌，整数原子操作避免浮点误差
- **令牌桶算法**：容量 = RPM，速率 = RPM/60 每秒
- **原子操作**：`compare_exchange` 实现无锁并发

**关键代码**：
```rust
// 刷新令牌（基于时间差，使用 CAS 避免重复计算）
fn refill(&self) {
    let now = current_time_ms();
    let last = self.last_ms.load(Ordering::Relaxed);
    let elapsed_ms = now - last;
    
    if elapsed_ms < MIN_REFILL_GAP_MS { return; }
    
    // CAS 争夺刷新窗口
    if self.last_ms.compare_exchange(last, now, ...).is_err() {
        return; // 失败者直接返回，不重复计算
    }
    
    // 只有赢家执行刷新
    let new_tokens = elapsed_ms * micro_per_ms(capacity);
    self.tokens.fetch_add(new_tokens, ...);
}

// 消费令牌
fn consume(&self) -> Option<Duration> {
    self.refill();
    
    loop {
        let cur = self.tokens.load(...);
        if cur < MICRO { /* 计算等待时间 */ }
        
        // CAS 减少令牌
        match self.tokens.compare_exchange(cur, cur - MICRO, ...) {
            Ok(_) => return None,  // 消费成功
            Err(_) => continue,    // 重试
        }
    }
}
```

**优化**：
- **读多写少**：`buckets` 使用 `RwLock`，不同租户并发读
- **懒初始化**：首次请求时才创建租户的桶
- **全局默认**：`default_rpm` 原子变量，新租户自动继承

### 5.3 配额（`middleware/quota_middleware.rs` + `metrics/quota.rs`）

**配额类型**：
- **RPM**：每分钟请求数
- **RPD**：每日请求数
- **TPM**：每分钟令牌数
- **TPD**：每日令牌数

**检查时机**：
- **预检**：请求开始前检查（使用保守估计，如 100 tokens）
- **实际扣除**：请求完成后按真实用量扣除

**与限流的区别**：
- **限流**：保护网关自身（防止过载），短期窗口（秒/分钟级），恢复快
- **配额**：业务策略（租户计费），长期窗口（日/月级），恢复慢

### 5.4 熔断器（`circuit_breaker.rs`）

**状态机**：
```
Closed (正常) 
    └─ 连续失败 ≥ 阈值（默认 5）→ Open (开路)
                                         │
                                         ├─ 冷却期（默认 60s）
                                         │
                                         └─ 冷却结束 → HalfOpen (半开)
                                                            │
                            ┌───────────────────────────────┘
                            │
                            ├─ 成功 → Closed
                            │
                            └─ 失败 → Open
```

**并发优化**：
- **读锁快路径**：99% 情况只需读锁（Closed/HalfOpen/未冷却的 Open）
- **写锁慢路径**：仅冷却结束的 Open→HalfOpen 转换需要写锁
- **状态隔离**：每个供应商独立熔断，互不影响

**指标**：
- 总拒绝请求数（`total_rejected` 原子计数）
- 各供应商状态快照（用于 `/deep-health`）

### 5.5 重试与降级（`retry.rs`）

**重试策略**：
- **指数退避**：1s → 2s → 4s → 8s → 16s（上限）
- **全抖动**：`sleep(rand(0, backoff))` 防同步重试风暴
- **可重试判定**：
  ```rust
  fn is_retryable(error: &GatewayError) -> bool {
      match error {
          UpstreamError { status: Some(408 | 429 | 500..=599) } => true,
          UpstreamError { status: None } => true,  // transport timeout
          RateLimited => true,
          _ => false,
      }
  }
  ```

**降级链**：
1. 根据模型查找所有可用供应商
2. 内置供应商优先（openai/anthropic/gemini/ollama）
3. 依次尝试，失败即切换（不重试同一供应商多次）

### 5.6 计量与成本（`metrics/metering.rs`）

**数据流**：
```
请求完成
    │
    ▼
MeteringEvent (详细事件)
    │
    ├─ 写入内存聚合 (TenantUsage)
    │   ├─ total_requests / tokens / cost
    │   └─ per_model 细分
    │
    ├─ 追加到 VecDeque (上限 10,000，防 OOM)
    │
    └─ 异步追加到 JSONL (如配置了 metering_path)
         └─ 启动时回放，恢复用量/成本
```

**成本告警**：
- **一次性触发**：每个计费周期首次超过阈值时触发
- **防重复**：`alert_triggered` 标志位，重置计费周期时清零
- **日志**：`tracing::warn!` 记录告警事件

**成本汇总（Admin API）**：
- 滑动窗口（24h/7d/30d）
- 顶 5 租户 + 全局模型分布
- 支持租户过滤

### 5.7 持久化（`persistence.rs`）

**现状**：
- **计量事件**：JSONL 落盘 + 启动回放（#6 已完成）
- **Key Store / 配额**：仍为内存，重启需从配置重建

**安全缺口**：
- 计量事件**未签名**：有文件写权限者可伪造/篡改
- **后续计划**：补逐事件 HMAC 签名

## 6. 关键设计决策

### 6.1 为什么用 Axum？

- **异步优先**：完美匹配 Tokio + async/await
- **类型安全**：提取器（Extractor）编译时检查
- **中间件灵活**：`tower::Service` 生态丰富
- **路由高效**：基于匹配树（matchit），O(log n）

### 6.2 为什么选择 RwLock 而非 Mutex？

- **读多写少场景**：配置读取、提供商查找、认证验证都是高频读
- **并发性能**：多个租户可并发读取各自的状态，不互相阻塞
- **分层锁**：
  - `RwLock<HashMap>`：粗粒度，保护整个集合
  - 内部状态（如 `TokenBucket`）：细粒度原子操作，避免锁竞争

### 6.3 为什么计量事件用 VecDeque 而非 Channel？

- **边界控制**：`VecDeque` 可设置容量上限，防止 OOM
- **重放能力**：启动时需从磁盘回放事件重建状态
- **同步语义**：计量是请求完成后的同步操作，不需要异步解耦

### 6.4 为什么熔断器状态用 RwLock 而非 DashMap？

- **状态机复杂性**：熔断器状态转换涉及多个字段（failures, opened_at, half_open_successes），需要原子性
- **写少读多**：99% 情况是读锁检查状态，仅状态转换需要写锁
- **简化代码**：`RwLock` 语义清晰，调试容易

## 7. 性能优化点

### 7.1 热路径优化

| 优化 | 位置 | 效果 |
|------|------|------|
| `auth_enabled` 原子变量 | `auth_middleware` | 避免每次加锁检查配置 |
| µ-令牌原子操作 | `TokenBucket` | 无锁限流 |
| 读锁快路径 | `circuit_breaker` | 99% 请求无需写锁 |
| 懒初始化桶 | `RateLimiter` | 避免预分配所有租户 |

### 7.2 内存优化

| 优化 | 位置 | 效果 |
|------|------|------|
| `VecDeque` 容量上限 | `MeteringService` | 防 OOM |
| 事件异步持久化 | `record` | 不阻塞请求路径 |
| 缓存键哈希 | `compute_cache_key` | 减少内存占用 |

### 7.3 并发优化

| 优化 | 位置 | 效果 |
|------|------|------|
| 每租户独立桶 | `RateLimiter` | 无跨租户锁竞争 |
| 供应商级熔断 | `CircuitBreaker` | 故障隔离 |
| 全抖动重试 | `retry.rs` | 防同步重试风暴 |

## 8. 安全设计

### 8.1 已实施的安全措施

| 措施 | 位置 | 说明 |
|------|------|------|
| HMAC-SHA256 + salt | `auth_key.rs` | 密钥存储不可逆 |
| 常时比较 | `ApiKeyEntry::verify` | 防时序攻击 |
| 弱密钥检测 | `main.rs` | 启动时告警 |
| RBAC | `middleware/rbac.rs` | 管理操作需 admin 角色 |
| 审计日志 | `audit.rs` | 管理操作留痕（JSONL） |
| 非 root 运行 | `Dockerfile` | 容器安全基线 |
| 熔断器 | `circuit_breaker.rs` | 防雪崩 |

### 8.2 安全缺口

| 问题 | 位置 | 风险 | 计划 |
|------|------|------|------|
| 计量事件未签名 | `persistence.rs` | 可伪造账单 | 补 HMAC 签名 |
| 纯 HTTP 传输 | `main.rs` 注释 | 密钥明文传输 | 需反向代理终结 TLS |
| Key Store 内存 | `state.rs` | 重启丢失 | 迁移到 DB |

## 9. 企业级特性支持

| 特性 | 状态 | 说明 |
|------|------|------|
| 多租户隔离 | ✅ | 按 tenant_id 隔离计量/配额/成本 |
| 角色权限（RBAC） | ✅ | admin / developer 两级 |
| 计量与成本 | ✅ | 按模型定价，告警阈值 |
| 限流 | ✅ | 每租户令牌桶 |
| 配额 | ✅ | RPM/RPD/TPM/TPD |
| 审计日志 | ✅ | JSONL 追加（可选） |
| 熔断器 | ✅ | 每供应商独立 |
| 重试 + 降级 | ✅ | 指数退避 + 跨供应商 |
| 响应缓存 | ✅ | moka TTL 缓存 |
| Prometheus 指标 | ✅ | /metrics 端点 |
| OTel 链路追踪 | ❌ | 仅有日志 + 指标 |
| 动态模型发现 | ❌ | 需手动配置模型列表 |
| Provider 优先级 | ❌ | 内置供应商硬编码优先 |

## 10. 总结

这是一个设计良好的企业级 AI 网关：

**优势**：
- ✅ **分层清晰**：core（抽象）→ providers（适配）→ server（服务）
- ✅ **安全基线**：HMAC 存储、常时比较、RBAC、审计
- ✅ **企业就绪**：多租户、计量、配额、熔断、重试
- ✅ **可观测性**：Prometheus + JSON 日志 + 审计
- ✅ **性能优化**：原子操作、读写锁、懒初始化

**待改进**：
- ⚠️ **持久化不全**：Key Store / 配额仍内存
- ⚠️ **计量未签名**：可被篡改
- ⚠️ **无 OTel**：缺少分布式追踪
- ⚠️ **路由简单**：内置供应商硬编码优先级

**适用场景**：
- 统一多 LLM 供应商 API
- 企业内部共享 AI 能力
- SaaS 产品后端（需补充 DB 持久化）

---
*分析日期：2026-07-05*
*基于 commit `a54c0d5` 代码*
