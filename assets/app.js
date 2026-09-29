const $ = (id) => document.getElementById(id);
const scopes = ["status", "since", "idle", "history", "summary"];
const scopeLabel = {
  status: "是否在线",
  since: "开始时间",
  idle: "空闲与锁屏",
  history: "时段明细",
  summary: "在线总时长",
};

function showError(message) {
  const node = $("error");
  node.hidden = !message;
  node.textContent = message || "";
}

async function api(path, options) {
  const response = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  if (response.status === 204) return null;
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || `请求失败 ${response.status}`);
  return body;
}

function clock(ms) {
  if (!ms) return "—";
  return new Date(ms).toLocaleString("zh-CN", { hour12: false });
}

function duration(ms) {
  const total = Math.max(0, Math.round(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (hours) return `${hours}小时${minutes}分`;
  if (minutes) return `${minutes}分`;
  return `${total}秒`;
}

function idleText(seconds) {
  if (seconds < 5) return "刚刚有操作";
  return duration(seconds * 1000);
}

function todayValue() {
  const now = new Date();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  return `${now.getFullYear()}-${month}-${day}`;
}

async function refreshStatus() {
  const status = await api("/api/status");
  const online = status.online;
  $("pill").textContent = online ? "在线" : "离开";
  $("pill").className = `pill ${online ? "online" : "away"}`;
  $("headline").textContent = online ? "在电脑前" : "不在电脑前";
  $("headline").className = `headline ${online ? "online" : "away"}`;
  $("idle").textContent = idleText(status.idle_secs);
  $("since").textContent = clock(status.since_ms);
  $("today").textContent = duration(status.today_online_secs * 1000);
  $("locked").textContent = status.locked ? "已锁屏" : "未锁屏";
}

async function loadPolicy() {
  const policy = await api("/api/policy");
  $("idle-threshold").value = policy.idle_threshold_secs;
  $("locked-offline").checked = policy.locked_is_offline;
  $("confirm-secs").value = policy.online_confirm_secs;
}

async function loadHistory() {
  const from = $("from-date").value;
  const to = $("to-date").value;
  const data = await api(`/api/sessions?from=${from}&to=${to}`);
  const summary = await api(`/api/summary?from=${from}&to=${to}`);
  const online = data.segments.filter((segment) => segment.state === "online");
  $("history-total").textContent = `在线 ${duration(summary.online_secs * 1000)}，${summary.segment_count} 段`;
  const list = $("history-list");
  list.replaceChildren();
  if (!online.length) {
    const item = document.createElement("li");
    item.textContent = "这段时间没有在线记录";
    list.append(item);
    return;
  }
  for (const segment of online) {
    const end = segment.ended_at ?? Date.now();
    const item = document.createElement("li");
    const title = document.createElement("span");
    title.textContent = `${clock(segment.started_at)} – ${segment.ended_at ? clock(segment.ended_at) : "进行中"}`;
    const meta = document.createElement("span");
    meta.className = "meta";
    meta.textContent = duration(end - segment.started_at);
    item.append(title, meta);
    list.append(item);
  }
}

function selectedScopes(root) {
  return [...root.querySelectorAll('input[name="scope"]:checked')].map((node) => node.value);
}

function scopeChecks(selected) {
  return scopes.map((scope) => {
    const checked = selected.includes(scope) ? "checked" : "";
    return `<label class="check"><input type="checkbox" name="scope" value="${scope}" ${checked}> ${scopeLabel[scope]}</label>`;
  }).join("");
}

async function loadClients() {
  const clients = await api("/api/mcp/clients");
  const box = $("clients");
  box.replaceChildren();
  const select = $("query-client");
  const current = select.value;
  select.replaceChildren(new Option("全部", ""));
  for (const client of clients) {
    select.append(new Option(client.name, client.id));
    const card = document.createElement("article");
    card.className = `client${client.revoked_at ? " revoked" : ""}`;
    card.innerHTML = `
      <header>
        <strong></strong>
        <span class="meta"></span>
      </header>
      <div class="checks"></div>
      <div class="row"></div>
    `;
    card.querySelector("strong").textContent = client.name;
    card.querySelector(".meta").textContent = client.revoked_at ? "已撤销" : `创建于 ${clock(client.created_at)}`;
    card.querySelector(".checks").innerHTML = scopeChecks(client.scopes);
    if (!client.revoked_at) {
      const save = document.createElement("button");
      save.type = "button";
      save.textContent = "保存范围";
      save.addEventListener("click", async () => {
        try {
          await api(`/api/mcp/clients/${client.id}`, {
            method: "PATCH",
            body: JSON.stringify({ scopes: selectedScopes(card) }),
          });
          showError("");
          await loadClients();
        } catch (error) {
          showError(error.message);
        }
      });
      const revoke = document.createElement("button");
      revoke.type = "button";
      revoke.textContent = "撤销";
      revoke.addEventListener("click", async () => {
        if (!confirm(`撤销 ${client.name}？它的 token 会立刻失效。`)) return;
        try {
          await api(`/api/mcp/clients/${client.id}/revoke`, { method: "POST" });
          await loadClients();
          await loadQueries();
        } catch (error) {
          showError(error.message);
        }
      });
      card.querySelector(".row").append(save, revoke);
    }
    box.append(card);
  }
  if ([...select.options].some((option) => option.value === current)) select.value = current;
}

async function loadQueries() {
  const clientId = $("query-client").value;
  const query = clientId ? `?client_id=${encodeURIComponent(clientId)}` : "";
  const data = await api(`/api/mcp/queries${query}`);
  const list = $("query-list");
  list.replaceChildren();
  if (!data.queries.length) {
    const item = document.createElement("li");
    item.textContent = "还没有查询";
    list.append(item);
    return;
  }
  for (const row of data.queries) {
    const item = document.createElement("li");
    const title = document.createElement("span");
    title.textContent = `${row.client_name || "未知"} · ${row.tool}`;
    const meta = document.createElement("span");
    meta.className = "meta";
    meta.textContent = `${row.allowed ? "允许" : "拒绝"} · ${row.result_summary} · ${clock(row.created_at)}`;
    item.append(title, meta);
    list.append(item);
  }
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((node) => node.classList.toggle("is-on", node === tab));
    document.querySelectorAll(".panel").forEach((panel) => {
      panel.hidden = panel.id !== tab.dataset.tab;
    });
    if (tab.dataset.tab === "history") loadHistory().catch((error) => showError(error.message));
    if (tab.dataset.tab === "access") {
      loadClients().then(loadQueries).catch((error) => showError(error.message));
    }
    if (tab.dataset.tab === "policy") loadPolicy().catch((error) => showError(error.message));
  });
});

$("policy-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await api("/api/policy", {
      method: "PUT",
      body: JSON.stringify({
        idle_threshold_secs: Number($("idle-threshold").value),
        locked_is_offline: $("locked-offline").checked,
        online_confirm_secs: Number($("confirm-secs").value),
      }),
    });
    $("policy-note").textContent = "已保存，下一次采样就会用新策略";
    showError("");
  } catch (error) {
    showError(error.message);
  }
});

$("history-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await loadHistory();
    showError("");
  } catch (error) {
    showError(error.message);
  }
});

$("client-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const created = await api("/api/mcp/clients", {
      method: "POST",
      body: JSON.stringify({
        name: $("client-name").value,
        scopes: selectedScopes($("client-form")),
      }),
    });
    const config = {
      mcpServers: {
        awake: {
          url: `${location.origin}/mcp`,
          headers: { Authorization: `Bearer ${created.token}` },
        },
      },
    };
    $("token-config").value = JSON.stringify(config, null, 2);
    $("token-box").hidden = false;
    $("client-name").value = "";
    showError("");
    await loadClients();
    await loadQueries();
  } catch (error) {
    showError(error.message);
  }
});

$("copy-token").addEventListener("click", async () => {
  await navigator.clipboard.writeText($("token-config").value);
  $("copy-token").textContent = "已复制";
});

$("query-client").addEventListener("change", () => {
  loadQueries().catch((error) => showError(error.message));
});

$("from-date").value = todayValue();
$("to-date").value = todayValue();

refreshStatus().catch((error) => showError(error.message));
loadPolicy().catch((error) => showError(error.message));
setInterval(() => {
  refreshStatus().catch((error) => showError(error.message));
}, 2000);
