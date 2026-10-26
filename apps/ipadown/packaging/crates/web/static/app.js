// ipaDown 前端逻辑（从 index.html 抽出，供 CSP script-src 'self' 使用）。
// 交互统一走 data-act 事件委托，页面不出现内联事件处理器。
function isSafariBrowser() {
  const ua = navigator.userAgent || '';
  const safariEngine = ua.includes('AppleWebKit/') && ua.includes('Safari/') && ua.includes('Version/');
  const excluded = /CriOS\/|Chrome\/|FxiOS\/|Firefox\/|EdgiOS\/|Edg\/|OPiOS\/|OPR\/|DuckDuckGo\/|GSA\/|YaBrowser\/|SamsungBrowser\//.test(ua);
  return safariEngine && !excluded;
}
function isDesktopBrowser() {
  const ua = navigator.userAgent || '';
  return !/Android|iPhone|iPad|iPod|Mobile/i.test(ua) && matchMedia('(hover:hover) and (pointer:fine)').matches;
}
const DESKTOP_DEBUG_KEY='ipadown-desktop-debug';
function storedDesktopDebugMode() {
  try { return sessionStorage.getItem(DESKTOP_DEBUG_KEY)==='1'; }
  catch { return false; }
}
const safariBrowser=isSafariBrowser();
const desktopBrowser=isDesktopBrowser();
let desktopDebugMode=desktopBrowser && storedDesktopDebugMode();
let browserAllowed=safariBrowser || desktopDebugMode;
let desktopDebugClicks=0;
let resolvedSafariAddress=location.origin;
let safariAddressPromise;
function safariAddress() { return resolvedSafariAddress; }
async function resolveSafariAddress() {
  try {
    const response=await fetch('/api/config',{cache:'no-store'}), config=await response.json();
    if (response.ok && /^https:\/\/[^/]+/i.test(config.public_base_url || '')) resolvedSafariAddress=String(config.public_base_url).replace(/\/$/,'');
  } catch {}
  document.getElementById('safari-address').textContent=resolvedSafariAddress;
  return resolvedSafariAddress;
}
function renderBrowserAccess() {
  document.body.classList.remove('browser-checking');
  document.body.classList.toggle('browser-blocked',!browserAllowed);
  document.getElementById('browser-gate').classList.toggle('hidden',browserAllowed);
  document.getElementById('desktop-debug-banner').classList.toggle('hidden',!desktopDebugMode);
  document.getElementById('safari-address').textContent=safariAddress();
}
function legacyCopy(text) {
  const input=document.createElement('textarea');
  input.value=text; input.readOnly=true; input.style.position='fixed'; input.style.opacity='0'; input.style.pointerEvents='none';
  document.body.appendChild(input); input.select();
  const copied=document.execCommand('copy'); input.remove();
  if (!copied) throw new Error('copy failed');
}
async function copySafariAddress(button) {
  const label=button.textContent;
  try {
    const address=await safariAddressPromise;
    try {
      if (!navigator.clipboard?.writeText) throw new Error('clipboard unavailable');
      await navigator.clipboard.writeText(address);
    } catch { legacyCopy(address); }
    button.textContent='已复制';
  } catch { button.textContent='复制失败'; }
  clearTimeout(button._copyTimer); button._copyTimer=setTimeout(()=>button.textContent=label,1800);
}
function enableDesktopDebug() {
  desktopDebugMode=true;
  browserAllowed=true;
  try { sessionStorage.setItem(DESKTOP_DEBUG_KEY,'1'); } catch {}
  renderBrowserAccess();
  init();
  toast('已进入桌面调试模式');
}
function handleDesktopDebugClick() {
  desktopDebugClicks+=1;
  if (desktopDebugClicks < 5) return;
  enableDesktopDebug();
}
renderBrowserAccess();
safariAddressPromise=resolveSafariAddress();
if (!safariBrowser && desktopBrowser && !desktopDebugMode) {
  document.getElementById('browser-gate-icon').addEventListener('click',handleDesktopDebugClick);
}

const themeBtn = document.getElementById('theme-btn');
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  themeBtn.textContent = theme === 'dark' ? '🌙' : '☀️';
  document.querySelector('meta[name="theme-color"]').content = theme === 'dark' ? '#000000' : '#f5f5f7';
  localStorage.setItem('ipadown-theme', theme);
}
function toggleTheme() { applyTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'); }
themeBtn.onclick = toggleTheme;
applyTheme(localStorage.getItem('ipadown-theme') || (matchMedia('(prefers-color-scheme:light)').matches ? 'light' : 'dark'));

const VALID_TABS=['account','search','download'];
function storedTab() {
  try { const value=sessionStorage.getItem('ipadown-tab'); return VALID_TABS.includes(value) ? value : 'search'; }
  catch { return 'search'; }
}
function showTab(tab,scroll=true) {
  if (!VALID_TABS.includes(tab)) tab='search';
  closeCustomSelects();
  closeDeviceMenu();
  document.querySelectorAll('[data-tab-panel]').forEach(panel => {
    const active=panel.dataset.tabPanel===tab;
    panel.classList.toggle('hidden',!active);
    panel.setAttribute('aria-hidden',String(!active));
  });
  document.querySelectorAll('[data-tab]').forEach(button => {
    const active=button.dataset.tab===tab;
    button.classList.toggle('active',active);
    button.setAttribute('aria-selected',String(active));
  });
  try { sessionStorage.setItem('ipadown-tab',tab); } catch {}
  if (scroll) window.scrollTo({top:0,behavior:'smooth'});
  if (tab==='account') refreshAccounts();
  if (tab==='download') { pollJobs(); refreshFiles(); }
}

async function api(url, options) {
  const response = await fetch(url, options);
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(data.error || `HTTP ${response.status}`);
    error.authExpired = !!data.auth_expired;
    error.codeRequired = !!data.code_required;
    throw error;
  }
  return data;
}
function esc(value) { return String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char])); }
function arg(value) { return encodeURIComponent(String(value ?? '')).replace(/'/g,'%27'); }
function fmtSize(bytes) {
  if (!bytes) return '0 B';
  const units = ['B','KB','MB','GB']; let index = 0;
  while (bytes >= 1024 && index < units.length - 1) { bytes /= 1024; index++; }
  return bytes.toFixed(bytes >= 100 || index === 0 ? 0 : 1) + ' ' + units[index];
}
// 历史版本目录的体积字符串（如 "952.5 MB"）→ 字节数，失败返回 null
function parseSize(text) {
  const match = /^([\d.]+)\s*([KMGT]?)(?:B)?$/i.exec(String(text || '').trim());
  if (!match) return null;
  const value = parseFloat(match[1]);
  if (!Number.isFinite(value)) return null;
  const units = { '': 1, K: 1024, M: 1024 ** 2, G: 1024 ** 3, T: 1024 ** 4 };
  return Math.round(value * (units[match[2].toUpperCase()] || 1));
}
function toast(message, isError=false) {
  const node = document.getElementById('toast');
  node.textContent = message; node.style.color = isError ? 'var(--ui-danger)' : 'var(--ui-text)'; node.style.display = 'block';
  clearTimeout(node._timer); node._timer = setTimeout(() => node.style.display = 'none', 3200);
}
function handleAuthErr(error) {
  if (!error?.authExpired) return false;
  showAuthBanner(true,!!error.codeRequired); return true;
}
function showAuthBanner(show, needsCode=false) {
  const banner=document.getElementById('auth-banner');
  banner.firstChild.textContent=needsCode ? '下载需要完成 Apple ID 双重认证。 ' : '登录状态需要更新，请重新验证 Apple ID。 ';
  banner.querySelector('button').onclick=()=>openLoginModal(needsCode);
  banner.classList.toggle('hidden', !show);
}

function customSelect(id, value, options, onChange, ariaLabel, extraClass='') {
  const selected = options.find(option => option.value === value) || options[0] || {value:'',label:'—'};
  return `<div class="custom-select ${extraClass}" id="${esc(id)}" data-value="${esc(selected.value)}" data-onchange="${esc(onChange || '')}">
    <button type="button" class="select-trigger" aria-label="${esc(ariaLabel)}" aria-haspopup="listbox" aria-expanded="false" data-act="toggle-select">
      <span class="select-label">${esc(selected.label)}</span><span class="select-chevron" aria-hidden="true"></span>
    </button>
    <div class="select-menu hidden" role="listbox" aria-label="${esc(ariaLabel)}">
      ${options.map(option => `<button type="button" class="select-option" role="option" data-value="${esc(option.value)}" data-label="${esc(option.label)}" aria-selected="${option.value === selected.value}" data-act="choose-select">${esc(option.label)}</button>`).join('')}
    </div>
  </div>`;
}
function closeCustomSelects(except='') {
  document.querySelectorAll('.custom-select').forEach(root => {
    if (root.id === except) return;
    root.querySelector('.select-menu')?.classList.add('hidden');
    root.querySelector('.select-trigger')?.setAttribute('aria-expanded','false');
    root.closest('.card')?.classList.remove('menu-open');
  });
}
function toggleCustomSelect(id, forceOpen) {
  const root = document.getElementById(id); if (!root) return;
  const menu = root.querySelector('.select-menu');
  const open = forceOpen ?? menu.classList.contains('hidden');
  const card = root.closest('.card');
  if (open) closeDeviceMenu();
  closeCustomSelects(open ? id : '');
  // 展开时抬升所在卡片层叠层级，避免菜单被相邻卡片遮挡
  card?.classList.toggle('menu-open', open);
  menu.classList.toggle('hidden', !open); root.querySelector('.select-trigger').setAttribute('aria-expanded', String(open));
  if (open) requestAnimationFrame(() => (menu.querySelector('[aria-selected="true"]') || menu.querySelector('.select-option'))?.focus());
}
function chooseCustomSelect(id, value, notify=true) {
  const root = document.getElementById(id); if (!root) return;
  const options = [...root.querySelectorAll('.select-option')];
  const selected = options.find(option => option.dataset.value === value) || options[0]; if (!selected) return;
  root.dataset.value = selected.dataset.value; root.querySelector('.select-label').textContent = selected.dataset.label;
  options.forEach(option => option.setAttribute('aria-selected', String(option === selected)));
  toggleCustomSelect(id, false);
  root.querySelector('.select-trigger').focus({preventScroll:true});
  if (notify && root.dataset.onchange && typeof window[root.dataset.onchange] === 'function') window[root.dataset.onchange](selected.dataset.value);
}
function customSelectTriggerKeydown(event, id) {
  if (['ArrowDown','ArrowUp','Enter',' '].includes(event.key)) { event.preventDefault(); toggleCustomSelect(id, true); }
}
function customSelectOptionKeydown(event, option, id) {
  const options = [...option.parentElement.querySelectorAll('.select-option')]; const index = options.indexOf(option);
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); options[(index + (event.key === 'ArrowDown' ? 1 : -1) + options.length) % options.length].focus(); }
  else if (event.key === 'Home' || event.key === 'End') { event.preventDefault(); options[event.key === 'Home' ? 0 : options.length - 1].focus(); }
  else if (event.key === 'Escape') { event.preventDefault(); toggleCustomSelect(id, false); document.querySelector(`#${CSS.escape(id)} .select-trigger`)?.focus(); }
}
function customSelectValue(id) { return document.getElementById(id)?.dataset.value || ''; }
document.addEventListener('pointerdown', event => {
  if (!event.target.closest('.custom-select')) closeCustomSelects();
  if (!event.target.closest('#device-nav')) closeDeviceMenu();
  if (!event.target.closest('.swipe-row')) closeAllSwipeRows();
});
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && !document.getElementById('device-menu')?.classList.contains('hidden')) closeDeviceMenu(true);
});

let modalReturnFocus = null;
let modalPageScrollY = 0;
let modalBodyInlineStyle = null;
let modalTouchY = null;
function lockPageScroll() {
  const body = document.body;
  if (body.classList.contains('modal-open')) return;
  modalPageScrollY = window.scrollY || window.pageYOffset || 0;
  modalBodyInlineStyle = {
    position:body.style.position, top:body.style.top, left:body.style.left,
    right:body.style.right, width:body.style.width, paddingRight:body.style.paddingRight
  };
  const scrollbarGap = Math.max(0, window.innerWidth - document.documentElement.clientWidth);
  body.style.position = 'fixed';
  body.style.top = `-${modalPageScrollY}px`;
  body.style.left = '0';
  body.style.right = '0';
  body.style.width = '100%';
  if (scrollbarGap) body.style.paddingRight = `${scrollbarGap}px`;
  document.documentElement.classList.add('modal-open');
  body.classList.add('modal-open');
}
function unlockPageScroll() {
  const body = document.body;
  if (!body.classList.contains('modal-open')) return;
  const scrollY = modalPageScrollY;
  document.documentElement.classList.remove('modal-open');
  body.classList.remove('modal-open');
  if (modalBodyInlineStyle) Object.assign(body.style, modalBodyInlineStyle);
  modalBodyInlineStyle = null;
  window.scrollTo(0, scrollY);
}
function openModal(html,preserveVersionRequest=false,compact=false) {
  if (!preserveVersionRequest) versionRequestId++;
  stopVersionObserver();
  const mask = document.getElementById('modal-mask'), modal = document.getElementById('modal-box');
  modal.classList.toggle('compact',compact);
  if (mask.style.display !== 'flex') { modalReturnFocus = document.activeElement; lockPageScroll(); }
  document.getElementById('modal-body').innerHTML = html; mask.style.display = 'flex';
  requestAnimationFrame(() => (document.getElementById('modal-body').querySelector('input,[data-autofocus]') || document.getElementById('modal-box')).focus({preventScroll:true}));
}
function versionDetailIsOpen() {
  const detailView=document.getElementById('version-detail-view');
  return Boolean(detailView && !detailView.classList.contains('hidden'));
}
function closeModal(force=false) {
  if (!force && versionDetailIsOpen()) { backToVersionList(); return; }
  versionRequestId++; stopVersionObserver(); document.getElementById('modal-mask').style.display = 'none'; unlockPageScroll(); closeCustomSelects();
  if (modalReturnFocus && document.contains(modalReturnFocus)) modalReturnFocus.focus({preventScroll:true}); modalReturnFocus = null;
}
function modalScrollContainer(target, modal) {
  let node = target instanceof Element ? target : target?.parentElement;
  while (node && node !== modal) {
    const overflowY = getComputedStyle(node).overflowY;
    if ((overflowY === 'auto' || overflowY === 'scroll') && node.scrollHeight > node.clientHeight + 1) return node;
    node = node.parentElement;
  }
  return modal;
}
function beginModalTouch(event) {
  modalTouchY = event.touches.length === 1 ? event.touches[0].clientY : null;
}
function guardModalTouchMove(event) {
  if (event.touches.length !== 1 || modalTouchY === null) return;
  const currentY = event.touches[0].clientY;
  const deltaY = currentY - modalTouchY;
  modalTouchY = currentY;
  const modal = document.getElementById('modal-box');
  if (!modal.contains(event.target)) { event.preventDefault(); return; }
  const scroller = modalScrollContainer(event.target, modal);
  const cannotScroll = scroller.scrollHeight <= scroller.clientHeight + 1;
  const atTop = scroller.scrollTop <= 0;
  const atBottom = scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 1;
  if (cannotScroll || (deltaY > 0 && atTop) || (deltaY < 0 && atBottom)) event.preventDefault();
}
function endModalTouch() { modalTouchY = null; }
const modalMask = document.getElementById('modal-mask');
modalMask.addEventListener('click', event => { if (event.target === event.currentTarget) closeModal(); });
modalMask.addEventListener('touchstart', beginModalTouch, {passive:true});
modalMask.addEventListener('touchmove', guardModalTouchMove, {passive:false});
modalMask.addEventListener('touchend', endModalTouch, {passive:true});
modalMask.addEventListener('touchcancel', endModalTouch, {passive:true});
document.addEventListener('keydown', event => {
  const mask = document.getElementById('modal-mask'); if (mask.style.display !== 'flex') return;
  if (event.key === 'Escape') { closeModal(); return; }
  if (event.key !== 'Tab') return;
  const focusable = [...document.getElementById('modal-box').querySelectorAll('button:not([disabled]),input:not([disabled])')].filter(node => node.offsetParent !== null);
  if (!focusable.length) return; const first = focusable[0], last = focusable.at(-1);
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
});

let accounts = [];
function flag(country) { return country?.length === 2 ? String.fromCodePoint(...[...country.toUpperCase()].map(char => 127397 + char.charCodeAt(0))) : '🏳'; }
async function refreshAccounts() {
  try { accounts = await api('/api/auth/accounts'); } catch { accounts = []; }
  const box = document.getElementById('accounts');
  if (!accounts.length) {
    box.innerHTML = `<div class="empty-state"><div>暂无账户</div><button type="button" data-act="open-login">登录 Apple ID</button></div>`;
  } else {
    box.innerHTML = accounts.map(account => `<div class="account-row"><span style="font-size:20px">${flag(account.country)}</span><div class="account-main">
      <div style="font-weight:650">${esc(account.email)}${account.current ? ' <span class="pill" style="color:var(--ui-accent);background:var(--ui-accent-soft)">当前</span>' : ''}</div>
      <div class="dim" style="font-size:12px">${esc(account.country || '地区未识别')}${account.name ? ' · ' + esc(account.name) : ''}</div></div>
      <div class="account-actions">${account.current ? '<button type="button" class="ghost" data-act="refresh-session">验证</button>' : `<button type="button" class="ghost" data-act="select-account" data-email="${arg(account.email)}">切换</button>`}
      <button type="button" class="danger" data-act="logout" data-email="${arg(account.email)}">退出</button></div></div>`).join('');
  }
  rebuildCountryOptions();
}
function rebuildCountryOptions() {
  const countries = [...new Set(accounts.map(account => account.country).filter(Boolean))];
  if (!countries.length) countries.push('CN');
  const current = accounts.find(account => account.current)?.country || countries[0];
  document.getElementById('search-country-slot').innerHTML = customSelect('search-country', current, countries.map(country => ({value:country,label:`${flag(country)} ${country}`})), 'onCountryChange', 'App Store 地区');
}
async function selectAccount(email) {
  try { await api('/api/auth/select',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email})}); await refreshAccounts(); }
  catch (error) { toast('❌ ' + error.message,true); }
}
async function onCountryChange(country) {
  const target = accounts.find(account => account.country === country);
  if (target && !target.current) await selectAccount(target.email);
  if (document.getElementById('search-term').value.trim()) doSearch();
}
async function refreshSession() {
  const message = document.getElementById('refresh-msg'); if (message) message.textContent = '验证中…';
  try { const result = await api('/api/auth/refresh',{method:'POST'}); if (message) message.textContent=''; showAuthBanner(false); toast(`✅ 登录状态有效：${result.email}`); }
  catch (error) { if (message) message.textContent=''; if (handleAuthErr(error)) openLoginModal(error.codeRequired); toast('❌ ' + error.message,true); }
}
let pendingDownload=null;
const prompted2faJobs=new Set();
function openLoginModal(requireCode=false,email='',notice='') {
  const selectedEmail=email || accounts.find(account=>account.current)?.email || '';
  openModal(`<h3>${requireCode ? '完成双重认证' : '登录 Apple ID'}</h3><div class="login-form">
    <input id="m-email" type="email" placeholder="Apple ID 邮箱" autocomplete="username" value="${esc(selectedEmail)}">
    <input id="m-password" type="password" placeholder="密码" autocomplete="current-password">
    <input id="m-code" class="${requireCode ? '' : 'hidden'}" type="text" placeholder="双重验证码（6 位数字）" inputmode="numeric" autocomplete="one-time-code" maxlength="6">
    <div id="login-msg" class="dim" style="min-height:18px;font-size:13px">${esc(notice || (requireCode ? '请输入密码和受信任设备上显示的 6 位验证码' : ''))}</div><button id="login-btn" type="button" data-act="do-login">${requireCode ? '验证并继续' : '登录'}</button>
    <div class="dim" style="font-size:12px">建议使用小号 Apple ID。首次登录需要初始化签名组件，可能需要几分钟。</div></div>`,false,true);
  ['m-email','m-password','m-code'].forEach(id => document.getElementById(id).addEventListener('keydown',event => { if (event.key === 'Enter') doLogin(); }));
  if (requireCode && selectedEmail) requestAnimationFrame(()=>document.getElementById('m-password')?.focus({preventScroll:true}));
}
async function doLogin() {
  const email = document.getElementById('m-email').value.trim(), password = document.getElementById('m-password').value, code = document.getElementById('m-code').value.trim();
  const message = document.getElementById('login-msg'), button = document.getElementById('login-btn');
  if (!email || !password) { message.className='err'; message.textContent='请输入邮箱和密码'; return; }
  if (!document.getElementById('m-code').classList.contains('hidden') && !/^\d{6}$/.test(code)) { message.className='err'; message.textContent='请输入 6 位数字双重验证码'; return; }
  button.disabled=true; message.className='dim'; message.textContent='登录中…首次初始化可能需要几分钟';
  try {
    const result = await api('/api/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email,password,code:code || undefined})});
    if (result.code_required) { const codeInput=document.getElementById('m-code'); codeInput.classList.remove('hidden'); document.querySelector('#modal-body h3').textContent='完成双重认证'; message.textContent='请输入受信任设备上显示的 6 位验证码'; button.textContent='验证并继续'; button.disabled=false; codeInput.focus(); return; }
    const retry=pendingDownload; pendingDownload=null;
    closeModal(); showAuthBanner(false); await refreshAccounts(); if (result.cleared_jobs) await pollJobs(); toast(`✅ 登录成功：${result.email}`);
    if (retry) { toast('✅ 认证成功，正在重新发起下载'); await startDownload(retry.bundleId,retry.versionId,retry.platform,null,retry.size); }
  } catch (error) { message.className='err'; message.textContent='❌ ' + error.message; button.disabled=false; }
}
async function logout(email) {
  if (!confirm(`退出登录 ${email}？`)) return;
  try { await api('/api/auth/logout',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({email})}); showAuthBanner(false); await refreshAccounts(); }
  catch (error) { toast('❌ ' + error.message,true); }
}

const VALID_PLATFORMS=['iphone','ipad','appletv'];
function storedPlatform() {
  try { const value=localStorage.getItem('ipadown-platform'); return VALID_PLATFORMS.includes(value) ? value : 'iphone'; }
  catch { return 'iphone'; }
}
let searchPlatform=storedPlatform();
function platformLabel(platform) { return {iphone:'iPhone',ipad:'iPad',appletv:'Apple TV'}[platform] || 'iPhone'; }
function platformOsLabel(platform) { return platform==='appletv' ? 'tvOS' : (platform==='ipad' ? 'iPadOS' : 'iOS'); }
function platformIcon(platform) {
  if (platform==='ipad') return '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="4" y="2.5" width="16" height="19" rx="2.2"></rect><path d="M11 18.5h2"></path></svg>';
  if (platform==='appletv') return '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="2.5" y="4" width="19" height="13" rx="2"></rect><path d="M9 21h6M12 17v4"></path></svg>';
  return '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="7" y="2.5" width="10" height="19" rx="2.2"></rect><path d="M10 5h4M11 18.5h2"></path></svg>';
}
function closeDeviceMenu(returnFocus=false) {
  const menu=document.getElementById('device-menu'), trigger=document.getElementById('device-trigger');
  if (!menu || !trigger) return;
  menu.classList.add('hidden'); trigger.setAttribute('aria-expanded','false');
  if (returnFocus) trigger.focus({preventScroll:true});
}
function toggleDeviceMenu(forceOpen) {
  const menu=document.getElementById('device-menu'), trigger=document.getElementById('device-trigger'); if (!menu || !trigger) return;
  const open=forceOpen ?? menu.classList.contains('hidden');
  closeCustomSelects();
  menu.classList.toggle('hidden',!open); trigger.setAttribute('aria-expanded',String(open));
  if (open) requestAnimationFrame(() => menu.querySelector('.active')?.focus({preventScroll:true}));
}
function deviceTriggerKeydown(event) {
  if (['ArrowDown','ArrowUp','Enter',' '].includes(event.key)) { event.preventDefault(); toggleDeviceMenu(true); }
}
function deviceOptionKeydown(event,option) {
  const options=[...option.parentElement.querySelectorAll('.device-menu-option')], index=options.indexOf(option);
  if (event.key==='ArrowDown' || event.key==='ArrowUp') { event.preventDefault(); options[(index+(event.key==='ArrowDown'?1:-1)+options.length)%options.length].focus(); }
  else if (event.key==='Home' || event.key==='End') { event.preventDefault(); options[event.key==='Home'?0:options.length-1].focus(); }
  else if (event.key==='Escape') { event.preventDefault(); closeDeviceMenu(true); }
  else if (event.key==='Tab') closeDeviceMenu();
}
function renderPlatformSwitch() {
  document.querySelectorAll('.device-menu-option[data-platform]').forEach(button => {
    const active=button.dataset.platform===searchPlatform;
    button.classList.toggle('active',active);
    button.setAttribute('aria-selected',String(active));
  });
  const trigger=document.getElementById('device-trigger');
  if (trigger) {
    trigger.innerHTML=platformIcon(searchPlatform);
    trigger.setAttribute('aria-label',`设备：${platformLabel(searchPlatform)}`);
    trigger.title=platformLabel(searchPlatform);
  }
}
function selectPlatform(platform) {
  if (!VALID_PLATFORMS.includes(platform)) return;
  const changed=platform!==searchPlatform;
  closeDeviceMenu(true);
  if (!changed) return;
  searchPlatform=platform;
  try { localStorage.setItem('ipadown-platform',platform); } catch {}
  renderPlatformSwitch();
  searchRequestId++;
  searchApps.clear();
  document.getElementById('search-card').classList.add('hidden');
  document.getElementById('search-results').innerHTML='';
  if (document.getElementById('search-term').value.trim()) doSearch();
}

let searchRequestId=0;
async function doSearch() {
  const requestId=++searchRequestId;
  const term = document.getElementById('search-term').value.trim(); if (!term) return;
  const country = customSelectValue('search-country') || 'CN', platform=searchPlatform, card = document.getElementById('search-card'), box = document.getElementById('search-results');
  card.classList.remove('hidden'); box.innerHTML='<div class="dim" style="padding:10px 0">搜索中…</div>';
  try {
    const list = await api(`/api/search?term=${encodeURIComponent(term)}&country=${encodeURIComponent(country)}&platform=${encodeURIComponent(platform)}`);
    if (requestId!==searchRequestId) return;
    if (!list.length) { box.innerHTML='<div class="dim" style="padding:10px 0">没有找到应用</div>'; return; }
    searchApps.clear();
    list.forEach(app => searchApps.set(`${platform}:${country}:${app.bundle_id}`,app));
    box.innerHTML = list.map(app => `<div class="app-row">${app.artwork_url ? `<img src="${esc(app.artwork_url)}" loading="lazy" alt="">` : '<img alt="">'}
      <div class="app-info"><div class="app-name">${esc(app.name)}</div><div class="app-meta">${esc(app.bundle_id)} · ${esc(app.formatted_price ?? (app.price ? app.price : '免费'))} · 当前 ${esc(app.version)}</div></div>
      <button type="button" class="ghost" data-act="show-versions" data-bundle="${arg(app.bundle_id)}" data-app-id="${Number(app.id)}" data-country="${arg(country)}" data-platform="${arg(platform)}">历史版本</button></div>`).join('');
  } catch (error) { if (requestId===searchRequestId) box.innerHTML=`<div class="err" style="padding:10px 0">❌ ${esc(error.message)}</div>`; }
}

const searchApps=new Map();
const verState={bundleId:'',appId:0,country:'CN',platform:'iphone',app:{},entries:[],rendered:0,scrollTop:0,detailVersionId:null};
const VERSION_BATCH_SIZE=20;
let versionObserver=null;
let versionRequestId=0;
function compareVersionsNewestFirst(left,right) {
  const leftDate=Date.parse(left.release_date || ''), rightDate=Date.parse(right.release_date || '');
  if (Number.isFinite(leftDate) && Number.isFinite(rightDate) && leftDate !== rightDate) return rightDate-leftDate;
  if (Number.isFinite(leftDate) !== Number.isFinite(rightDate)) return Number.isFinite(leftDate) ? -1 : 1;
  return String(right.id || '').localeCompare(String(left.id || ''),undefined,{numeric:true,sensitivity:'base'});
}
async function showVersions(bundleId,appId,country,platform='iphone') {
  const requestId=++versionRequestId;
  const searchApp=searchApps.get(`${platform}:${country}:${bundleId}`) || {};
  openModal('<h3>历史版本</h3><div class="dim">加载中…</div>',true);
  try {
    const query=`bundle_id=${encodeURIComponent(bundleId)}&app_id=${appId}&country=${encodeURIComponent(country)}&platform=${encodeURIComponent(platform)}`;
    const result = await api(`/api/versions?${query}`);
    if (requestId!==versionRequestId) return;
    const entries=(result.versions || []).map(item => ({...item,id:String(item.version_id)})).sort(compareVersionsNewestFirst);
    Object.assign(verState,{bundleId,appId,country,platform,app:{...searchApp,...result.app},entries,rendered:0,scrollTop:0,degraded:!!result.degraded}); renderVersions();
  } catch (error) { if (requestId!==versionRequestId) return; handleAuthErr(error); openModal(`<h3>历史版本</h3><div class="err">❌ ${esc(error.message)}</div>`,true); }
}
function renderVersions() {
  stopVersionObserver(); verState.rendered=0;
  openModal(`<div id="version-list-view"><h3>${esc(verState.app.name || '应用')} · 历史版本</h3>
    <div class="list-summary">${verState.entries.length} 个版本${verState.degraded ? ' · 仅版本 ID（目录服务降级中）' : ''}</div>
    <div class="version-list" id="version-list"></div><div class="version-sentinel" id="version-sentinel" aria-live="polite"></div></div>
    <div id="version-detail-view" class="hidden"></div>`,true);
  appendVersionBatch();
}
function versionItemHtml(entry) {
  const version=entry.display_version || (verState.degraded ? '' : '版本未知'), date=String(entry.release_date || '').slice(0,10) || (verState.degraded ? '' : '日期未知');
  return `<button type="button" class="version-item" data-version-id="${esc(entry.id)}" data-act="show-version-detail">
    <span><span class="version-name">${esc(version)}</span><code class="version-id">${esc(entry.id)}</code></span>
    <span class="version-meta">${date ? esc(date) : ''}${entry.size ? `${date ? ' · ' : ''}${esc(entry.size)}` : ''}</span><span class="version-chevron" aria-hidden="true">›</span></button>`;
}
function stopVersionObserver() {
  versionObserver?.disconnect(); versionObserver=null;
}
function observeVersionSentinel() {
  stopVersionObserver();
  const sentinel=document.getElementById('version-sentinel'); if (!sentinel || verState.rendered>=verState.entries.length) return;
  if (!('IntersectionObserver' in window)) { sentinel.innerHTML='<button type="button" class="ghost" data-act="append-versions">继续加载</button>'; return; }
  versionObserver=new IntersectionObserver(entries => { if (entries.some(entry=>entry.isIntersecting)) appendVersionBatch(); },{root:document.getElementById('modal-box'),rootMargin:'220px 0px'});
  versionObserver.observe(sentinel);
}
function appendVersionBatch() {
  const list=document.getElementById('version-list'), sentinel=document.getElementById('version-sentinel'); if (!list || !sentinel) return;
  const end=Math.min(verState.rendered+VERSION_BATCH_SIZE,verState.entries.length);
  list.insertAdjacentHTML('beforeend',verState.entries.slice(verState.rendered,end).map(versionItemHtml).join(''));
  verState.rendered=end;
  if (end>=verState.entries.length) { stopVersionObserver(); sentinel.textContent=verState.entries.length ? `已加载全部 ${verState.entries.length} 个版本` : '暂无历史版本'; }
  else { sentinel.textContent=`已加载 ${end} / ${verState.entries.length}`; observeVersionSentinel(); }
}
function detailRow(label,value,code=false) {
  return `<div class="detail-row"><span class="detail-label">${esc(label)}</span><span class="detail-value">${code ? `<code>${esc(value || '-')}</code>` : esc(value || '-')}</span></div>`;
}
function showVersionDetail(versionId) {
  const entry=verState.entries.find(item=>item.id===versionId); if (!entry) return;
  const listView=document.getElementById('version-list-view'), detailView=document.getElementById('version-detail-view'), modal=document.getElementById('modal-box');
  verState.scrollTop=modal.scrollTop; verState.detailVersionId=versionId; stopVersionObserver(); listView.classList.add('hidden'); detailView.classList.remove('hidden'); modal.scrollTop=0;
  const app=verState.app, price=app.formatted_price ?? (app.price === 0 ? '免费' : app.price ?? '-');
  detailView.innerHTML=`<div class="detail-hero">${app.artwork_url ? `<img src="${esc(app.artwork_url)}" alt="">` : '<img alt="">'}<div><h3 class="detail-title" tabindex="-1">${esc(app.name || '应用详细信息')}</h3><div class="dim">${esc(app.artist_name || app.bundle_id || '')}</div></div></div>
    <div class="detail-grid">${detailRow('选定版本',entry.display_version)}${detailRow('设备',platformLabel(verState.platform))}${detailRow('版本 ID',entry.id,true)}${detailRow('发布日期',String(entry.release_date || '').slice(0,10))}${detailRow('安装包大小',entry.size)}${detailRow('当前版本',app.version)}${detailRow('最低系统',app.minimum_os_version ? `${platformOsLabel(verState.platform)} ${app.minimum_os_version}` : '-')}${detailRow('App Store ID',String(verState.appId),true)}${detailRow('地区',`${flag(verState.country)} ${verState.country}`)}${detailRow('价格',String(price))}${detailRow('Bundle ID',verState.bundleId,true)}</div>
    ${entry.release_notes ? `<div class="release-notes"><div class="release-notes-title">更新日志</div><p class="release-notes-content">${esc(entry.release_notes)}</p></div>` : ''}
    <div class="detail-actions"><button type="button" class="ghost" data-act="back-to-versions">返回版本列表</button><button type="button" data-act="start-download" data-bundle="${arg(verState.bundleId)}" data-version-id="${arg(entry.id)}" data-platform="${arg(verState.platform)}" data-size="${parseSize(entry.size) || ''}">下载此版本</button></div>
    <div class="download-feedback hidden" id="download-feedback" role="status" aria-live="polite"></div>`;
  requestAnimationFrame(()=>detailView.querySelector('.detail-title')?.focus({preventScroll:true}));
}
function backToVersionList() {
  const listView=document.getElementById('version-list-view'), detailView=document.getElementById('version-detail-view'); if (!listView || !detailView) return;
  const versionId=verState.detailVersionId; verState.detailVersionId=null;
  detailView.classList.add('hidden'); listView.classList.remove('hidden');
  requestAnimationFrame(()=>{
    document.getElementById('modal-box').scrollTop=verState.scrollTop; observeVersionSentinel();
    if (versionId) document.querySelector(`[data-version-id="${CSS.escape(versionId)}"]`)?.focus({preventScroll:true});
  });
}
const startingDownloads=new Set();
async function startDownload(bundleId,versionId,platform='iphone',button=null,size=null) {
  const key=`${platform}\u0000${bundleId}\u0000${versionId || ''}`;
  if (startingDownloads.has(key)) { toast('正在准备这个版本，请稍候'); return; }
  startingDownloads.add(key);
  const feedback=document.getElementById('download-feedback');
  const previousLabel=button?.textContent || '下载此版本';
  if (button) {
    button.disabled=true; button.classList.add('is-loading'); button.setAttribute('aria-busy','true');
    button.innerHTML='<span class="button-spinner" aria-hidden="true"></span>正在准备…';
  }
  if (feedback) {
    feedback.classList.remove('hidden','err');
    feedback.textContent='正在创建下载任务，请稍候…';
  }
  try {
    const result=await api('/api/downloads',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({bundle_id:bundleId,version_id:versionId || undefined,platform,size:size || undefined})});
    toast(result.reused?'相同任务已在队列中':'✅ 已加入下载队列'); closeModal(true); showTab('download'); pollJobs();
  } catch (error) {
    toast('❌ '+error.message,true);
    if (feedback) { feedback.classList.add('err'); feedback.textContent=`创建下载任务失败：${error.message}`; }
    if (handleAuthErr(error)) {
      pendingDownload={bundleId,versionId,platform}; showAuthBanner(true,error.codeRequired);
      openLoginModal(error.codeRequired,'',error.codeRequired ? '下载已暂停。完成双重认证后会自动重试。' : '登录状态已失效，重新登录后会自动重试下载。');
    }
  } finally {
    startingDownloads.delete(key);
    if (button && document.contains(button)) {
      button.disabled=false; button.classList.remove('is-loading'); button.removeAttribute('aria-busy'); button.textContent=previousLabel;
    }
  }
}

let completedJobIds=null;
let renderedJobIds='';
const openSwipeRows=new Set();
const SWIPE_ACTION_WIDTH=74;
function jobStatusText(job,pct) {
  if (job.status==='done') return '完成';
  if (job.status==='error') return '失败';
  if (job.status==='queued') return '排队中';
  if (job.status==='paused') return '已暂停';
  return pct!=null?`${pct}%`:'下载中';
}
function rowJobFilename(row) {
  const suffix=row.dataset.platform==='ipad'?'_ipad':(row.dataset.platform==='appletv'?'_appletv':'');
  return `${row.dataset.bundle}_${row.dataset.versionId||'latest'}${suffix}.ipa`;
}
async function pollJobs() {
  let list; try { list=await api('/api/downloads'); } catch { return false; }
  const box=document.getElementById('jobs');
  const nextCompletedIds=new Set(list.filter(job=>job.status==='done').map(job=>job.id));
  if (completedJobIds && [...nextCompletedIds].some(id=>!completedJobIds.has(id))) refreshFiles();
  completedJobIds=nextCompletedIds;
  const ids=list.map(job=>job.id).join(',');
  if (!list.length) {
    if (renderedJobIds!=='') { box.innerHTML='<div class="dim">暂无任务</div>'; renderedJobIds=''; }
    return false;
  }
  if (ids!==renderedJobIds) {
    renderedJobIds=ids;
    box.innerHTML='';
    list.forEach(job => box.appendChild(buildJobRow(job)));
  }
  list.forEach(job => updateJobRow(job));
  const challenge=list.find(job=>job.needs_2fa && !prompted2faJobs.has(job.id));
  if (challenge) {
    prompted2faJobs.add(challenge.id); pendingDownload={bundleId:challenge.bundle_id,versionId:challenge.version_id || null,platform:challenge.platform || 'iphone',size:challenge.total || null};
    showAuthBanner(true,true);
    openLoginModal(true,challenge.account,'下载任务需要双重认证。验证成功后会自动重试。');
  }
  return list.some(job=>job.status==='queued' || job.status==='downloading');
}
function buildJobRow(job) {
  const row=document.createElement('div');
  row.className='swipe-row';
  row.dataset.jobId=job.id;
  row.dataset.status=job.status;
  row.dataset.bundle=job.bundle_id;
  row.dataset.versionId=job.version_id||'';
  row.dataset.platform=job.platform||'iphone';
  row.innerHTML=`<button type="button" class="swipe-action" data-act="delete-job" data-id="${esc(job.id)}">删除</button>
    <div class="swipe-content"><div class="job-row"><div class="job-layout"><div class="job-main"><div class="job-name"></div><div class="dim job-meta"></div></div><div class="bar"><i></i></div><div class="job-status dim"></div></div><div class="err job-error hidden"></div></div>`;
  makeSwipeable(row, () => jobRowTapped(row));
  updateJobRow(job);
  return row;
}
function updateJobRow(job) {
  const row=document.querySelector(`#jobs [data-job-id="${CSS.escape(job.id)}"]`); if (!row) return;
  if (row.dataset.status!==job.status) { row.replaceWith(buildJobRow(job)); return; }
  const done=job.status==='done', pct=job.total ? Math.min(100,Math.floor(job.downloaded/job.total*100)) : null;
  const statusEl=row.querySelector('.job-status');
  statusEl.textContent=jobStatusText(job,pct);
  statusEl.className=`job-status ${job.status==='error'?'err':(done?'ok':'dim')}`;
  const bar=row.querySelector('.bar > i');
  if (done) { bar.classList.remove('indet'); bar.style.width='100%'; }
  else if (pct!=null) { bar.classList.remove('indet'); bar.style.width=`${pct}%`; }
  else { bar.classList.add('indet'); bar.style.width=''; }
  const sizeText=job.total ? `${fmtSize(job.downloaded)} / ${fmtSize(job.total)}` : fmtSize(job.downloaded);
  row.querySelector('.job-name').textContent=job.app_name||job.bundle_id;
  row.querySelector('.job-meta').textContent=`${platformLabel(job.platform)} · ${job.version_id||'latest'} · ${sizeText}`;
  const errorEl=row.querySelector('.job-error');
  if (job.error) { errorEl.textContent=job.error; errorEl.classList.remove('hidden'); }
  else { errorEl.classList.add('hidden'); errorEl.textContent=''; }
}
function jobRowTapped(row) {
  const status=row.dataset.status, id=row.dataset.jobId;
  if (status==='queued'||status==='downloading') { pauseJob(id); return; }
  if (status==='paused') { resumeJob(id); return; }
  if (status==='done') installFile(rowJobFilename(row),null);
}
async function pauseJob(id) {
  try { await api(`/api/downloads/${encodeURIComponent(id)}/pause`,{method:'POST'}); toast('已暂停'); pollJobs(); }
  catch (error) { toast('❌ '+error.message,true); }
}
async function resumeJob(id) {
  try { await api(`/api/downloads/${encodeURIComponent(id)}/resume`,{method:'POST'}); toast('已继续下载'); pollJobs(); }
  catch (error) { toast('❌ '+error.message,true); }
}
async function deleteJob(id) {
  try { await api(`/api/downloads/${encodeURIComponent(id)}`,{method:'DELETE'}); toast('✅ 已删除'); pollJobs(); }
  catch (error) { toast('❌ '+error.message,true); }
}
// ---------- 左滑手势（任务/文件通用） ----------
function closeSwipeRow(row) {
  const content=row.querySelector('.swipe-content'), action=row.querySelector('.swipe-action');
  if (content) content.style.transform='';
  if (action) action.style.transform='';
  openSwipeRows.delete(row);
}
function closeAllSwipeRows(except=null) {
  [...openSwipeRows].forEach(row => { if (row!==except && document.contains(row)) closeSwipeRow(row); });
}
function makeSwipeable(row,onTap) {
  const content=row.querySelector('.swipe-content'), action=row.querySelector('.swipe-action');
  let startX=0, startY=0, dragging=false, base=0, moved=false;
  row.addEventListener('pointerdown', event => {
    if (event.pointerType==='mouse' && event.button!==0) return;
    dragging=true; moved=false;
    startX=event.clientX; startY=event.clientY;
    base=openSwipeRows.has(row)?-SWIPE_ACTION_WIDTH:0;
    content.style.transition='none'; action.style.transition='none';
  });
  row.addEventListener('pointermove', event => {
    if (!dragging) return;
    const dx=event.clientX-startX, dy=event.clientY-startY;
    if (!moved && Math.abs(dy)>Math.abs(dx) && Math.abs(dy)>8) { dragging=false; return; }
    if (Math.abs(dx)>6) moved=true;
    if (!moved) return;
    closeAllSwipeRows(row);
    const offset=Math.min(0,Math.max(-SWIPE_ACTION_WIDTH-24,base+dx));
    content.style.transform=`translateX(${offset}px)`;
    action.style.transform=`translateX(${offset}px)`;
  });
  const finish=event => {
    if (!dragging) return; dragging=false;
    content.style.transition=''; action.style.transition='';
    const dx=event.clientX-startX;
    const shouldOpen=(base+dx)<-SWIPE_ACTION_WIDTH/2;
    if (shouldOpen) openSwipeRows.add(row); else openSwipeRows.delete(row);
    const target=shouldOpen?-SWIPE_ACTION_WIDTH:0;
    content.style.transform=`translateX(${target}px)`;
    action.style.transform=`translateX(${target}px)`;
  };
  row.addEventListener('pointerup',finish);
  row.addEventListener('pointercancel',finish);
  row.addEventListener('click', event => {
    if (event.target.closest('.swipe-action')) return;
    if (openSwipeRows.has(row)) { closeSwipeRow(row); return; }
    if (moved) return;
    if (onTap) onTap();
  });
}
async function keepJobsUpdated() {
  const active=await pollJobs();
  setTimeout(keepJobsUpdated,active?1500:5000);
}
async function refreshFiles() {
  let list; try { list=await api('/api/files'); } catch { return; }
  const box=document.getElementById('files'); if (!list.length) { box.innerHTML='<div class="dim">暂无文件</div>'; return; }
  box.innerHTML=`<div class="list-summary">${list.length} 个文件 · ${fmtSize(list.reduce((sum,file)=>sum+file.size,0))}</div><div class="file-list">`+
    list.map(file => `<div class="swipe-row" data-name="${arg(file.name)}"><button type="button" class="swipe-action" data-act="delete-file" data-name="${arg(file.name)}">删除</button>
      <div class="swipe-content"><div class="file-row"><div class="file-icon">${file.artwork_url ? `<img src="${esc(file.artwork_url)}" loading="lazy" alt="">` : '↓'}</div>
      <div class="file-info" title="${esc(file.name)}"><div class="file-title">${esc(file.title||file.bundle_id||file.name)}</div><div class="file-meta">${platformLabel(file.platform)} · ${esc(file.version||'版本未知')} · ${fmtSize(file.size)}</div></div></div></div></div>`).join('')+'</div>';
  box.querySelectorAll('.swipe-row').forEach(row => makeSwipeable(row, () => installFile(decodeURIComponent(row.dataset.name),null)));
}
async function installFile(name,button=null) {
  if (!safariBrowser) { toast('安装必须使用 Safari',true); return; }
  const previous=button?.textContent;
  if (button) { button.disabled=true; button.textContent='正在唤起…'; }
  try {
    const result=await api(`/api/install/${encodeURIComponent(name)}`);
    if (!result.install?.https) throw new Error('安装地址不是受信 HTTPS，请检查 PUBLIC_BASE_URL');
    window.location.href=result.install.itms_url;
  } catch (error) { toast('❌ '+error.message,true); if (button) { button.disabled=false; button.textContent=previous; } }
  if (button) setTimeout(() => { if (document.contains(button)) { button.disabled=false; button.textContent=previous; } },2200);
}
async function deleteFile(name) {
  if (!confirm(`删除 ${name}？此操作不可撤销。`)) return;
  try { await api(`/api/files/${encodeURIComponent(name)}`,{method:'DELETE'}); toast('✅ 已删除'); refreshFiles(); }
  catch (error) { toast('❌ '+error.message,true); }
}

let appInitialized=false;
async function init() {
  if (!browserAllowed || appInitialized) return;
  appInitialized=true;
  const config=await api('/api/config').catch(()=>({}));
  if (!config.public_base_url && location.protocol !== 'https:') document.getElementById('https-warn').classList.remove('hidden');
  renderPlatformSwitch(); showTab(storedTab(),false); rebuildCountryOptions(); refreshAccounts(); refreshFiles(); keepJobsUpdated();
}
init();

// ---------- 事件委托（替代内联事件处理器，配合 CSP script-src 'self'） ----------
const ACTIONS = {
  'copy-safari-address': el => copySafariAddress(el),
  'toggle-device-menu': () => toggleDeviceMenu(),
  'select-platform': el => selectPlatform(el.dataset.platform),
  'open-login': () => openLoginModal(false),
  'show-tab': el => showTab(el.dataset.tab),
  'close-modal': () => closeModal(),
  'do-search': () => doSearch(),
  'do-login': () => doLogin(),
  'refresh-session': () => refreshSession(),
  'select-account': el => selectAccount(decodeURIComponent(el.dataset.email)),
  'logout': el => logout(decodeURIComponent(el.dataset.email)),
  'toggle-select': el => toggleCustomSelect(el.closest('.custom-select')?.id),
  'choose-select': el => chooseCustomSelect(el.closest('.custom-select')?.id, el.dataset.value),
  'show-versions': el => showVersions(decodeURIComponent(el.dataset.bundle), Number(el.dataset.appId), decodeURIComponent(el.dataset.country), decodeURIComponent(el.dataset.platform)),
  'show-version-detail': el => showVersionDetail(el.dataset.versionId),
  'back-to-versions': () => backToVersionList(),
  'start-download': el => startDownload(decodeURIComponent(el.dataset.bundle), decodeURIComponent(el.dataset.versionId), decodeURIComponent(el.dataset.platform), el, el.dataset.size ? Number(el.dataset.size) : null),
  'append-versions': () => appendVersionBatch(),
  'delete-job': el => deleteJob(el.dataset.id),
  'delete-file': el => deleteFile(decodeURIComponent(el.dataset.name)),
};
document.addEventListener('click', event => {
  const el = event.target instanceof Element ? event.target.closest('[data-act]') : null;
  if (!el) return;
  const action = ACTIONS[el.dataset.act];
  if (action) action(el);
});
document.addEventListener('keydown', event => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  if (target.id === 'search-term' && event.key === 'Enter') { event.preventDefault(); doSearch(); return; }
  if (target.id === 'device-trigger') { deviceTriggerKeydown(event); return; }
  if (target.matches('.device-menu-option')) { deviceOptionKeydown(event, target); return; }
  if (target.matches('.select-trigger')) { customSelectTriggerKeydown(event, target.closest('.custom-select')?.id); return; }
  if (target.matches('.select-option')) { customSelectOptionKeydown(event, target, target.closest('.custom-select')?.id); return; }
});
// error 事件不冒泡，需在捕获阶段接住（图标加载失败时回退为占位符）
document.addEventListener('error', event => {
  const target = event.target;
  if (target instanceof Element && target.matches('.file-icon img')) target.parentElement.textContent = '↓';
}, true);
