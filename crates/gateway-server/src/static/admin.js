// ─── State ───────────────────────────────────────────────────────
let currentPage = 'dashboard';
let autoRefreshLogsInterval = null;
let autoRefreshDashboardInterval = null;
const API_KEY_STORAGE = 'ai_gateway_admin_key';

function getApiKey() {
    return localStorage.getItem(API_KEY_STORAGE) || '';
}

function setApiKey(key) {
    if (key) {
        localStorage.setItem(API_KEY_STORAGE, key);
    } else {
        localStorage.removeItem(API_KEY_STORAGE);
    }
}

let loginBannerVisible = false;

function showLoginPrompt(message) {
    loginBannerVisible = true;
    let banner = document.getElementById('login-banner');
    if (!banner) {
        banner = document.createElement('div');
        banner.id = 'login-banner';
        banner.className = 'login-banner';
        banner.innerHTML = `
            <div class="login-banner-inner">
                <span>🔑 ${message || '请输入 API Key 以访问管理面板'}</span>
                <input type="password" id="login-key-input" placeholder="输入 API Key" />
                <button class="btn btn-primary btn-sm" id="login-key-btn">确认</button>
            </div>
        `;
        document.body.insertBefore(banner, document.body.firstChild);
        document.getElementById('login-key-btn').addEventListener('click', submitLoginKey);
        document.getElementById('login-key-input').addEventListener('keydown', (e) => {
            if (e.key === 'Enter') submitLoginKey();
        });
    } else {
        // 只更新提示文字，保留用户已输入的内容
        const msgEl = banner.querySelector('span');
        if (msgEl) msgEl.textContent = '🔑 ' + (message || '请输入 API Key 以访问管理面板');
    }
    // 聚焦到输入框
    setTimeout(() => {
        const inp = document.getElementById('login-key-input');
        if (inp) inp.focus();
    }, 50);
}

function submitLoginKey() {
    const inp = document.getElementById('login-key-input');
    const key = inp ? inp.value.trim() : '';
    if (key) {
        setApiKey(key);
        loginBannerVisible = false;
        const banner = document.getElementById('login-banner');
        if (banner) banner.remove();
        clearAllIntervals();
        navigateTo('dashboard');
    }
}

function clearLoginPrompt() {
    loginBannerVisible = false;
    const banner = document.getElementById('login-banner');
    if (banner) banner.remove();
}

function clearAllIntervals() {
    if (autoRefreshLogsInterval) { clearInterval(autoRefreshLogsInterval); autoRefreshLogsInterval = null; }
    if (autoRefreshDashboardInterval) { clearInterval(autoRefreshDashboardInterval); autoRefreshDashboardInterval = null; }
}

// ─── Navigation ──────────────────────────────────────────────────
document.querySelectorAll('.nav-item').forEach(item => {
    item.addEventListener('click', () => {
        const page = item.dataset.page;
        navigateTo(page);
    });
});

function navigateTo(page) {
    currentPage = page;

    // Update nav
    document.querySelectorAll('.nav-item').forEach(el => el.classList.remove('active'));
    document.querySelector(`[data-page="${page}"]`).classList.add('active');

    // Update pages
    document.querySelectorAll('.page').forEach(el => el.classList.remove('active'));
    document.getElementById(`page-${page}`).classList.add('active');

    // Update title
    const titles = {
        dashboard: '仪表盘',
        tenants: '租户管理',
        providers: '提供商管理',
        apikeys: 'API Key 管理',
        usage: '用量统计',
        config: '系统配置',
        logs: '实时日志'
    };
    document.getElementById('page-title').textContent = titles[page] || page;

    // Load page data
    loadPageData(page);
}

function loadPageData(page) {
    switch (page) {
        case 'dashboard': loadDashboard(); break;
        case 'tenants': loadTenants(); break;
        case 'providers': loadProviders(); break;
        case 'apikeys': loadApiKeys(); break;
        case 'usage': loadUsage(); break;
        case 'config': loadConfig(); break;
        case 'logs': loadLogs(); break;
    }
}

// ─── Usage Page (MVP 2 Metering) ────────────────────────────────
async function loadUsage() {
    // Load rate card
    try {
        const card = await api('/api/admin/config/rate-card');
        const prompt = card.prompt_per_million ?? 0;
        const completion = card.completion_per_million ?? 0;
        const promptEl = document.getElementById('ratecard-prompt');
        const completionEl = document.getElementById('ratecard-completion');
        if (promptEl) promptEl.value = prompt;
        if (completionEl) completionEl.value = completion;
    } catch (e) {
        console.warn('Load rate-card failed:', e);
    }

    // Load usage
    try {
        const usageList = await api('/api/admin/usage');
        let totalReq = 0, totalTokens = 0, totalCost = 0, totalErrors = 0;
        const tbody = document.querySelector('#usage-table tbody');
        tbody.innerHTML = '';

        if (!usageList || usageList.length === 0) {
            const tr = document.createElement('tr');
            const td = document.createElement('td');
            td.colSpan = 7;
            td.style.cssText = 'text-align:center;color:#888;padding:20px;';
            td.textContent = '暂无用量数据';
            tr.appendChild(td);
            tbody.appendChild(tr);
        } else {
            usageList.forEach(u => {
                totalReq += (u.total_requests || 0);
                totalTokens += (u.total_tokens || 0);
                totalCost += (u.total_cost_cents || 0);
                totalErrors += (u.total_errors || 0);

                const tr = document.createElement('tr');
                const cells = [
                    u.tenant_id || '-',
                    (u.total_requests || 0).toLocaleString(),
                    (u.total_prompt_tokens || 0).toLocaleString(),
                    (u.total_completion_tokens || 0).toLocaleString(),
                    (u.total_tokens || 0).toLocaleString(),
                    (u.total_errors || 0).toLocaleString(),
                    (u.total_cost_cents || 0).toFixed(2),
                ];
                cells.forEach(val => {
                    const td = document.createElement('td');
                    td.textContent = val;
                    tr.appendChild(td);
                });
                tbody.appendChild(tr);
            });
        }

        document.getElementById('usage-total-cost').textContent = totalCost.toFixed(2);
        document.getElementById('usage-total-requests').textContent = totalReq.toLocaleString();
        document.getElementById('usage-total-tokens').textContent = totalTokens.toLocaleString();
        document.getElementById('usage-total-errors').textContent = totalErrors.toLocaleString();
    } catch (e) {
        console.warn('Load usage failed:', e);
        toast('加载用量数据失败: ' + e.message, 'error');
    }
}

async function saveRateCard() {
    const promptEl = document.getElementById('ratecard-prompt');
    const completionEl = document.getElementById('ratecard-completion');
    const prompt = parseInt(promptEl.value || '0', 10);
    const completion = parseInt(completionEl.value || '0', 10);

    if (isNaN(prompt) || prompt < 0 || isNaN(completion) || completion < 0) {
        toast('请输入有效的非负整数费率', 'error');
        return;
    }

    try {
        await api('/api/admin/config/rate-card', {
            method: 'PUT',
            body: JSON.stringify({
                prompt_per_million: prompt,
                completion_per_million: completion,
            }),
        });
        toast('费率卡保存成功', 'success');
    } catch (e) {
        toast('费率卡保存失败: ' + e.message, 'error');
    }
}

// ─── API Helper ──────────────────────────────────────────────────
async function api(path, options = {}) {
    const headers = { 'Content-Type': 'application/json' };
    const apiKey = getApiKey();
    if (apiKey) {
        headers['Authorization'] = 'Bearer ' + apiKey;
    }
    const res = await fetch(path, {
        headers,
        ...options
    });
    if (res.status === 401) {
        setApiKey('');
        clearAllIntervals();
        showLoginPrompt('认证失败，请重新输入 API Key');
        throw new Error('Authentication failed');
    }
    if (!res.ok) {
        const text = await res.text();
        throw new Error(text || `HTTP ${res.status}`);
    }
    return res.json();
}

// ─── Toast ───────────────────────────────────────────────────────
function toast(message, type = 'info') {
    const container = document.getElementById('toast-container');
    const el = document.createElement('div');
    el.className = `toast ${type}`;
    el.textContent = message;
    container.appendChild(el);
    setTimeout(() => el.remove(), 3500);
}

// ─── Dashboard ───────────────────────────────────────────────────
async function loadDashboard() {
    try {
        const m = await api('/api/admin/metrics');

        document.getElementById('stat-requests').textContent = m.total_requests.toLocaleString();
        document.getElementById('stat-prompt-tokens').textContent = m.total_prompt_tokens.toLocaleString();
        document.getElementById('stat-completion-tokens').textContent = m.total_completion_tokens.toLocaleString();
        document.getElementById('stat-errors').textContent = m.total_errors.toLocaleString();
        document.getElementById('stat-providers').textContent = m.providers_count;
        document.getElementById('stat-models').textContent = m.models_count;

        // System status
        document.getElementById('status-cache').textContent = m.cache_enabled ? '已启用' : '已禁用';
        document.getElementById('status-cache').className = `badge ${m.cache_enabled ? 'enabled' : 'disabled'}`;
        document.getElementById('status-cache-size').textContent = m.cache_size;
        document.getElementById('status-ratelimit').textContent = m.rate_limit_rpm;
        document.getElementById('status-auth').textContent = m.auth_enabled ? '已启用' : '已禁用';
        document.getElementById('status-auth').className = `badge ${m.auth_enabled ? 'enabled' : 'disabled'}`;
        document.getElementById('status-keys').textContent = m.api_keys_count;

        // Model chart
        renderModelChart(m.per_model);
    } catch (e) {
        toast('加载仪表盘失败: ' + e.message, 'error');
    }
}

function renderModelChart(perModel) {
    const container = document.getElementById('model-chart');
    const entries = Object.entries(perModel);

    if (entries.length === 0) {
        container.innerHTML = '<div class="empty-state">暂无数据</div>';
        return;
    }

    const maxVal = Math.max(...entries.map(e => e[1]), 1);
    const colors = ['#6366f1', '#8b5cf6', '#a78bfa', '#c084fc', '#e879f9'];

    container.innerHTML = entries.map(([model, count], i) => {
        const pct = (count / maxVal) * 100;
        return `
            <div class="chart-bar">
                <span class="chart-bar-label" title="${model}">${model}</span>
                <div class="chart-bar-track">
                    <div class="chart-bar-fill" style="width: ${pct}%; background: ${colors[i % colors.length]}"></div>
                </div>
                <span class="chart-bar-value">${count.toLocaleString()}</span>
            </div>
        `;
    }).join('');
}

// ─── Tenants ────────────────────────────────────────────────────
async function loadTenants() {
    try {
        const tenants = await api('/api/admin/tenants');
        const tbody = document.querySelector('#tenants-table tbody');

        if (tenants.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="empty-state">暂无租户</td></tr>';
            return;
        }

        tbody.innerHTML = tenants.map(t => `
            <tr>
                <td><strong>${escHtml(t.id)}</strong></td>
                <td>${t.quotas.max_rpm}</td>
                <td>${t.quotas.max_rpd.toLocaleString()}</td>
                <td>${t.quotas.max_tpd.toLocaleString()}</td>
                <td class="actions">
                    ${escHtml(t.id) !== 'default' ? `<button class="btn btn-danger btn-sm" onclick="deleteTenant('${escHtml(t.id)}')">删除</button>` : '<span class="apikey-unset">系统保留</span>'}
                </td>
            </tr>
        `).join('');

        // Populate tenant dropdown in key form
        const tenantSelect = document.getElementById('new-key-tenant');
        if (tenantSelect) {
            tenantSelect.innerHTML = '<option value="">默认</option>' +
                tenants.map(t => `<option value="${escHtml(t.id)}">${escHtml(t.id)}</option>`).join('');
        }
    } catch (e) {
        toast('加载租户失败: ' + e.message, 'error');
    }
}

function showAddTenantModal() {
    document.getElementById('tenant-form').reset();
    openModal('tenant-modal');
}

async function deleteTenant(id) {
    if (!confirm(`确定要删除租户 "${id}" 吗？`)) return;
    try {
        await api(`/api/admin/tenants/${encodeURIComponent(id)}`, { method: 'DELETE' });
        toast('租户已删除', 'success');
        loadTenants();
    } catch (e) {
        toast('删除失败: ' + e.message, 'error');
    }
}

document.getElementById('tenant-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const id = document.getElementById('tenant-id').value.trim();
    const rpm = parseInt(document.getElementById('tenant-rpm').value) || 60;
    if (!id) { toast('请输入租户 ID', 'error'); return; }

    try {
        await api('/api/admin/tenants', {
            method: 'POST',
            body: JSON.stringify({
                id,
                quotas: { max_rpm: rpm, max_rpd: 10000, max_tpm: 500000, max_tpd: 5000000 }
            })
        });
        toast('租户已添加', 'success');
        closeModal('tenant-modal');
        loadTenants();
    } catch (e) {
        toast('添加失败: ' + e.message, 'error');
    }
});

// ─── Providers ────────────────────────────────────────────────────
async function loadProviders() {
    try {
        const providers = await api('/api/admin/providers');
        const tbody = document.querySelector('#providers-table tbody');

        if (providers.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="empty-state">暂无提供商，点击"添加提供商"开始</td></tr>';
            return;
        }

        tbody.innerHTML = providers.map(p => `
            <tr>
                <td><strong>${escHtml(p.name)}</strong></td>
                <td>${p.base_url ? escHtml(p.base_url) : '<span class="apikey-unset">默认</span>'}</td>
                <td>${p.api_key_set ? '<span class="apikey-set">● 已配置</span>' : '<span class="apikey-unset">未配置</span>'}</td>
                <td>${p.models.map(m => `<span class="model-tag">${escHtml(m)}</span>`).join('')}</td>
                <td class="actions">
                    <button class="btn btn-secondary btn-sm" onclick="editProvider('${escHtml(p.name)}')">编辑</button>
                    <button class="btn btn-danger btn-sm" onclick="deleteProvider('${escHtml(p.name)}')">删除</button>
                </td>
            </tr>
        `).join('');
    } catch (e) {
        toast('加载提供商失败: ' + e.message, 'error');
    }
}

function showAddProviderModal() {
    document.getElementById('provider-modal-title').textContent = '添加提供商';
    document.getElementById('provider-form').reset();
    document.getElementById('provider-name').disabled = false;
    openModal('provider-modal');
}

async function editProvider(name) {
    try {
        const p = await api(`/api/admin/providers/${encodeURIComponent(name)}`);
        document.getElementById('provider-modal-title').textContent = '编辑提供商';
        document.getElementById('provider-name').value = p.name;
        document.getElementById('provider-name').disabled = true;
        document.getElementById('provider-apikey').value = '';
        document.getElementById('provider-apikey').placeholder = p.api_key_set ? '留空保持不变' : '输入 API Key';
        document.getElementById('provider-baseurl').value = p.base_url || '';
        document.getElementById('provider-models').value = p.models.join(', ');
        openModal('provider-modal');
    } catch (e) {
        toast('加载提供商详情失败: ' + e.message, 'error');
    }
}

async function deleteProvider(name) {
    if (!confirm(`确定要删除提供商 "${name}" 吗？其下所有模型将被移除。`)) return;
    try {
        await api(`/api/admin/providers/${encodeURIComponent(name)}`, { method: 'DELETE' });
        toast('提供商已删除', 'success');
        loadProviders();
    } catch (e) {
        toast('删除失败: ' + e.message, 'error');
    }
}

document.getElementById('provider-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const name = document.getElementById('provider-name').value.trim();
    const apiKey = document.getElementById('provider-apikey').value.trim() || null;
    const baseUrl = document.getElementById('provider-baseurl').value.trim() || null;
    const models = document.getElementById('provider-models').value
        .split(',').map(s => s.trim()).filter(Boolean);

    if (!name || models.length === 0) {
        toast('请填写名称和模型列表', 'error');
        return;
    }

    const isEdit = document.getElementById('provider-name').disabled;
    const url = isEdit
        ? `/api/admin/providers/${encodeURIComponent(name)}`
        : '/api/admin/providers';
    const method = isEdit ? 'PUT' : 'POST';

    const body = isEdit ? {} : { name, api_key: apiKey, base_url: baseUrl, models };
    if (isEdit) {
        if (apiKey) body.api_key = apiKey;
        if (baseUrl) body.base_url = baseUrl;
        body.models = models;
    }

    try {
        await api(url, {
            method,
            body: JSON.stringify(body)
        });
        toast(isEdit ? '提供商已更新' : '提供商已添加', 'success');
        closeModal('provider-modal');
        loadProviders();
    } catch (e) {
        toast('保存失败: ' + e.message, 'error');
    }
});

// ─── API Keys ─────────────────────────────────────────────────────
async function loadApiKeys() {
    try {
        const data = await api('/api/admin/keys');
        document.getElementById('auth-status-text').textContent = data.enabled ? '已启用' : '已禁用';
        document.getElementById('auth-status-text').className = `badge ${data.enabled ? 'enabled' : 'disabled'}`;

        const tbody = document.querySelector('#keys-table tbody');
        if (data.keys.length === 0) {
            tbody.innerHTML = '<tr><td colspan="2" class="empty-state">暂无 API Key</td></tr>';
            return;
        }

        tbody.innerHTML = data.keys.map(k => `
            <tr>
                <td><span class="key-masked">${escHtml(k)}</span></td>
                <td>
                    <button class="btn btn-danger btn-sm" onclick="deleteKey('${escHtml(k)}')">删除</button>
                </td>
            </tr>
        `).join('');
    } catch (e) {
        toast('加载 API Keys 失败: ' + e.message, 'error');
    }
}

function showAddKeyModal() {
    document.getElementById('key-form').reset();
    openModal('key-modal');
}

async function deleteKey(key) {
    if (!confirm('确定要删除此 API Key 吗？')) return;
    try {
        await api(`/api/admin/keys/${encodeURIComponent(key)}`, { method: 'DELETE' });
        toast('API Key 已删除', 'success');
        loadApiKeys();
    } catch (e) {
        toast('删除失败: ' + e.message, 'error');
    }
}

document.getElementById('key-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const key = document.getElementById('new-key-value').value.trim();
    if (!key) { toast('请输入 API Key 值', 'error'); return; }

    try {
        await api('/api/admin/keys', {
            method: 'POST',
            body: JSON.stringify({ key })
        });
        toast('API Key 已添加', 'success');
        closeModal('key-modal');
        loadApiKeys();
    } catch (e) {
        toast('添加失败: ' + e.message, 'error');
    }
});

// ─── Config ───────────────────────────────────────────────────────
async function loadConfig() {
    try {
        const m = await api('/api/admin/metrics');
        document.getElementById('cache-enabled').checked = m.cache_enabled;
        document.getElementById('rpm-value').textContent = m.rate_limit_rpm;
    } catch (e) {
        toast('加载配置失败: ' + e.message, 'error');
    }
}

document.getElementById('cache-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
        await api('/api/admin/config/cache', {
            method: 'PUT',
            body: JSON.stringify({
                enabled: document.getElementById('cache-enabled').checked,
                max_capacity: parseInt(document.getElementById('cache-capacity').value) || 1000,
                ttl_seconds: parseInt(document.getElementById('cache-ttl').value) || 300
            })
        });
        toast('缓存配置已更新', 'success');
    } catch (e) {
        toast('保存失败: ' + e.message, 'error');
    }
});

document.getElementById('ratelimit-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    try {
        await api('/api/admin/config/rate-limit', {
            method: 'PUT',
            body: JSON.stringify({
                requests_per_minute: parseInt(document.getElementById('rpm-value').value) || 60
            })
        });
        toast('限流配置已更新', 'success');
    } catch (e) {
        toast('保存失败: ' + e.message, 'error');
    }
});

// ─── Logs ─────────────────────────────────────────────────────────
async function loadLogs() {
    try {
        const logs = await api('/api/admin/logs');
        renderLogs(logs);
    } catch (e) {
        toast('加载日志失败: ' + e.message, 'error');
    }
}

function renderLogs(logs) {
    const container = document.getElementById('log-container');
    if (logs.length === 0) {
        container.innerHTML = '<div class="log-empty">等待日志...</div>';
        return;
    }

    container.innerHTML = logs.map(log => `
        <div class="log-entry">
            <span class="log-time">${escHtml(log.timestamp)}</span>
            <span class="log-level ${escHtml(log.level)}">${escHtml(log.level)}</span>
            <span class="log-message">${escHtml(log.message)}</span>
        </div>
    `).join('');

    container.scrollTop = container.scrollHeight;
}

function clearLogs() {
    document.getElementById('log-container').innerHTML = '<div class="log-empty">日志已清空</div>';
}

function refreshLogs() {
    loadLogs();
}

function toggleAutoRefresh() {
    const checkbox = document.getElementById('auto-refresh-logs');
    if (checkbox.checked) {
        startAutoRefreshLogs();
    } else {
        stopAutoRefreshLogs();
    }
}

function startAutoRefreshLogs() {
    if (autoRefreshLogsInterval) return;
    autoRefreshLogsInterval = setInterval(loadLogs, 2000);
}

function stopAutoRefreshLogs() {
    if (autoRefreshLogsInterval) {
        clearInterval(autoRefreshLogsInterval);
        autoRefreshLogsInterval = null;
    }
}

// ─── Modal ───────────────────────────────────────────────────────
function openModal(id) {
    document.getElementById(id).classList.add('active');
}

function closeModal(id) {
    document.getElementById(id).classList.remove('active');
}

// Close modal on overlay click
document.querySelectorAll('.modal-overlay').forEach(overlay => {
    overlay.addEventListener('click', (e) => {
        if (e.target === overlay) overlay.classList.remove('active');
    });
});

// ─── Helpers ──────────────────────────────────────────────────────
function escHtml(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
}

function refreshCurrentPage() {
    loadPageData(currentPage);
    toast('已刷新', 'info');
}

// ─── Auto-refresh Dashboard ──────────────────────────────────────
function startAutoRefreshDashboard() {
    autoRefreshDashboardInterval = setInterval(() => {
        if (currentPage === 'dashboard') loadDashboard();
    }, 5000);
}

// ─── Init ─────────────────────────────────────────────────────────
if (getApiKey()) {
    navigateTo('dashboard');
    startAutoRefreshDashboard();
    startAutoRefreshLogs();
} else {
    showLoginPrompt();
}
