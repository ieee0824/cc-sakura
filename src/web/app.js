'use strict';
const $ = id => document.getElementById(id);
const posts = new Map();
const translations = new Map();
const translating = new Set();
const translationErrors = new Map();
const collapsedThreads = new Set();
const unreadReplies = new Set();
const {replyIds,build:buildThreads} = SakuraThreads;
let newest = 0, oldest = null, pending = 0, busy = false, more = true;
const labels = {message:'メッセージ',scope:'担当範囲',status:'状況',request:'依頼',proposal:'提案',evidence:'根拠',results:'検証結果',tests:'テスト',test:'テスト',next:'次の作業',finding:'発見',review:'レビュー',recommendation:'推奨',coordination:'担当調整',change:'変更内容',observed:'観測結果',conclusion:'結論',action:'実施内容',local:'ローカル検証',ci:'CI',reply_to:'返信先',refs:'参照',open_question:'未解決の論点'};
const types = {assignment_claim:'担当宣言',claim:'担当宣言',assignment_request:'担当依頼',verification_update:'検証報告',verification_and_scope_update:'検証・担当更新',design_review:'設計レビュー',design_review_request:'レビュー依頼',coordination:'担当調整',coordination_notice:'周知',discussion_topic:'議題',fix_pushed:'修正をpush',ci_observation:'CIの観測',issue_created:'Issue作成',investigation_request:'調査依頼',scope_notice:'変更範囲の共有',overlap_check:'競合確認'};
function node(tag, text, className) { const n = document.createElement(tag); if (text !== undefined) n.textContent = text; if (className) n.className = className; return n; }
function color(sender) { let h = 0; for (const c of String(sender)) h = (h * 31 + c.charCodeAt(0)) | 0; return `color-${Math.abs(h) % 5}`; }
function initials(name) { return String(name || '?').split(/[-_\s]+/).slice(0,2).map(s=>s[0]||'').join('').toUpperCase(); }
async function api(path, options = {}) {
  const response = await fetch(`api/${path}`, {cache:'no-store',...options, headers:{'X-Sakura-Viewer':'1',...options.headers}});
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || `取得に失敗しました (${response.status})`);
  return data;
}
function connection(ok) { $('connection').textContent = ok ? '自動更新中' : '再接続待ち'; $('status-dot').classList.toggle('offline',!ok); }
function error(message) { $('error').textContent = message || ''; $('error').hidden = !message; }
function ingest(messages, markNew=false, paged=true) {
  let added = 0;
  for (const m of messages) {
    if (!Number.isSafeInteger(m.id) || m.id < 1) continue;
    if (!posts.has(m.id)) {added++; if (markNew && replyIds(m).length) unreadReplies.add(m.id);}
    posts.set(m.id,m); newest = Math.max(newest,m.id);
    if (paged) oldest = oldest === null ? m.id : Math.min(oldest,m.id);
  }
  return added;
}
function fieldValue(container, value) {
  if (typeof value === 'string') container.append(node('div',value,'field-content'));
  else if (Array.isArray(value) && value.every(v=>typeof v==='string'||typeof v==='number')) {
    const list = node('ul'); for (const item of value) list.append(node('li',String(item))); container.append(list);
  } else container.append(node('pre',JSON.stringify(value,null,2)));
}
function participantName(id) { for (const m of posts.values()) if (m.sender===id) return m.handle; return id; }
function replyLink(id) {
  const link = node('a',`↪ #${id}`,'reply-link'); link.href=`#post-${id}`;
  link.addEventListener('click',event=>{
    const target=$(`post-${id}`);
    if (!target) {event.preventDefault();error('参照先が未表示です。絞り込みを解除するか、過去の投稿を読み込んでください。');return;}
    for (let parent=target.parentElement;parent;parent=parent.parentElement) if (parent.matches('details.thread-replies')) {parent.open=true;const root=parent.closest('.thread').querySelector('.post').id;collapsedThreads.delete(Number(root.replace('post-','')));}
  });
  return link;
}
function showInterpretation(box,result) {
  const names = {translated:'AI翻訳',uncertain:'AI翻訳・推測',uninterpretable:'解読不能',provided:'送信者の解釈'};
  box.textContent = `${names[result.status] || 'AI翻訳'}\n${result.text}`; box.hidden=false; box.classList.remove('error');
}
function syncTranslation(article,m) {
  const button=article.querySelector('.translate-button');
  const box=article.querySelector('.interpretation');
  button.disabled=translating.has(m.id);
  button.textContent=button.disabled?'✧ 翻訳しています…':'✧ AIで翻訳';
  if (translations.has(m.id)) showInterpretation(box,translations.get(m.id));
  else if (translationErrors.has(m.id)) {box.textContent=translationErrors.get(m.id);box.hidden=false;box.classList.add('error');}
}
function referenceTag(kind,value) {
  const number=String(value).match(/(?:^|\/)(\d+)\/?$/)?.[1];
  const tag=node('span',number?`${kind} #${number}`:`${kind} ${value}`,'tag');
  tag.title=String(value);return tag;
}
function card(m) {
  const fragment = $('post-template').content.cloneNode(true);
  const article = fragment.querySelector('article'); article.id=`post-${m.id}`;
  const avatar = fragment.querySelector('.post-avatar'); avatar.textContent=initials(m.handle); avatar.classList.add(color(m.sender));
  fragment.querySelector('.post-handle').textContent=m.handle || '不明な投稿者';
  fragment.querySelector('.post-id').textContent=`#${m.id}`;
  const time=fragment.querySelector('time'); const date=new Date(m.created_at);
  time.textContent = Number.isNaN(date.getTime()) ? String(m.created_at) : new Intl.DateTimeFormat('ja-JP',{month:'numeric',day:'numeric',hour:'2-digit',minute:'2-digit'}).format(date);
  time.dateTime=String(m.created_at); time.title=Number.isNaN(date.getTime()) ? '' : date.toLocaleString('ja-JP');
  const tags=fragment.querySelector('.post-tags'); const payload=m.payload;
  if (payload && typeof payload==='object' && !Array.isArray(payload)) {
    if (typeof payload.type==='string') tags.append(node('span',types[payload.type] || payload.type,'tag'));
    if (payload.issue!==undefined && ['string','number'].includes(typeof payload.issue)) tags.append(referenceTag('Issue',payload.issue));
    if (payload.pr!==undefined && ['string','number'].includes(typeof payload.pr)) tags.append(referenceTag('PR',payload.pr));
    for (const id of replyIds(m)) tags.append(replyLink(id));
  }
  const body=fragment.querySelector('.post-body');
  if (m.human) body.append(node('p',m.human));
  else if (typeof payload==='string') body.append(node('p',payload));
  else if (payload && typeof payload==='object' && !Array.isArray(payload)) {
    for (const [key,value] of Object.entries(payload)) {
      if (['type','from','reply_to','issue','pr','discussion_id'].includes(key)) continue;
      const field=node('div',undefined,'field'); field.append(node('span',labels[key] || key,'field-label')); fieldValue(field,value); body.append(field);
    }
    if (!body.childNodes.length) body.append(node('pre',JSON.stringify(payload,null,2)));
  } else body.append(node('pre',JSON.stringify(payload,null,2)));
  const mentions=fragment.querySelector('.post-mentions');
  mentions.textContent=(m.mentions||[]).map(id=>`@${participantName(id)}`).join('  ');
  fragment.querySelector('.original pre').textContent=JSON.stringify(m,null,2);
  const button=fragment.querySelector('.translate-button');
  button.hidden=!!m.human;
  syncTranslation(article,m);
  button.addEventListener('click',async()=>{
    translating.add(m.id); translationErrors.delete(m.id); syncTranslation(article,m);
    try {const result=await api(`translate/${m.id}`,{method:'POST'}); translations.set(m.id,result);}
    catch(e) {translationErrors.set(m.id,e.message);}
    finally {translating.delete(m.id); const current=$(`post-${m.id}`); if (current) syncTranslation(current,m);}
  });
  return fragment;
}
function appendPost(container,id,plan) {
  const fragment=card(plan.posts.get(id));
  if (!plan.matched.has(id)) {
    fragment.querySelector('.post-tags').append(node('span','会話の文脈','tag context-tag'));
    fragment.querySelector('article').classList.add('context-post');
  }
  container.append(fragment);
}
function threadView(root,plan) {
  const section=node('section',undefined,'thread'); section.setAttribute('aria-label',`投稿 #${root} のスレッド`);
  appendPost(section,root,plan);
  const parent=replyIds(plan.posts.get(root))[0];
  if (parent && !plan.posts.has(parent)) {
    const notice=node('div',undefined,'missing-parent');
    notice.append(node('span',`返信先 #${parent} が未取得のため、単独で表示しています。`));
    const load=node('button','返信先を読み込む','quiet-button');load.type='button';
    load.addEventListener('click',async()=>{
      load.disabled=true;
      try {const result=await api(`message/${parent}`);ingest([result.message],false,false);render();error('');}
      catch(e) {error(e.message);load.disabled=false;}
    });
    notice.append(load);section.append(notice);
  }
  if (!plan.counts.get(root)) return section;
  const details=node('details',undefined,'thread-replies'); details.open=!collapsedThreads.has(root);
  const summary=node('summary'); summary.append(node('span',`${plan.counts.get(root)}件の返信`));
  const badge=node('span',undefined,'unread-badge');summary.append(badge);
  const group=node('div',undefined,'reply-group');
  const ids=[];
  const stack=plan.children.get(root).slice().reverse().map(id=>({id,target:group,depth:1}));
  while (stack.length) {
    const {id,target,depth}=stack.pop();ids.push(id);
    const wrapper=node('div',undefined,'thread-reply');appendPost(wrapper,id,plan);target.append(wrapper);
    const children=plan.children.get(id);
    if (children.length) {
      // Limit visual/DOM nesting on narrow screens and adversarially long chains.
      const nested=depth<3?node('div',undefined,'reply-children'):target;
      if (depth<3) wrapper.append(nested);
      for (const child of children.slice().reverse()) stack.push({id:child,target:nested,depth:depth+1});
    }
  }
  const updateBadge=()=>{const count=ids.filter(id=>unreadReplies.has(id)).length;badge.textContent=`新着 ${count}`;badge.hidden=!count;};
  updateBadge();
  summary.addEventListener('click',()=>{
    if (details.open) collapsedThreads.add(root);
    else {collapsedThreads.delete(root);for (const id of ids) unreadReplies.delete(id);updateBadge();}
  });
  details.append(summary,group);section.append(details);return section;
}
function render() {
  const query=$('search').value.trim().toLocaleLowerCase(); const author=$('author').value;
  const sorted=[...posts.values()].sort((a,b)=>b.id-a.id);
  const matches=sorted.filter(m=>(!author||m.sender===author)&&(!query||JSON.stringify(m).toLocaleLowerCase().includes(query)));
  const plan=buildThreads(sorted,matches.map(m=>m.id));
  const expanded=[...document.querySelectorAll('.post .original[open]')].map(el=>el.closest('.post').id);
  const fragment=document.createDocumentFragment();for (const root of plan.roots) fragment.append(threadView(root,plan));$('timeline').replaceChildren(fragment);
  for (const id of expanded) {const details=$(id)?.querySelector('.original');if (details) details.open=true;}
  $('timeline').setAttribute('aria-busy','false'); $('count').textContent=`${posts.size}件を読み込み済み${query||author?` · 一致 ${matches.length}件${plan.visible.size>matches.length?` ＋ 文脈 ${plan.visible.size-matches.length}件`:''}`:` · ${plan.roots.length}スレッド`}`;
  $('empty').hidden=matches.length!==0; $('empty').querySelector('h2').textContent=posts.size ? '一致する会話がありません' : 'まだ会話がありません';
  $('empty').querySelector('p').textContent=posts.size ? '検索条件や投稿者の絞り込みを変更してください。' : '新しい投稿が届くと、ここに表示されます。';
  const people=new Map(); for (const m of sorted) {if (!people.has(m.sender)) people.set(m.sender,{name:m.handle,count:0}); people.get(m.sender).count++;}
  $('author').replaceChildren(node('option','すべての投稿者')); $('author').firstChild.value='';
  const list=document.createDocumentFragment();
  for (const [id,p] of people) {
    const option=node('option',p.name); option.value=id; $('author').append(option);
    const person=node('div',undefined,'person'); person.append(node('span',initials(p.name),`person-avatar ${color(id)}`));
    const info=node('div'); info.append(node('div',p.name,'person-name'),node('div',`${p.count} 投稿`,'person-count')); person.append(info); list.append(person);
  }
  $('participants').replaceChildren(list); $('author').value=author;
  $('older').hidden=!more; pending=0; $('new-messages').hidden=true;
}
async function refresh(initial=false,force=false) {
  if (busy) return; busy=true; $('refresh').disabled=true;
  try {
    let added=0;
    if (initial) {const data=await api('history'); more=data.messages.length===100; added+=ingest(data.messages);}
    else {for (let page=0;page<10;page++) {const data=await api(`history?after=${newest}`); added+=ingest(data.messages,true); if (data.messages.length<100) break;}}
    error(''); connection(true); $('updated').textContent=`最終取得 ${new Date().toLocaleTimeString('ja-JP',{hour:'2-digit',minute:'2-digit',second:'2-digit'})}`;
    if (initial||force||(added && window.scrollY<160)) render();
    else if (added) {pending+=added; $('new-messages').textContent=`↑ 新しい投稿 ${pending}件`; $('new-messages').hidden=false;}
  } catch(e) {connection(false); error(e.message); $('timeline').setAttribute('aria-busy','false');}
  finally {busy=false; $('refresh').disabled=false;}
}
$('search').addEventListener('input',render); $('author').addEventListener('change',render);
$('refresh').addEventListener('click',()=>refresh(false,true));
$('new-messages').addEventListener('click',()=>{render();window.scrollTo({top:0,behavior:'smooth'});});
$('older').addEventListener('click',async()=>{
  if (busy || oldest===null) return; busy=true; $('older').disabled=true;
  try {const data=await api(`history?before=${oldest}`); more=data.messages.length===100; ingest(data.messages); render(); error('');}
  catch(e) {error(e.message);} finally {busy=false;$('older').disabled=false;}
});
(async()=>{
  try {const info=await api('info'); $('room').textContent=info.room; $('side-room').textContent=info.room; document.title=`${info.room} · sakura`;}
  catch(e) {connection(false);error(e.message);}
  await refresh(true);
  setInterval(()=>{if (!document.hidden) refresh(posts.size===0);},5000);
})();
