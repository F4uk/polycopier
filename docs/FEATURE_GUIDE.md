# polycopier 新功能启用指南

> 对应 commit `ebc4a88` — engineering mode 更新
> 生成时间：2026-04-19

---

## 1. PnL & Equity 图表（自动启用，无需配置）

**功能**：实时显示 7 天 Equity + 已实现/未实现 PnL，1H / 1D / 7D 时间范围，hover 显示十字线详情。

**启用方式**：零配置。服务端自动每 10 秒采样一次，最多保留 60,480 条记录（约 7 天）。

**查看入口**：Web UI 顶部导航 → **"PnL Chart"** tab。

---

## 2. 按类别仓位上限（`[risk_by_category]`）

**功能**：按市场分类（如 `politics.us-election`、`sports.nfl`）设置总仓位上限，在 BUY 开仓时校验，防止单一板块风险过度集中。

**适用场景**：你同时跟单多个主题，但又不想某一类目（如"美国大选"）的总敞口超过某个阈值。

### 启用步骤

① 打开 `config.toml`（工作目录根目录），在文件末尾添加：

```toml
[risk_by_category]
enabled = true
default_limit = 20.0        # 未分类市场的默认上限（USDC），设为 0 则无上限

# 以下为各分类的具体上限，分类名来自市场 slug 解析：
"politics.us-election" = 50.0
"sports.nfl" = 30.0
"economics.fed" = 20.0
# "某个分类" = 0.0  # 设为 0 = 完全禁止开仓
```

② 重启 bot 使配置生效。

③ **可选**：通过 Web UI Settings 面板配置（更直观，支持预设模板）：
   - Settings & Env → 滚动到 "Per-Category Position Limits" 区域
   - 勾选 "Enable Category Limits"
   - 使用 "Conservative" / "Aggressive" / "Politics Only" 预设，或手动添加/修改分类

### 分类名规则

分类从 Polymarket 市场的 URL slug 自动解析，规则为：主分类 + 子分类（如果有），用 `.` 连接。

| 市场名称 | 解析出的分类 |
|---|---|
| "US 2024 Presidential Election" | `politics.us-election` |
| "Super Bowl 2025 Winner" | `sports` |
| "Fed Rate Decision June 2025" | `economics.fed` |
| "Will NVIDIA beat earnings Q2?" | `economics` |
| 短 slug 无子分类 | 直接用主分类，如 `politics` |

### 行为说明

- **只在 BUY（开仓）时检查**，SELL 不受限制。
- 当前已有仓位 + 新开仓 > 上限 → 拒绝该笔开仓。
- 未在配置中列出的分类 → 使用 `default_limit`。
- `default_limit = 0`（默认）→ 无默认上限。

### 日志示例

```
[Risk/Category] Category 'politics.us-election': current=$30.00, trade=$15.00, limit=$50.00   ← 通过
[Risk/Category] Skipping open: category 'sports.nfl' would exceed position limit              ← 拒绝
```

---

## 3. 本地止损止盈（`[stop_loss]`，config.example.toml 中已有）

**功能**：对已持有的仓位，实时监控价格，触发条件时强制以市价卖出。

```toml
[stop_loss]
enabled = true
force_stop_price  = 0.15   # 价格跌破此值 → 立即止损卖出
force_close_price = 0.95   # 价格升至此值 → 立即止盈卖出
check_interval_secs = 3    # 检查频率
```

> **注意**：`force_stop_price` 和 `force_close_price` 是绝对价格（0~1），不是百分比。

---

## 4. 滑点保护（已有配置，无需额外操作）

`config.toml` → `[execution]` → `max_slippage_pct`：

```toml
[execution]
max_slippage_pct = 0.02   # 滑点上限 2%，超出则拒绝挂单
```

---

## 5. 洗盘过滤（自动启用，无需配置）

自动检测并跳过同一地址 60 秒内对同一 token ≥ 3 次的连续买卖行为，防范虚假刷单信号。

---

## 快速启用清单

| 功能 | 启用方式 |
|---|---|
| PnL Chart | ✅ 自动，无需操作 |
| 按类别仓位上限 | `config.toml` → `[risk_by_category]` → `enabled = true` |
| 本地止损止盈 | `config.toml` → `[stop_loss]` → `enabled = true` |
| 滑点保护 | `config.toml` → `[execution.max_slippage_pct]` |
| 洗盘过滤 | ✅ 自动，无需操作 |

