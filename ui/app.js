/**
 * REPLAYR INSPECTOR UI
 * Lightweight request inspector for the Replayr proxy
 *
 * No build step, no npm, vanilla JS with modern browser APIs
 */

(function() {
  'use strict';

  // ==========================================================================
  // STATE
  // ==========================================================================

  const state = {
    requests: [],
    markedIds: new Set(),
    selectedId: null,
    filter: '',
    isRecording: false,
    interceptPattern: null,
    interceptQueue: [],
    wsConnected: false,
    maxBufferSize: 1000
  };

  // ==========================================================================
  // DOM REFERENCES
  // ==========================================================================

  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => document.querySelectorAll(sel);

  const dom = {
    requestList: $('#requestList'),
    requestCount: $('#requestCount'),
    emptyState: $('#emptyState'),
    detailPanel: $('#detailPanel'),
    detailContent: $('#detailContent'),
    filterInput: $('#filterInput'),
    clearFilter: $('#clearFilter'),
    toggleRecord: $('#toggleRecord'),
    clearBuffer: $('#clearBuffer'),
    saveAll: $('#saveAll'),
    markAll: $('#markAll'),
    saveMarked: $('#saveMarked'),
    recordingLed: $('#recordingLed'),
    interceptLed: $('#interceptLed'),
    bufferCount: $('#bufferCount'),
    recordingStatus: $('#recordingStatus'),
    interceptStatus: $('#interceptStatus'),
    interceptPattern: $('#interceptPattern'),
    scenarioStep: $('#scenarioStep'),
    toastContainer: $('#toastContainer'),
    interceptModal: $('#interceptModal'),
    waveformCanvas: $('#waveformCanvas')
  };

  // ==========================================================================
  // WEBSOCKET CONNECTION
  // ==========================================================================

  let ws = null;
  let reconnectAttempts = 0;
  const maxReconnectAttempts = 10;
  const reconnectDelay = 2000;

  function connectWebSocket() {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const wsUrl = `${protocol}//${window.location.host}/api/v1/ws`;

    try {
      ws = new WebSocket(wsUrl);

      ws.onopen = () => {
        console.log('[Replayr] WebSocket connected');
        reconnectAttempts = 0;
        updateConnectionStatus(true);
      };

      ws.onclose = () => {
        console.log('[Replayr] WebSocket disconnected');
        updateConnectionStatus(false);
        scheduleReconnect();
      };

      ws.onerror = (err) => {
        console.error('[Replayr] WebSocket error:', err);
      };

      ws.onmessage = (event) => {
        try {
          const message = JSON.parse(event.data);
          handleWebSocketMessage(message);
        } catch (e) {
          console.error('[Replayr] Failed to parse message:', e);
        }
      };
    } catch (e) {
      console.error('[Replayr] Failed to connect WebSocket:', e);
      scheduleReconnect();
    }
  }

  function scheduleReconnect() {
    if (reconnectAttempts < maxReconnectAttempts) {
      reconnectAttempts++;
      setTimeout(connectWebSocket, reconnectDelay * reconnectAttempts);
    }
  }

  function updateConnectionStatus(connected) {
    state.wsConnected = connected;
    const led = $('.status-led[title="WebSocket Connected"]');
    if (led) {
      led.dataset.status = connected ? 'connected' : 'error';
    }
  }

  function handleWebSocketMessage(message) {
    // Replayr backend currently broadcasts raw interaction objects.
    if (message && message.request && message.response) {
      addRequest(message);
      return;
    }

    switch (message.type) {
      case 'interaction':
        addRequest(message.data);
        break;
      case 'intercepted':
        handleInterceptedRequest(message.data);
        break;
      case 'status':
        updateStatus(message.data);
        break;
      default:
        console.log('[Replayr] Unknown message type:', message.type);
    }

    refreshInterceptQueue(false).catch(() => {});
  }

  // ==========================================================================
  // REQUEST MANAGEMENT
  // ==========================================================================

  async function refreshRequestsFromServer() {
    const query = state.filter.trim()
      ? `?filter=${encodeURIComponent(state.filter.trim())}`
      : '';

    const response = await fetch(`/api/v1/requests${query}`);
    if (!response.ok) {
      throw new Error(`failed to fetch requests: ${response.status}`);
    }

    state.requests = await response.json();
    renderRequestList();
    updateBufferCount();

    if (state.selectedId) {
      const selected = state.requests.find(r => r.id === state.selectedId);
      if (selected) {
        showRequestDetail(selected);
      } else {
        state.selectedId = null;
        hideDetail();
      }
    }
  }

  function addRequest(interaction) {
    if (state.filter.trim()) {
      refreshRequestsFromServer().catch((err) => {
        console.warn('[Replayr] Failed to refresh filtered requests', err);
      });
      return;
    }

    // Add to beginning of array (newest first)
    state.requests.unshift(interaction);

    // Trim to max buffer size
    if (state.requests.length > state.maxBufferSize) {
      state.requests = state.requests.slice(0, state.maxBufferSize);
    }

    // Apply filter and render
    renderRequestList();
    updateBufferCount();
  }

  function getFilteredRequests() {
    return state.requests;
  }

  function renderRequestList() {
    const filtered = getFilteredRequests();
    dom.requestCount.textContent = filtered.length;

    if (filtered.length === 0) {
      dom.emptyState.style.display = 'flex';
      // Clear existing items except empty state
      Array.from(dom.requestList.children).forEach(child => {
        if (child !== dom.emptyState) child.remove();
      });
      return;
    }

    dom.emptyState.style.display = 'none';

    // For performance, we'll do a simple re-render
    // In production, you'd want virtual scrolling
    const fragment = document.createDocumentFragment();

    filtered.forEach(req => {
      const item = createRequestItem(req);
      fragment.appendChild(item);
    });

    // Clear and append
    dom.requestList.innerHTML = '';
    dom.requestList.appendChild(fragment);
  }

  function createRequestItem(req) {
    const item = document.createElement('div');
    item.className = 'request-item';
    item.dataset.id = req.id;

    if (state.selectedId === req.id) {
      item.classList.add('is-selected');
    }

    const isMarked = state.markedIds.has(req.id);
    const status = req.response?.status || 0;
    const statusClass = getStatusClass(status);
    const isStreaming = req.response?.streaming || false;

    item.innerHTML = `
      <div class="request-item__marker">
        <svg class="request-item__star ${isMarked ? 'is-marked' : ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M12 2l3.09 6.26L22 9.27l-5 4.87 1.18 6.88L12 17.77l-6.18 3.25L7 14.14 2 9.27l6.91-1.01L12 2z"/>
        </svg>
      </div>
      <div class="request-item__content">
        <div class="request-item__main">
          <span class="request-item__method request-item__method--${req.request.method.toLowerCase()}">${req.request.method}</span>
          <span class="request-item__path">${req.request.path}</span>
        </div>
        <div class="request-item__meta">
          <span class="request-item__status ${statusClass}">${status}</span>
          <span class="request-item__time">${formatDuration(req.metadata?.latency_ms)}</span>
          ${isStreaming ? `
            <span class="request-item__streaming">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M2 12h4l3-9 4 18 3-9h4"/>
              </svg>
              SSE
            </span>
          ` : ''}
          ${req.metadata?.provider ? `<span>${req.metadata.provider}</span>` : ''}
        </div>
      </div>
    `;

    // Event handlers
    item.addEventListener('click', (e) => {
      if (e.target.closest('.request-item__star')) {
        toggleMark(req.id);
        e.stopPropagation();
      } else {
        selectRequest(req.id);
      }
    });

    return item;
  }

  function getStatusClass(status) {
    if (status >= 200 && status < 300) return 'request-item__status--success';
    if (status >= 300 && status < 400) return 'request-item__status--redirect';
    if (status >= 400 && status < 500) return 'request-item__status--client-error';
    if (status >= 500) return 'request-item__status--server-error';
    return '';
  }

  function toggleMark(id) {
    if (state.markedIds.has(id)) {
      state.markedIds.delete(id);
    } else {
      state.markedIds.add(id);
    }
    renderRequestList();
  }

  function selectRequest(id) {
    state.selectedId = id;
    const request = state.requests.find(r => r.id === id);

    if (!request) return;

    // Update selection in list
    $$('.request-item').forEach(item => {
      item.classList.toggle('is-selected', item.dataset.id === id);
    });

    // Show detail panel
    showRequestDetail(request);
  }

  // ==========================================================================
  // DETAIL PANEL
  // ==========================================================================

  function showRequestDetail(req) {
    $('.detail-panel__empty').style.display = 'none';
    dom.detailContent.style.display = 'flex';

    // Header
    $('#detailMethod').textContent = req.request.method;
    $('#detailMethod').className = `detail-method`;

    $('#detailPath').textContent = req.request.path;

    const status = req.response?.status || 0;
    $('#detailStatus').textContent = status;
    $('#detailStatus').className = `detail-status ${status >= 400 ? 'detail-status--error' : 'detail-status--success'}`;

    $('#detailTime').textContent = formatDuration(req.metadata?.latency_ms);
    $('#detailSize').textContent = formatBytes(getResponseSize(req));
    $('#detailProvider').textContent = req.metadata?.provider || '—';

    // Request headers & body
    $('#requestHeaders').innerHTML = highlightHeaders(req.request.headers);
    $('#requestBody').innerHTML = highlightJSON(req.request.body);

    // Response headers & body
    $('#responseHeaders').innerHTML = highlightHeaders(req.response?.headers);

    if (req.response?.streaming && req.response?.chunks) {
      // Combine chunks for display
      const combinedBody = req.response.chunks.map(c => c.data).join('');
      $('#responseBody').innerHTML = highlightSSE(combinedBody);
      renderTimingView(req);
    } else {
      $('#responseBody').innerHTML = highlightJSON(req.response?.body);
    }
  }

  function highlightHeaders(headers) {
    if (!headers) return '<span class="json-null">null</span>';

    return Object.entries(headers)
      .map(([key, value]) => {
        return `<span class="json-key">${escapeHtml(key)}</span>: <span class="json-string">${escapeHtml(value)}</span>`;
      })
      .join('\n');
  }

  function highlightJSON(data) {
    if (data === null || data === undefined) {
      return '<span class="json-null">null</span>';
    }

    if (typeof data === 'string') {
      try {
        data = JSON.parse(data);
      } catch (e) {
        return `<span class="json-string">${escapeHtml(data)}</span>`;
      }
    }

    return syntaxHighlight(JSON.stringify(data, null, 2));
  }

  function syntaxHighlight(json) {
    return json.replace(/("(\\u[a-zA-Z0-9]{4}|\\[^u]|[^\\"])*"(\s*:)?|\b(true|false|null)\b|-?\d+(?:\.\d*)?(?:[eE][+\-]?\d+)?)/g, (match) => {
      let cls = 'json-number';
      if (/^"/.test(match)) {
        if (/:$/.test(match)) {
          cls = 'json-key';
          match = match.slice(0, -1); // Remove the colon, we'll add it back
          return `<span class="${cls}">${escapeHtml(match)}</span>:`;
        } else {
          cls = 'json-string';
        }
      } else if (/true|false/.test(match)) {
        cls = 'json-boolean';
      } else if (/null/.test(match)) {
        cls = 'json-null';
      }
      return `<span class="${cls}">${escapeHtml(match)}</span>`;
    });
  }

  function highlightSSE(data) {
    // Highlight SSE event structure
    return escapeHtml(data).replace(/(event: \w+)/g, '<span class="json-key">$1</span>')
      .replace(/(data: )/g, '<span class="json-boolean">$1</span>');
  }

  function renderTimingView(req) {
    if (!req.response?.streaming || !req.response?.chunks) {
      return;
    }

    const chunks = req.response.chunks;
    const ttfb = chunks[0]?.delay_ms || 0;
    const totalTime = chunks.reduce((acc, c) => acc + (c.delay_ms || 0), 0);

    $('#timingTTFB').textContent = formatDuration(ttfb);
    $('#timingTotal').textContent = formatDuration(totalTime);
    $('#timingChunks').textContent = chunks.length;

    // Draw waveform
    drawWaveform(chunks);

    // Render chunk list
    const chunkList = $('#chunkList');
    chunkList.innerHTML = chunks.slice(0, 50).map((chunk, i) => `
      <div class="chunk-item">
        <span class="chunk-item__index">#${i + 1}</span>
        <span class="chunk-item__delay">+${chunk.delay_ms}ms</span>
        <span class="chunk-item__size">${formatBytes(chunk.data?.length || 0)}</span>
        <span class="chunk-item__preview">${escapeHtml((chunk.data || '').slice(0, 50))}</span>
      </div>
    `).join('');
  }

  function drawWaveform(chunks) {
    const canvas = dom.waveformCanvas;
    if (!canvas) return;

    const ctx = canvas.getContext('2d');
    const dpr = window.devicePixelRatio || 1;

    // Set canvas size
    const rect = canvas.parentElement.getBoundingClientRect();
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;
    ctx.scale(dpr, dpr);

    const width = rect.width;
    const height = rect.height;

    // Clear
    ctx.clearRect(0, 0, width, height);

    if (chunks.length === 0) return;

    // Get theme color
    const style = getComputedStyle(document.documentElement);
    const accentColor = style.getPropertyValue('--accent-primary').trim();
    const gridColor = style.getPropertyValue('--border-subtle').trim();

    // Draw grid
    ctx.strokeStyle = gridColor;
    ctx.lineWidth = 0.5;
    for (let i = 0; i <= 4; i++) {
      const y = (height / 4) * i;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(width, y);
      ctx.stroke();
    }

    // Calculate max delay for normalization
    const maxDelay = Math.max(...chunks.map(c => c.delay_ms || 0), 1);
    const stepWidth = width / chunks.length;

    // Draw bars
    ctx.fillStyle = accentColor;
    chunks.forEach((chunk, i) => {
      const barHeight = ((chunk.delay_ms || 0) / maxDelay) * (height - 10);
      const x = i * stepWidth;
      const y = height - barHeight;

      ctx.fillRect(x + 1, y, Math.max(stepWidth - 2, 2), barHeight);
    });

    // Draw line connecting peaks
    ctx.strokeStyle = accentColor;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    chunks.forEach((chunk, i) => {
      const x = i * stepWidth + stepWidth / 2;
      const y = height - ((chunk.delay_ms || 0) / maxDelay) * (height - 10);

      if (i === 0) {
        ctx.moveTo(x, y);
      } else {
        ctx.lineTo(x, y);
      }
    });
    ctx.stroke();
  }

  // ==========================================================================
  // TABS
  // ==========================================================================

  function initTabs() {
    $$('.detail-tab').forEach(tab => {
      tab.addEventListener('click', () => {
        const tabName = tab.dataset.tab;

        // Update active tab
        $$('.detail-tab').forEach(t => t.classList.remove('detail-tab--active'));
        tab.classList.add('detail-tab--active');

        // Update active content
        $$('.detail-tab-content').forEach(c => c.classList.remove('detail-tab-content--active'));
        $(`[data-content="${tabName}"]`).classList.add('detail-tab-content--active');
      });
    });
  }

  // ==========================================================================
  // ACTIONS
  // ==========================================================================

  async function replayRequest() {
    if (!state.selectedId) return;

    const req = state.requests.find(r => r.id === state.selectedId);
    if (!req) return;

    try {
      const response = await fetch(`/api/v1/requests/${state.selectedId}/replay`, {
        method: 'POST'
      });

      if (response.ok) {
        showToast('Request replayed', 'success');
      } else {
        showToast('Failed to replay request', 'error');
      }
    } catch (e) {
      showToast('Failed to replay request', 'error');
    }
  }

  async function copyAsCurl() {
    if (!state.selectedId) return;

    const req = state.requests.find(r => r.id === state.selectedId);
    if (!req) return;

    try {
      const response = await fetch(`/api/v1/requests/${state.selectedId}/curl`, {
        method: 'POST'
      });
      if (!response.ok) {
        throw new Error(`curl endpoint failed: ${response.status}`);
      }
      const data = await response.json();

      await navigator.clipboard.writeText(data.curl);
      showToast('Copied to clipboard', 'success');
    } catch (e) {
      // Fallback: generate curl locally
      const curl = generateCurl(req);
      await navigator.clipboard.writeText(curl);
      showToast('Copied to clipboard', 'success');
    }
  }

  function generateCurl(req) {
    const parts = ['curl'];

    parts.push(`-X ${req.request.method}`);

    if (req.request.headers) {
      Object.entries(req.request.headers).forEach(([key, value]) => {
        if (key.toLowerCase() !== 'content-length') {
          parts.push(`-H '${key}: ${value}'`);
        }
      });
    }

    if (req.request.body) {
      const body = typeof req.request.body === 'string'
        ? req.request.body
        : JSON.stringify(req.request.body);
      parts.push(`-d '${body.replace(/'/g, "\\'")}'`);
    }

    parts.push(`'${window.location.origin}${req.request.path}'`);

    return parts.join(' \\\n  ');
  }

  async function toggleRecording() {
    state.isRecording = !state.isRecording;

    try {
      await fetch('/api/v1/record', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          enabled: state.isRecording,
          output: state.isRecording ? './session.json' : null
        })
      });

      updateRecordingUI();
      showToast(state.isRecording ? 'Recording started' : 'Recording stopped', 'success');
    } catch (e) {
      state.isRecording = !state.isRecording; // Revert
      showToast('Failed to toggle recording', 'error');
    }
  }

  function updateRecordingUI() {
    dom.toggleRecord.classList.toggle('is-recording', state.isRecording);
    dom.recordingLed.dataset.status = state.isRecording ? 'active' : 'inactive';
    dom.recordingStatus.textContent = state.isRecording ? 'ON' : 'OFF';
    dom.recordingStatus.classList.toggle('status-item__value--active', state.isRecording);
    dom.recordingStatus.classList.toggle('status-item__value--inactive', !state.isRecording);
  }

  async function clearBuffer() {
    if (!confirm('Clear all requests from the buffer?')) return;

    try {
      const response = await fetch('/api/v1/requests', { method: 'DELETE' });
      if (!response.ok) {
        throw new Error(`clear failed: ${response.status}`);
      }
      state.requests = [];
      state.markedIds.clear();
      state.selectedId = null;
      renderRequestList();
      hideDetail();
      updateBufferCount();
      showToast('Buffer cleared', 'success');
    } catch (e) {
      showToast('Failed to clear buffer', 'error');
    }
  }

  async function saveAll() {
    const filename = prompt('Download all requests as:', 'session.json');
    if (!filename) return;

    try {
      downloadCassette(state.requests, filename);
      showToast(`Saved ${state.requests.length} requests`, 'success');
    } catch (e) {
      showToast('Failed to save requests', 'error');
    }
  }

  async function saveMarked() {
    if (state.markedIds.size === 0) {
      showToast('No requests marked', 'error');
      return;
    }

    const filename = prompt('Download marked requests as:', 'marked.json');
    if (!filename) return;

    try {
      const marked = state.requests.filter(r => state.markedIds.has(r.id));
      downloadCassette(marked, filename);
      showToast(`Saved ${marked.length} requests`, 'success');
    } catch (e) {
      showToast('Failed to save requests', 'error');
    }
  }

  function markAllVisible() {
    const filtered = getFilteredRequests();
    filtered.forEach(req => state.markedIds.add(req.id));
    renderRequestList();
    showToast(`Marked ${filtered.length} requests`, 'success');
  }

  function hideDetail() {
    dom.detailContent.style.display = 'none';
    $('.detail-panel__empty').style.display = 'flex';
  }

  // ==========================================================================
  // THEME SWITCHING
  // ==========================================================================

  function initThemes() {
    const savedTheme = localStorage.getItem('replayr-theme') || 'midnight';
    const selector = $('#themeSelect');
    document.documentElement.dataset.theme = savedTheme;
    if (selector) {
      selector.value = savedTheme;
      selector.addEventListener('change', () => {
        const theme = selector.value;
        document.documentElement.dataset.theme = theme;
        localStorage.setItem('replayr-theme', theme);
      });
    }
  }

  // ==========================================================================
  // FILTER
  // ==========================================================================

  function initFilter() {
    let debounceTimer;

    dom.filterInput.addEventListener('input', (e) => {
      clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => {
        state.filter = e.target.value.trim();
        refreshRequestsFromServer().catch((err) => {
          console.warn('[Replayr] Failed applying filter', err);
          showToast('Invalid or failed filter', 'error');
        });
      }, 220);
    });

    dom.clearFilter.addEventListener('click', () => {
      dom.filterInput.value = '';
      state.filter = '';
      refreshRequestsFromServer().catch((err) => {
        console.warn('[Replayr] Failed clearing filter', err);
      });
    });
  }

  // ==========================================================================
  // COPY BUTTONS
  // ==========================================================================

  function initCopyButtons() {
    $$('.copy-btn').forEach(btn => {
      btn.addEventListener('click', async () => {
        const targetId = btn.dataset.copy;
        const target = $(`#${targetId}`);

        if (target) {
          await navigator.clipboard.writeText(target.textContent);
          btn.textContent = 'Copied!';
          btn.classList.add('copied');
          setTimeout(() => {
            btn.textContent = 'Copy';
            btn.classList.remove('copied');
          }, 1500);
        }
      });
    });
  }

  // ==========================================================================
  // INTERCEPT MODAL
  // ==========================================================================

  function handleInterceptedRequest(data) {
    state.interceptQueue.push(normalizeInterceptEntry(data));
    dom.interceptLed.dataset.status = 'active';
    showInterceptModal(state.interceptQueue[0]);
  }

  function normalizeInterceptEntry(data) {
    if (data.request) {
      return {
        id: data.id,
        request: data.request
      };
    }

    return {
      id: data.id,
      request: {
        method: data.method,
        path: data.path,
        headers: data.headers || {},
        body: data.body ?? null
      }
    };
  }

  function showInterceptModal(data) {
    if (!data || !data.request) return;
    dom.interceptModal.style.display = 'flex';
    $('#interceptHeaders').value = JSON.stringify(data.request.headers, null, 2);
    $('#interceptBody').value = typeof data.request.body === 'string'
      ? data.request.body
      : JSON.stringify(data.request.body, null, 2);
  }

  function hideInterceptModal() {
    dom.interceptModal.style.display = 'none';
  }

  async function refreshInterceptQueue(showModal = true) {
    const response = await fetch('/api/v1/intercept/queue');
    if (!response.ok) {
      throw new Error(`intercept queue failed: ${response.status}`);
    }

    const queue = await response.json();
    state.interceptQueue = queue.map(normalizeInterceptEntry);
    const hasItems = state.interceptQueue.length > 0;
    dom.interceptLed.dataset.status = hasItems ? 'active' : 'inactive';

    if (hasItems) {
      dom.interceptStatus.style.display = 'flex';
      dom.interceptPattern.textContent = state.interceptPattern || 'queue active';
      if (showModal && dom.interceptModal.style.display === 'none') {
        showInterceptModal(state.interceptQueue[0]);
      }
    } else if (dom.interceptModal.style.display !== 'none') {
      hideInterceptModal();
    }
  }

  async function releaseIntercepted() {
    const data = state.interceptQueue[0];
    if (!data) return;

    try {
      let headers, body;
      try {
        headers = JSON.parse($('#interceptHeaders').value);
      } catch (e) {
        showToast('Invalid headers JSON', 'error');
        return;
      }

      try {
        body = JSON.parse($('#interceptBody').value);
      } catch (e) {
        body = $('#interceptBody').value;
      }

      await fetch(`/api/v1/intercept/${data.id}/release`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ headers, body })
      });

      state.interceptQueue.shift();
      hideInterceptModal();
      showToast('Request released', 'success');
    } catch (e) {
      showToast('Failed to release request', 'error');
    }
  }

  async function dropIntercepted() {
    const data = state.interceptQueue[0];
    if (!data) return;

    try {
      await fetch(`/api/v1/intercept/${data.id}/drop`, { method: 'POST' });
      state.interceptQueue.shift();
      hideInterceptModal();
      showToast('Request dropped', 'success');
    } catch (e) {
      showToast('Failed to drop request', 'error');
    }
  }

  // ==========================================================================
  // STATUS UPDATES
  // ==========================================================================

  function updateStatus(data) {
    if (data.recording !== undefined) {
      state.isRecording = data.recording;
      updateRecordingUI();
    }

    if (data.intercept_pattern !== undefined) {
      state.interceptPattern = data.intercept_pattern;
      dom.interceptLed.dataset.status = data.intercept_pattern ? 'active' : 'inactive';
      dom.interceptStatus.style.display = data.intercept_pattern ? 'flex' : 'none';
      dom.interceptPattern.textContent = data.intercept_pattern || '';
    }

    if (data.scenario_step !== undefined) {
      dom.scenarioStep.textContent = data.scenario_step || '—';
    }
  }

  function initInterceptControls() {
    dom.interceptLed.style.cursor = 'pointer';
    dom.interceptLed.title = 'Set or clear intercept pattern';
    dom.interceptLed.addEventListener('click', async () => {
      const current = state.interceptPattern || '';
      const value = prompt('Intercept CEL pattern (blank to clear):', current);
      if (value === null) return;

      const pattern = value.trim() || null;
      try {
        const response = await fetch('/api/v1/intercept', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ pattern })
        });
        if (!response.ok) {
          throw new Error(`failed to set intercept: ${response.status}`);
        }

        state.interceptPattern = pattern;
        dom.interceptStatus.style.display = pattern ? 'flex' : 'none';
        dom.interceptPattern.textContent = pattern || '';
        dom.interceptLed.dataset.status = pattern ? 'active' : 'inactive';
        showToast(pattern ? 'Intercept enabled' : 'Intercept cleared', 'success');
      } catch (err) {
        console.warn('[Replayr] Failed updating intercept pattern', err);
        showToast('Failed to update intercept pattern', 'error');
      }
    });
  }

  function updateBufferCount() {
    dom.bufferCount.textContent = `${state.requests.length} / ${state.maxBufferSize}`;
  }

  async function loadInitialData() {
    try {
      await refreshRequestsFromServer();

      const recordRes = await fetch('/api/v1/record');
      if (recordRes.ok) {
        const data = await recordRes.json();
        state.isRecording = !!data.enabled;
        updateRecordingUI();
      }

      await refreshInterceptQueue(false);
    } catch (err) {
      console.warn('[Replayr] Failed to load initial data', err);
    }
  }

  // ==========================================================================
  // TOAST NOTIFICATIONS
  // ==========================================================================

  function showToast(message, type = 'info') {
    const toast = document.createElement('div');
    toast.className = `toast toast--${type}`;
    toast.innerHTML = `
      <svg class="toast__icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        ${type === 'success'
          ? '<path d="M20 6L9 17l-5-5"/>'
          : '<circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/>'}
      </svg>
      <span>${escapeHtml(message)}</span>
    `;

    dom.toastContainer.appendChild(toast);

    setTimeout(() => {
      toast.style.opacity = '0';
      toast.style.transform = 'translateX(20px)';
      setTimeout(() => toast.remove(), 300);
    }, 3000);
  }

  // ==========================================================================
  // UTILITIES
  // ==========================================================================

  function escapeHtml(str) {
    if (str === null || str === undefined) return '';
    return String(str)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  function formatDuration(ms) {
    if (ms === null || ms === undefined) return '—';
    if (ms < 1000) return `${Math.round(ms)}ms`;
    return `${(ms / 1000).toFixed(2)}s`;
  }

  function formatBytes(bytes) {
    if (bytes === null || bytes === undefined) return '—';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
  }

  function getResponseSize(req) {
    if (!req.response) return 0;
    if (req.response.chunks) {
      return req.response.chunks.reduce((acc, c) => acc + (c.data?.length || 0), 0);
    }
    if (req.response.body) {
      return typeof req.response.body === 'string'
        ? req.response.body.length
        : JSON.stringify(req.response.body).length;
    }
    return 0;
  }

  function downloadCassette(interactions, filename) {
    const payload = {
      replayr_version: '1',
      cassette: {
        id: crypto.randomUUID ? crypto.randomUUID() : `cassette_${Date.now()}`,
        name: filename.replace(/\.json$/i, ''),
        created_at: new Date().toISOString(),
        source: 'inspector-download'
      },
      interactions
    };

    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = filename.endsWith('.json') ? filename : `${filename}.json`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  }

  // ==========================================================================
  // INITIALIZATION
  // ==========================================================================

  function init() {
    initThemes();
    initTabs();
    initFilter();
    initCopyButtons();
    initInterceptControls();

    // Event listeners
    dom.toggleRecord.addEventListener('click', toggleRecording);
    dom.clearBuffer.addEventListener('click', clearBuffer);
    dom.saveAll.addEventListener('click', saveAll);
    dom.markAll.addEventListener('click', markAllVisible);
    dom.saveMarked.addEventListener('click', saveMarked);

    $('#replayRequest').addEventListener('click', replayRequest);
    $('#copyAsCurl').addEventListener('click', copyAsCurl);

    $('#closeInterceptModal').addEventListener('click', hideInterceptModal);
    $('#releaseRequest').addEventListener('click', releaseIntercepted);
    $('#dropRequest').addEventListener('click', dropIntercepted);

    // Close modal on backdrop click
    dom.interceptModal.querySelector('.modal__backdrop').addEventListener('click', hideInterceptModal);

    // Connect WebSocket
    connectWebSocket();

    // Load initial server state
    loadInitialData();

    // Keep intercept queue in sync
    setInterval(() => {
      refreshInterceptQueue(false).catch(() => {});
    }, 2000);

    console.log('[Replayr] Inspector UI initialized');
  }

  // Start
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
