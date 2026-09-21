const invoke = window.__TAURI__.core.invoke;
const state = { repo:'', route:location.hash.slice(1)||'dashboard', generation:null, revision:0, confirm:null, quietPolls:0 };
const content = document.querySelector('#content');
const repository = document.querySelector('#repository');
const connection = document.querySelector('#connection');

const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const items = value => value?.items || (Array.isArray(value) ? value : []);
function table(rows) {
  if (!rows.length) return '<div class="empty">No records</div>';
  const keys = [...new Set(rows.flatMap(row => Object.keys(row || {})))].slice(0,12);
  return `<div role="region" aria-label="Results" tabindex="0"><table><thead><tr>${keys.map(k=>`<th>${esc(k)}</th>`).join('')}</tr></thead><tbody>${rows.map(row=>`<tr>${keys.map(k=>`<td>${esc(typeof row[k]==='object'?JSON.stringify(row[k]):row[k])}</td>`).join('')}</tr>`).join('')}</tbody></table></div>`;
}
async function query(query) {
  const reply = await invoke('operator_query', { query });
  if (state.generation !== null && state.generation !== reply.daemon_generation) setConnection('reconnecting','Daemon restarted; refreshing…');
  state.generation = reply.daemon_generation;
  state.revision = Math.max(state.revision, reply.revision);
  return reply.payload.data;
}
async function mutate(action, key=crypto.randomUUID()) {
  return invoke('operator_mutate', { mutation:{ request_id:crypto.randomUUID(), idempotency_key:key, action } });
}
function setConnection(kind,message) { connection.dataset.state=kind; connection.textContent=message; }
async function heartbeat() {
  try { const s=await invoke('connection_status'); setConnection(s.state,s.message); if(s.generation!==state.generation&&state.generation!==null){state.revision=0;render();return;} if(s.state==='connected'){const events=await invoke('operator_observe',{after:state.revision});if(events.length){state.quietPolls=0;const expected=state.revision+1;if(state.revision&&events[0].sequence!==expected) state.revision=0;else state.revision=events.at(-1).sequence;render();}else if(++state.quietPolls>=6&&['dashboard','execution'].includes(state.route)){state.quietPolls=0;render();}} }
  catch(e){ const message=String(e); if(message.includes('event_gap')){state.revision=0;await render();return;} setConnection('disconnected',message); }
}
async function loadRepositories() {
  try {
    const data=await query({query:'repositories'}); const rows=data.repositories||items(data);
    repository.innerHTML=rows.map(r=>`<option value="${esc(r.repo_root||r.repository||r.path)}">${esc(r.name||r.repo_root||r.repository||r.path)}</option>`).join('');
    if(!state.repo && rows.length) state.repo=rows[0].repo_root||rows[0].repository||rows[0].path;
    repository.value=state.repo;
  } catch(e) { setConnection('disconnected',String(e)); }
}
async function render() {
  content.innerHTML='<p class="muted">Loading…</p>';
  try {
    if(state.route==='settings') return renderSettings();
    if(state.route==='local-llm') return renderLlm();
    if(!state.repo){ content.innerHTML='<h1>Familiar</h1><p class="empty">No repositories are registered yet.</p>'; return; }
    if(state.route==='backlog') return renderBacklog();
    if(state.route==='execution') return renderExecution();
    return renderDashboard();
  } catch(e) { content.innerHTML=`<h1>${esc(state.route)}</h1><p class="error" role="alert">${esc(e)}</p>`; }
}
async function renderDashboard(){
  const [inference,gates,sessions,project]=await Promise.all([
    query({query:'inference_status'}), query({query:'gates',repo:state.repo}),
    query({query:'sessions',repo:state.repo,limit:10}), query({query:'project_state',repo:state.repo})
  ]);
  content.innerHTML=`<h1>Dashboard</h1><div class="cards"><article class="card"><h2>Inference</h2><pre>${esc(JSON.stringify(inference,null,2))}</pre></article><article class="card"><h2>Project</h2><strong>${esc(project.state||'not registered')}</strong></article><article class="card"><h2>Waiting on you</h2><strong>${items(gates).length}</strong></article><article class="card"><h2>Recent sessions</h2><strong>${items(sessions).length}</strong></article></div><h2>Gates</h2>${table(items(gates))}<h2>Sessions</h2>${table(items(sessions))}<div class="toolbar"><button id="stop-daemon" class="danger">Stop Familiar…</button></div>`;
  document.querySelector('#stop-daemon').onclick=()=>confirmAction({action:'stop_daemon'},'Stop Familiar?','This stops the daemon and daemon-owned work. The desktop stays open and reports Disconnected.');
}
async function renderBacklog(){
  const [backlog,deps,blocked,checkpoints]=await Promise.all([
    query({query:'backlog',repo:state.repo,limit:200}), query({query:'dependencies',repo:state.repo}),
    query({query:'blocked_reasons',repo:state.repo}), query({query:'checkpoints',repo:state.repo})
  ]);
  content.innerHTML=`<h1>Backlog</h1><div class="toolbar"><button id="pause">Pause project</button><button id="resume-project">Resume project</button></div><div class="card form-grid"><label for="prd-path">PRD path</label><input id="prd-path" placeholder="docs/prds/PRD-104.md"><label for="prd-id">PRD identity</label><input id="prd-id" placeholder="PRD-104"><span></span><div class="toolbar"><button id="read-prd">Read</button><button id="start-prd">Start</button><button id="resume-prd">Resume retained</button><button id="release-prd" class="danger">Release…</button><button id="complete-prd" class="danger">Force complete…</button></div></div><div id="prd-detail"></div>${table(items(backlog))}<h2>Dependencies</h2>${table(items(deps))}<h2>Blocked reasons</h2>${table(items(blocked))}<h2>Checkpoints</h2>${table(items(checkpoints))}`;
  document.querySelector('#pause').onclick=()=>confirmAction({action:'set_project_paused',repo:state.repo,paused:true},'Pause this project?');
  document.querySelector('#resume-project').onclick=()=>confirmAction({action:'set_project_paused',repo:state.repo,paused:false},'Resume this project?');
  const path=()=>document.querySelector('#prd-path').value; const id=()=>document.querySelector('#prd-id').value;
  document.querySelector('#read-prd').onclick=async()=>{const value=await query({query:'prd_text',repo:state.repo,prd_path:path()});document.querySelector('#prd-detail').innerHTML=`<pre>${esc(value.text)}</pre>`};
  document.querySelector('#start-prd').onclick=()=>confirmAction({action:'start_prd',repo:state.repo,prd_path:path()},`Start ${path()}?`);
  document.querySelector('#resume-prd').onclick=()=>confirmAction({action:'resume_prd',repo:state.repo,prd_id:id()},`Resume ${id()}?`);
  document.querySelector('#release-prd').onclick=()=>confirmAction({action:'release_prd',repo:state.repo,prd_path:path()},`Release ${path()}?`,'This DISCARDS retained work and cannot be undone.',true);
  document.querySelector('#complete-prd').onclick=()=>confirmAction({action:'complete_prd',repo:state.repo,prd_path:path()},`Force-complete ${path()}?`,'This bypasses unsatisfied gates and cannot be undone.',true);
}
async function renderExecution(){
  const [executions,sessions,rounds]=await Promise.all([
    query({query:'executions',repo:state.repo,limit:100}), query({query:'sessions',repo:state.repo,limit:100}), query({query:'rounds',repo:state.repo,limit:100})
  ]);
  content.innerHTML=`<h1>Execution</h1><div class="card form-grid"><label for="execution-id">Execution ID</label><input id="execution-id"><label for="session-id">Session ID</label><input id="session-id"><span></span><div class="toolbar"><button id="cancel-execution" class="danger">Cancel execution…</button><button id="attempts">Attempts</button><button id="budget">Budget</button><button id="review">Review</button></div></div><div id="execution-detail"></div><h2>Live and recent executions</h2>${table(items(executions))}<h2>Sessions</h2>${table(items(sessions))}<h2>Rounds</h2>${table(items(rounds))}`;
  const execution=()=>document.querySelector('#execution-id').value;const session=()=>document.querySelector('#session-id').value;const detail=document.querySelector('#execution-detail');
  document.querySelector('#cancel-execution').onclick=()=>confirmAction({action:'cancel_execution',repo:state.repo,execution_id:execution()},`Cancel ${execution()}?`,'The running worker is terminated and work may be lost.');
  document.querySelector('#attempts').onclick=async()=>{detail.innerHTML=table(items(await query({query:'attempts',repo:state.repo,session_id:session()})))};
  document.querySelector('#budget').onclick=async()=>{detail.innerHTML=`<pre>${esc(JSON.stringify(await query({query:'budget',repo:state.repo,session_id:session()}),null,2))}</pre>`};
  document.querySelector('#review').onclick=async()=>{detail.innerHTML=table(items(await query({query:'review',repo:state.repo,session_id:session()})))};
}
function flatten(value,path=[],out=[]){
  if(value===null||['string','number','boolean'].includes(typeof value)) out.push({path,value});
  else if(Array.isArray(value)) value.forEach((v,i)=>flatten(v,[...path,String(i)],out));
  else if(value&&typeof value==='object') Object.entries(value).forEach(([k,v])=>flatten(v,[...path,k],out));
  return out;
}
function settingControl(field,index){
  const path=esc(JSON.stringify(field.path)); const value=String(field.value);
  if(typeof field.value==='boolean') return `<select id="f${index}" data-path="${path}" data-original="${value}"><option ${field.value?'selected':''}>true</option><option ${!field.value?'selected':''}>false</option></select>`;
  if(typeof field.value==='number') return `<input id="f${index}" type="number" data-path="${path}" value="${esc(value)}" data-original="${esc(value)}">`;
  return `<input id="f${index}" data-path="${path}" value="${esc(value)}" data-original="${esc(value)}">`;
}
async function renderSettings(){
  const [config,choices]=await Promise.all([query({query:'config_document'}),query({query:'config_choices'})]);
  const fields=flatten(config.document);
  content.innerHTML=`<h1>Settings</h1><p class="muted">Validated Familiar settings. The configuration file is never opened in an editor.</p><form id="settings-form" class="form-grid">${fields.map((f,i)=>`<label for="f${i}">${esc(f.path.join('.'))}</label>${settingControl(f,i)}`).join('')}<span></span><button class="primary" type="submit">Save changes</button></form><p id="save-result" role="status"></p>`;
  document.querySelector('#settings-form').onsubmit=async e=>{e.preventDefault();const edits=[...e.target.querySelectorAll('input,select')].filter(x=>x.dataset.path&&x.value!==x.dataset.original).map(x=>({path:JSON.parse(x.dataset.path),value:x.value}));if(!edits.length)return;try{await mutate({action:'save_config',edits});document.querySelector('#save-result').textContent='Saved. Restart-required values will take effect on the next daemon start.';}catch(err){document.querySelector('#save-result').textContent=String(err);}};
}
async function renderLlm(){
  const [settings,choices,models,status]=await Promise.all([query({query:'inference_settings'}),query({query:'config_choices'}),query({query:'discover_models'}),query({query:'inference_status'})]);
  const modelList=[...(choices.models||[]),...(models.models||[])].filter((v,i,a)=>v&&a.indexOf(v)===i);
  content.innerHTML=`<h1>Configure Local LLM</h1><div class="cards"><article class="card"><h2>Applied state</h2><pre>${esc(JSON.stringify(status,null,2))}</pre></article></div><form id="llm-form" class="form-grid"><label for="mode">Mode</label><select id="mode">${(choices.inference_modes||[]).map(v=>`<option ${v===settings.mode?'selected':''}>${esc(v)}</option>`).join('')}</select><label for="url">OpenAI-compatible URL</label><input id="url" type="url" value="${esc(settings.builtin_url)}"><label for="model">Model</label><input id="model" list="models" value="${esc(settings.builtin_model)}"><datalist id="models">${modelList.map(v=>`<option value="${esc(v)}">`).join('')}</datalist><span></span><div class="toolbar"><button id="test" type="button">Test connection</button><button class="primary" type="submit">Save and apply</button></div></form><p id="llm-result" role="status"></p>`;
  document.querySelector('#test').onclick=async()=>{const out=document.querySelector('#llm-result');out.textContent='Testing…';try{out.textContent=JSON.stringify(await query({query:'test_connection',target:'text_primary'}));}catch(e){out.textContent=String(e)}};
  document.querySelector('#llm-form').onsubmit=async e=>{e.preventDefault();const out=document.querySelector('#llm-result');try{await mutate({action:'save_inference_config',mode:e.target.mode.value,builtin_url:e.target.url.value,builtin_model:e.target.model.value});out.textContent='Saved and applied.';}catch(err){out.textContent=String(err)}};
}
function confirmAction(action,summary,warning='',needsAttribution=false){
  const dialog=document.querySelector('#confirm'); state.confirm={action,key:crypto.randomUUID()};
  document.querySelector('#confirm-summary').textContent=summary; document.querySelector('#confirm-warning').textContent=warning;
  document.querySelector('#actor-label').hidden=!needsAttribution;document.querySelector('#reason-label').hidden=!needsAttribution;dialog.showModal();
}
document.querySelector('#confirm').addEventListener('close',async e=>{if(e.target.returnValue!=='default'||!state.confirm){state.confirm=null;return;}const pending=state.confirm;state.confirm=null;try{if(!document.querySelector('#actor-label').hidden){pending.action.actor=document.querySelector('#actor').value;pending.action.reason=document.querySelector('#reason').value;}await mutate(pending.action,pending.key);render();}catch(err){document.querySelector('#confirm-error').textContent=String(err);}});
document.querySelectorAll('[data-route]').forEach(button=>button.onclick=()=>{state.route=button.dataset.route;location.hash=state.route;render();});
repository.onchange=()=>{state.repo=repository.value;render();};document.querySelector('#refresh').onclick=render;
window.addEventListener('hashchange',()=>{state.route=location.hash.slice(1)||'dashboard';render();});
setInterval(heartbeat,5000); heartbeat(); loadRepositories().then(render);
window.__TAURI__.event.listen('familiar://request-stop',()=>confirmAction(
  {action:'stop_daemon'},
  'Stop Familiar?',
  'This stops the daemon and daemon-owned work. The desktop stays open and reports Disconnected.'
));
