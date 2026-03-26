/// Embedded HTML for the Kanban web UI — single-page app with dark theme.
pub const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mu Kanban</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
:root{
  --bg:       #0f1117;
  --surface:  #1a1d27;
  --card:     #232733;
  --border:   #2e3347;
  --text:     #c9cdd8;
  --text-dim: #6b7280;
  --accent:   #6366f1;
  --green:    #22c55e;
  --yellow:   #eab308;
  --red:      #ef4444;
  --blue:     #3b82f6;
  --purple:   #a855f7;
  --orange:   #f97316;
  --cyan:     #06b6d4;
}
html,body{height:100%;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,monospace;background:var(--bg);color:var(--text);font-size:13px;line-height:1.4}
#app{display:flex;flex-direction:column;height:100vh}

/* Header */
.header{display:flex;align-items:center;justify-content:space-between;padding:8px 16px;background:var(--surface);border-bottom:1px solid var(--border);flex-shrink:0}
.header h1{font-size:15px;font-weight:600;letter-spacing:.5px}
.header h1 span{color:var(--accent)}
.header-actions{display:flex;gap:8px;align-items:center}
.stats-bar{display:flex;gap:12px;font-size:12px;color:var(--text-dim)}
.stats-bar .stat{display:flex;align-items:center;gap:4px}
.stats-bar .stat .dot{width:8px;height:8px;border-radius:50%;display:inline-block}
.btn{padding:4px 10px;border:1px solid var(--border);border-radius:4px;background:var(--surface);color:var(--text);cursor:pointer;font-size:12px;transition:background .15s}
.btn:hover{background:var(--card)}
.btn-primary{background:var(--accent);border-color:var(--accent);color:#fff}
.btn-primary:hover{opacity:.85}
.btn-sm{padding:2px 6px;font-size:11px}
.btn-danger{color:var(--red);border-color:var(--red)}
.btn-danger:hover{background:rgba(239,68,68,.1)}

/* Board */
.board{display:grid;grid-template-columns:repeat(6,minmax(200px,1fr));gap:8px;padding:8px;overflow-x:auto;flex:1;align-content:start}
.column{background:var(--surface);border:1px solid var(--border);border-radius:6px;display:flex;flex-direction:column;min-height:120px;max-height:calc(100vh - 120px);overflow:hidden}
.column-header{padding:8px 10px;font-size:12px;font-weight:600;text-transform:uppercase;letter-spacing:.8px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center;flex-shrink:0}
.column-header .count{background:var(--card);padding:1px 6px;border-radius:10px;font-size:11px;color:var(--text-dim)}
.column-cards{padding:4px;overflow-y:auto;flex:1}

/* Cards */
.card{background:var(--card);border:1px solid var(--border);border-radius:4px;padding:8px;margin-bottom:4px;cursor:pointer;transition:border-color .15s}
.card:hover{border-color:var(--accent)}
.card-name{font-size:12px;font-weight:600;margin-bottom:4px;word-break:break-word}
.card-meta{display:flex;justify-content:space-between;align-items:center;font-size:10px;color:var(--text-dim)}
.card-badge{padding:1px 5px;border-radius:3px;font-size:10px;font-weight:500;text-transform:uppercase}
.card-error{color:var(--red);font-size:11px;margin-top:4px;word-break:break-word}
.card-actions{display:flex;gap:4px;margin-top:6px}
.card-workdir{font-size:10px;color:var(--cyan);margin-top:3px;word-break:break-all}

/* State badge colors */
.badge-draft{background:rgba(107,114,128,.2);color:#9ca3af}
.badge-todo{background:rgba(59,130,246,.15);color:var(--blue)}
.badge-processing{background:rgba(234,179,8,.15);color:var(--yellow)}
.badge-feedback{background:rgba(168,85,247,.15);color:var(--purple)}
.badge-complete{background:rgba(34,197,94,.15);color:var(--green)}
.badge-error{background:rgba(239,68,68,.15);color:var(--red)}

/* Column accent borders */
.col-draft .column-header{border-left:3px solid #6b7280}
.col-todo .column-header{border-left:3px solid var(--blue)}
.col-processing .column-header{border-left:3px solid var(--yellow)}
.col-feedback .column-header{border-left:3px solid var(--purple)}
.col-complete .column-header{border-left:3px solid var(--green)}
.col-error .column-header{border-left:3px solid var(--red)}

/* Modal */
.modal-overlay{position:fixed;inset:0;background:rgba(0,0,0,.6);display:flex;align-items:center;justify-content:center;z-index:100}
.modal{background:var(--surface);border:1px solid var(--border);border-radius:8px;padding:16px;width:560px;max-width:90vw;max-height:85vh;overflow-y:auto}
.modal h2{font-size:14px;margin-bottom:12px}
.modal label{display:block;font-size:12px;color:var(--text-dim);margin-bottom:4px;margin-top:10px}
.modal input,.modal textarea{width:100%;padding:6px 8px;background:var(--card);border:1px solid var(--border);border-radius:4px;color:var(--text);font-family:inherit;font-size:12px}
.modal textarea{min-height:200px;resize:vertical}
.modal-actions{display:flex;gap:8px;justify-content:flex-end;margin-top:12px}

/* Activity log */
.activity{padding:4px 8px;font-size:11px;color:var(--text-dim)}
.activity-entry{padding:2px 0;border-bottom:1px solid var(--border)}

/* Connection status */
.conn-status{font-size:11px;display:flex;align-items:center;gap:4px}
.conn-dot{width:6px;height:6px;border-radius:50%}
.conn-dot.connected{background:var(--green)}
.conn-dot.disconnected{background:var(--red)}

/* Session log */
.session-entry{padding:8px;margin-bottom:4px;border-radius:4px;border-left:3px solid var(--border)}
.session-entry.role-user{border-left-color:var(--green);background:rgba(34,197,94,.05)}
.session-entry.role-assistant{border-left-color:var(--blue);background:rgba(59,130,246,.05)}
.session-entry.role-tool{border-left-color:var(--yellow);background:rgba(234,179,8,.05)}
.session-entry.role-system{border-left-color:var(--purple);background:rgba(168,85,247,.05)}
.session-role{font-size:11px;font-weight:600;text-transform:uppercase;margin-bottom:3px}
.session-text{white-space:pre-wrap;word-break:break-word;font-family:monospace;font-size:11px;line-height:1.5}
.session-tools{font-size:10px;color:var(--cyan);margin-top:3px}
.session-empty{color:var(--text-dim);padding:20px;text-align:center}
</style>
</head>
<body>
<div id="app">
  <div class="header">
    <h1><span>μ</span> Kanban</h1>
    <div class="stats-bar" id="stats-bar"></div>
    <div class="header-actions">
      <div class="conn-status">
        <div class="conn-dot disconnected" id="conn-dot"></div>
        <span id="conn-label">connecting</span>
      </div>
      <button class="btn btn-primary" onclick="showCreateModal()">+ New Task</button>
    </div>
  </div>
  <div class="board" id="board"></div>
</div>

<!-- Create Task Modal -->
<div class="modal-overlay" id="create-modal" style="display:none" onclick="if(event.target===this)hideCreateModal()">
  <div class="modal">
    <h2>Create New Task</h2>
    <label for="task-name">Name</label>
    <input id="task-name" placeholder="my-task-name" autocomplete="off">
    <label for="task-workdir">Working Directory (optional)</label>
    <input id="task-workdir" placeholder="/path/to/project">
    <label for="task-content">Task Content (markdown)</label>
    <textarea id="task-content" placeholder="---&#10;task_id: my-task&#10;---&#10;Describe the task here..."></textarea>
    <div class="modal-actions">
      <button class="btn" onclick="hideCreateModal()">Cancel</button>
      <button class="btn btn-primary" onclick="createTask()">Create Draft</button>
    </div>
  </div>
</div>

<!-- Edit Draft Modal -->
<div class="modal-overlay" id="edit-modal" style="display:none" onclick="if(event.target===this)hideEditModal()">
  <div class="modal">
    <h2 id="edit-title">Edit Draft</h2>
    <textarea id="edit-content"></textarea>
    <div class="modal-actions">
      <button class="btn" onclick="hideEditModal()">Cancel</button>
      <button class="btn btn-primary" onclick="saveDraft()">Save</button>
    </div>
  </div>
</div>

<!-- Session Log Modal -->
<div class="modal-overlay" id="session-modal" style="display:none" onclick="if(event.target===this)hideSessionModal()">
  <div class="modal" style="width:700px;max-height:85vh">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:12px">
      <h2 id="session-title">Session Log</h2>
      <button class="btn btn-sm" onclick="hideSessionModal()">Close</button>
    </div>
    <div id="session-log" style="overflow-y:auto;max-height:calc(85vh - 80px);font-size:12px"></div>
  </div>
</div>

<script>
const COLUMNS = ['draft','todo','processing','feedback','complete','error'];
const COLUMN_LABELS = {draft:'Draft',todo:'Todo',processing:'Processing',feedback:'Feedback',complete:'Complete',error:'Error'};
let documents = [];
let stats = {};
let editingId = null;
let evtSource = null;

function renderBoard() {
  const board = document.getElementById('board');
  board.innerHTML = '';
  for (const col of COLUMNS) {
    const docs = documents.filter(d => d.state === col);
    const div = document.createElement('div');
    div.className = `column col-${col}`;
    div.innerHTML = `
      <div class="column-header">
        <span>${COLUMN_LABELS[col]}</span>
        <span class="count">${docs.length}</span>
      </div>
      <div class="column-cards">${docs.map(d => renderCard(d)).join('')}</div>
    `;
    board.appendChild(div);
  }
}

function renderCard(doc) {
  const ago = timeAgo(doc.updated_at);
  const actions = cardActions(doc);
  const errorHtml = doc.error ? `<div class="card-error">${esc(doc.error)}</div>` : '';
  const wdHtml = doc.work_dir ? `<div class="card-workdir">${esc(doc.work_dir)}</div>` : '';
  return `
    <div class="card" data-id="${doc.id}">
      <div class="card-name" onclick="showSession('${doc.id}')" style="cursor:pointer">${esc(doc.original_name)}</div>
      <div class="card-meta">
        <span class="card-badge badge-${doc.state}">${doc.state}</span>
        <span>${ago}</span>
      </div>
      ${wdHtml}
      ${errorHtml}
      ${actions ? `<div class="card-actions">${actions}</div>` : ''}
    </div>
  `;
}

function cardActions(doc) {
  const btns = [];
  if (doc.state === 'draft') {
    btns.push(`<button class="btn btn-sm" onclick="editDraft('${doc.id}')">Edit</button>`);
    btns.push(`<button class="btn btn-sm btn-primary" onclick="submitDoc('${doc.id}')">Submit</button>`);
  }
  if (doc.state === 'todo' || doc.state === 'processing') {
    btns.push(`<button class="btn btn-sm btn-danger" onclick="cancelDoc('${doc.id}')">Cancel</button>`);
  }
  if (doc.state === 'error') {
    btns.push(`<button class="btn btn-sm" onclick="retryDoc('${doc.id}')">Retry</button>`);
    btns.push(`<button class="btn btn-sm btn-danger" onclick="cancelDoc('${doc.id}')">Revise</button>`);
  }
  btns.push(`<button class="btn btn-sm" onclick="openFolder('${doc.id}')" title="Open folder">📂</button>`);
  return btns.join('');
}

function renderStats() {
  const bar = document.getElementById('stats-bar');
  if (!stats.total_documents) { bar.innerHTML = '<span class="stat">No tasks</span>'; return; }
  bar.innerHTML = `
    <span class="stat"><span class="dot" style="background:var(--blue)"></span> Todo: ${stats.todo||0}</span>
    <span class="stat"><span class="dot" style="background:var(--yellow)"></span> Proc: ${stats.processing||0}</span>
    <span class="stat"><span class="dot" style="background:var(--purple)"></span> FB: ${stats.feedback||0}</span>
    <span class="stat"><span class="dot" style="background:var(--green)"></span> Done: ${stats.complete||0}</span>
    <span class="stat"><span class="dot" style="background:var(--red)"></span> Err: ${stats.errored||0}</span>
    <span class="stat">Total: ${stats.total_documents}</span>
  `;
}

// --- API calls ---
async function fetchState() {
  try {
    const res = await fetch('/api/state');
    const data = await res.json();
    documents = data.columns.flatMap(c => c.documents);
    renderBoard();
  } catch(e) { console.error('fetchState', e); }
}

async function fetchStats() {
  try {
    const res = await fetch('/api/stats');
    stats = await res.json();
    renderStats();
  } catch(e) { console.error('fetchStats', e); }
}

async function createTask() {
  const name = document.getElementById('task-name').value.trim();
  const content = document.getElementById('task-content').value;
  const workDir = document.getElementById('task-workdir').value.trim();
  if (!name) return;
  try {
    await fetch('/api/documents', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({ name, content, work_dir: workDir || null })
    });
    hideCreateModal();
    document.getElementById('task-name').value = '';
    document.getElementById('task-content').value = '';
    document.getElementById('task-workdir').value = '';
    setTimeout(() => { fetchState(); fetchStats(); }, 300);
  } catch(e) { console.error('createTask', e); }
}

async function submitDoc(id) {
  try {
    await fetch(`/api/documents/${id}/submit`, { method: 'POST' });
    setTimeout(() => { fetchState(); fetchStats(); }, 300);
  } catch(e) { console.error('submit', e); }
}

async function cancelDoc(id) {
  try {
    await fetch(`/api/documents/${id}/cancel`, { method: 'POST' });
    setTimeout(() => { fetchState(); fetchStats(); }, 300);
  } catch(e) { console.error('cancel', e); }
}

async function retryDoc(id) {
  try {
    await fetch(`/api/documents/${id}/retry`, { method: 'POST' });
    setTimeout(() => { fetchState(); fetchStats(); }, 300);
  } catch(e) { console.error('retry', e); }
}

async function openFolder(id) {
  try { await fetch(`/api/open-folder/${id}`, { method: 'POST' }); }
  catch(e) { console.error('openFolder', e); }
}

async function editDraft(id) {
  editingId = id;
  try {
    const res = await fetch(`/api/documents/${id}/content`);
    const content = await res.text();
    document.getElementById('edit-content').value = content;
    const doc = documents.find(d => d.id === id);
    document.getElementById('edit-title').textContent = `Edit: ${doc ? doc.original_name : id}`;
    document.getElementById('edit-modal').style.display = 'flex';
  } catch(e) { console.error('editDraft', e); }
}

async function saveDraft() {
  if (!editingId) return;
  const content = document.getElementById('edit-content').value;
  try {
    await fetch(`/api/documents/${editingId}/content`, {
      method: 'PUT',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({ content })
    });
    hideEditModal();
  } catch(e) { console.error('saveDraft', e); }
}

// --- Modals ---
function showCreateModal() { document.getElementById('create-modal').style.display = 'flex'; document.getElementById('task-name').focus(); }
function hideCreateModal() { document.getElementById('create-modal').style.display = 'none'; }
function hideEditModal() { document.getElementById('edit-modal').style.display = 'none'; editingId = null; }
function hideSessionModal() { document.getElementById('session-modal').style.display = 'none'; }

async function showSession(id) {
  const doc = documents.find(d => d.id === id);
  document.getElementById('session-title').textContent = doc ? `Session: ${doc.original_name}` : 'Session Log';
  const logDiv = document.getElementById('session-log');
  logDiv.innerHTML = '<div class="session-empty">Loading...</div>';
  document.getElementById('session-modal').style.display = 'flex';
  try {
    const res = await fetch(`/api/documents/${id}/session`);
    const entries = await res.json();
    if (!entries.length) {
      logDiv.innerHTML = '<div class="session-empty">No session log yet</div>';
      return;
    }
    logDiv.innerHTML = entries.map(e => {
      const toolHtml = e.tool_calls.length ? `<div class="session-tools">${e.tool_calls.map(t => esc(t)).join('<br>')}</div>` : '';
      return `<div class="session-entry role-${e.role}"><div class="session-role">${e.role}</div><div class="session-text">${esc(e.text)}</div>${toolHtml}</div>`;
    }).join('');
    logDiv.scrollTop = logDiv.scrollHeight;
  } catch(e) {
    logDiv.innerHTML = `<div class="session-empty">Failed to load session</div>`;
    console.error('showSession', e);
  }
}

// --- SSE ---
function connectSSE() {
  evtSource = new EventSource('/api/events');
  const dot = document.getElementById('conn-dot');
  const label = document.getElementById('conn-label');

  evtSource.onopen = () => {
    dot.className = 'conn-dot connected';
    label.textContent = 'live';
  };

  evtSource.addEventListener('init', (e) => {
    try {
      documents = JSON.parse(e.data);
      renderBoard();
      fetchStats();
    } catch(err) { console.error('init parse', err); }
  });

  evtSource.addEventListener('kanban', (e) => {
    try {
      const event = JSON.parse(e.data);
      handleKanbanEvent(event);
    } catch(err) { console.error('kanban parse', err); }
  });

  evtSource.onerror = () => {
    dot.className = 'conn-dot disconnected';
    label.textContent = 'reconnecting';
  };
}

function handleKanbanEvent(event) {
  switch (event.type) {
    case 'document_discovered':
    case 'state_changed':
    case 'processing_started':
    case 'processing_complete':
      // Refresh full state for simplicity
      fetchState();
      fetchStats();
      break;
    case 'stats_updated':
      // StatsUpdated carries the stats inline (it's a tuple variant)
      // The JSON shape is { type: "stats_updated", ... stats fields }
      stats = event;
      renderStats();
      break;
    case 'error':
      fetchState();
      fetchStats();
      break;
  }
}

// --- Utilities ---
function timeAgo(dateStr) {
  const d = new Date(dateStr);
  const now = new Date();
  const diff = Math.floor((now - d) / 1000);
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff/60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff/3600)}h ago`;
  return `${Math.floor(diff/86400)}d ago`;
}

function esc(str) {
  if (!str) return '';
  return str.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
}

// --- Init ---
fetchState();
fetchStats();
connectSSE();
// Periodic refresh as safety net
setInterval(() => { fetchState(); fetchStats(); }, 10000);
</script>
</body>
</html>
"##;
