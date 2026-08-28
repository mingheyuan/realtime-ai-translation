const stack = document.querySelector("#caption-stack");
const waiting = document.querySelector("#waiting-card");
const statusLabel = document.querySelector("#overlay-status");
const startButton = document.querySelector("#overlay-start");
const stopButton = document.querySelector("#overlay-stop");
const asrSelect = document.querySelector("#overlay-asr");
const audioSelect = document.querySelector("#overlay-audio");
const directionButton = document.querySelector("#overlay-direction");

let socket;
let reconnectTimer;
let sessionRunning = false;
let activeSegmentId = null;
let historySegmentId = null;
let historySealTimer;
const segmentVersions = new Map();
let preferences = {
  source_language: "zh-CN",
  target_language: "en-US",
  audio_source: "microphone",
  asr_engine: "apple_speech",
};

function setRunning(running) {
  sessionRunning = running;
  startButton.disabled = running;
  stopButton.disabled = !running;
  asrSelect.disabled = running;
  audioSelect.disabled = running;
  directionButton.disabled = running;
  document.body.classList.toggle("session-running", running);
}

function updateSettingsControls() {
  asrSelect.value = preferences.asr_engine;
  audioSelect.value = preferences.audio_source;
  directionButton.textContent = preferences.source_language.startsWith("zh")
    ? "中 → 英"
    : "英 → 中";
}

function selectedAsrLabel() {
  const option = asrSelect.selectedOptions[0];
  return option?.dataset.label || option?.textContent.replace("（未配置）", "") || "语音识别";
}

async function loadAsrProviders() {
  try {
    const health = await jsonRequest("/api/health");
    for (const option of asrSelect.options) {
      const provider = health.asr_providers?.find((item) => item.id === option.value);
      if (!provider) continue;
      option.dataset.label = provider.label;
      option.textContent = provider.available ? provider.label : `${provider.label}（未配置）`;
      option.disabled = !provider.available;
    }
  } catch (error) {
    statusLabel.textContent = error.message;
  }
}

async function jsonRequest(url, options = {}) {
  const response = await fetch(url, {
    ...options,
    headers: { "content-type": "application/json", ...(options.headers || {}) },
  });
  const body = response.status === 204 ? null : await response.json();
  if (!response.ok) throw new Error(body?.error || `请求失败（${response.status}）`);
  return body;
}

function delay(milliseconds) {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

async function waitForSessionStopped() {
  for (let attempt = 0; attempt < 45; attempt += 1) {
    const state = await jsonRequest("/api/overlay/state");
    if (!state.running) return;
    if (attempt === 10) statusLabel.textContent = "正在完成当前句终稿…";
    await delay(200);
  }
  throw new Error("停止超时，请检查服务状态");
}

async function refreshOverlayState() {
  try {
    const state = await jsonRequest("/api/overlay/state");
    preferences = state.preferences;
    updateSettingsControls();
    setRunning(state.running);
    if (state.running) statusLabel.textContent = "实时翻译中";
  } catch (error) {
    statusLabel.textContent = error.message;
  }
}

function connect() {
  window.clearTimeout(reconnectTimer);
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${protocol}//${location.host}/ws`);
  socket.addEventListener("open", () => {
    document.body.classList.add("connected");
    document.body.classList.remove("disconnected");
    statusLabel.textContent = sessionRunning ? "实时翻译中" : "已连接 · 等待开始";
  });
  socket.addEventListener("close", () => {
    document.body.classList.remove("connected");
    document.body.classList.add("disconnected");
    statusLabel.textContent = "连接中断 · 正在重连";
    reconnectTimer = window.setTimeout(connect, 1200);
  });
  socket.addEventListener("message", ({ data }) => {
    try {
      handleEvent(JSON.parse(data));
    } catch {
      // Ignore malformed or unrelated WebSocket messages.
    }
  });
}

function handleEvent(event) {
  if (event.type === "caption") renderCaption(event);
  if (event.type === "session_status") {
    if (event.running && !sessionRunning) resetCaptions();
    setRunning(event.running);
    statusLabel.textContent = event.message;
  }
  if (event.type === "error") statusLabel.textContent = event.message;
}

function resetCaptions() {
  window.clearTimeout(historySealTimer);
  activeSegmentId = null;
  historySegmentId = null;
  segmentVersions.clear();
  for (const card of stack.querySelectorAll(".caption-card:not(.waiting)")) card.remove();
  waiting.hidden = false;
}

function createCard(segmentId) {
  const card = document.createElement("article");
  card.className = "caption-card current";
  card.dataset.segment = segmentId;
  card.innerHTML = `
    <div class="caption-meta"><span class="phase">当前段 · ASR 原文</span><span class="latency">实时</span></div>
    <p class="source"></p>
    <p class="translation">正在生成快速译文…</p>`;
  return card;
}

function archiveCurrent() {
  const current = stack.querySelector(".caption-card.current");
  if (!current || activeSegmentId === null) return;
  window.clearTimeout(historySealTimer);
  stack.querySelector(".caption-card.history")?.remove();
  historySegmentId = activeSegmentId;
  activeSegmentId = null;
  current.className = "caption-card history";
  stack.prepend(current);
  if (current.dataset.state === "final") {
    sealHistory(current, finalHistoryPhase(current));
  } else {
    markHistoryFinalizing(current);
  }
}

function markHistoryFinalizing(card) {
  card.dataset.sealed = "false";
  card.classList.add("finalizing");
  card.classList.remove("translation-pending");
  card.querySelector(".phase").textContent = "上一段 · LLM 终稿处理中";
  card.querySelector(".latency").textContent = "最长 3 秒";
  const pendingSegmentId = historySegmentId;
  historySealTimer = window.setTimeout(() => {
    if (historySegmentId !== pendingSegmentId || card.dataset.sealed === "true") return;
    sealHistory(card, "上一段 · 快速译文 · 已封存");
  }, 3000);
}

function finalHistoryPhase(card) {
  return card.dataset.llmApplied === "true"
    ? "上一段 · LLM 终稿 · 已封存"
    : "上一段 · 最终译文 · 已封存";
}

function sealHistory(card, phase) {
  window.clearTimeout(historySealTimer);
  card.dataset.sealed = "true";
  card.dataset.state = "history";
  card.classList.remove("finalizing", "translation-pending");
  card.querySelector(".phase").textContent = phase;
  card.querySelector(".latency").textContent = "";
}

function cardForEvent(event) {
  if (event.segment_id === historySegmentId) {
    const history = stack.querySelector(".caption-card.history");
    return history?.dataset.sealed !== "true" && event.state === "final"
      ? history
      : null;
  }
  if (event.segment_id === activeSegmentId) {
    return stack.querySelector(".caption-card.current");
  }
  if (activeSegmentId !== null && event.segment_id > activeSegmentId) archiveCurrent();
  const newestVisibleId = Math.max(activeSegmentId ?? -1, historySegmentId ?? -1);
  if (event.segment_id <= newestVisibleId) return null;

  const card = createCard(event.segment_id);
  activeSegmentId = event.segment_id;
  stack.append(card);
  return card;
}

function renderCaption(event) {
  if (!sessionRunning) {
    setRunning(true);
    statusLabel.textContent = "实时翻译中";
  }
  const knownRevision = segmentVersions.get(event.segment_id) ?? -1;
  if (knownRevision >= event.revision) return;
  segmentVersions.set(event.segment_id, event.revision);

  const card = cardForEvent(event);
  if (!card) return;
  const finalizingHistory = event.segment_id === historySegmentId;
  waiting.hidden = true;

  card.dataset.state = event.state;
  card.dataset.llmApplied = String(event.llm_applied);

  const source = card.querySelector(".source");
  const translation = card.querySelector(".translation");
  const receivedTranslation = event.translation_text.trim().length > 0;
  const hasVisibleTranslation = translation.dataset.hasTranslation === "true";
  const holdTranslation = event.state === "partial" && !receivedTranslation && hasVisibleTranslation;

  source.textContent = event.source_text;
  if (receivedTranslation) {
    translation.textContent = event.translation_text;
    translation.dataset.hasTranslation = "true";
  } else if (!hasVisibleTranslation) {
    translation.textContent = "正在生成快速译文…";
  }
  card.classList.toggle("translation-pending", holdTranslation);

  const phases = {
    partial: holdTranslation ? "当前段 · ASR 更新" : "当前段 · ASR 原文",
    draft: "当前段 · 快速译文",
    final: event.llm_applied ? "当前段 · LLM 终稿" : "当前段 · 最终译文",
  };
  card.querySelector(".phase").textContent = finalizingHistory
    ? event.llm_applied
      ? "上一段 · LLM 终稿"
      : "上一段 · 最终译文"
    : phases[event.state];
  card.querySelector(".latency").textContent = holdTranslation
    ? "等待新译文"
    : event.latency_ms
      ? `${event.latency_ms} ms`
      : "实时";

  if (event.state === "final" && finalizingHistory) {
    sealHistory(card, finalHistoryPhase(card));
  }
}

startButton.addEventListener("click", async () => {
  startButton.disabled = true;
  statusLabel.textContent = `正在按需加载 ${selectedAsrLabel()}…`;
  try {
    const state = await jsonRequest("/api/overlay/state");
    preferences = state.preferences;
    updateSettingsControls();
    await jsonRequest("/api/session/start", {
      method: "POST",
      body: JSON.stringify(preferences),
    });
    setRunning(true);
  } catch (error) {
    setRunning(false);
    statusLabel.textContent = error.message;
  }
});

async function savePreferences(nextPreferences) {
  asrSelect.disabled = true;
  audioSelect.disabled = true;
  directionButton.disabled = true;
  try {
    const state = await jsonRequest("/api/overlay/state", {
      method: "POST",
      body: JSON.stringify(nextPreferences),
    });
    preferences = state.preferences;
    updateSettingsControls();
    statusLabel.textContent = "设置已更新 · 可以开始";
  } catch (error) {
    updateSettingsControls();
    statusLabel.textContent = error.message;
  } finally {
    asrSelect.disabled = sessionRunning;
    audioSelect.disabled = sessionRunning;
    directionButton.disabled = sessionRunning;
  }
}

asrSelect.addEventListener("change", () => {
  savePreferences({ ...preferences, asr_engine: asrSelect.value });
});

audioSelect.addEventListener("change", () => {
  savePreferences({ ...preferences, audio_source: audioSelect.value });
});

directionButton.addEventListener("click", () => {
  const chineseSource = preferences.source_language.startsWith("zh");
  savePreferences({
    ...preferences,
    source_language: chineseSource ? "en-US" : "zh-CN",
    target_language: chineseSource ? "zh-CN" : "en-US",
  });
});

stopButton.addEventListener("click", async () => {
  stopButton.disabled = true;
  statusLabel.textContent = "正在结束当前句…";
  try {
    await jsonRequest("/api/session/stop", { method: "POST" });
    await waitForSessionStopped();
    setRunning(false);
    statusLabel.textContent = "已停止 · 可以重新开始";
  } catch (error) {
    await refreshOverlayState();
    if (sessionRunning) statusLabel.textContent = error.message;
  }
});

updateSettingsControls();
loadAsrProviders();
refreshOverlayState();
connect();
