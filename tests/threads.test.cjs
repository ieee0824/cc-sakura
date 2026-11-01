const {test}=require('node:test');
const assert=require('node:assert/strict');
const {replyIds,build}=require('../src/web/threads.js');
const post=(id,reply_to)=>({id,payload:{reply_to}});

test('replies nest under their primary parent, ordered oldest first; active threads move up',()=>{
  const p=build([post(1),post(2),post(3,1),post(4,3),post(5,1)]);
  assert.deepEqual(p.roots,[1,2]);
  assert.deepEqual(p.children.get(1),[3,5]);
  assert.deepEqual(p.children.get(3),[4]);
  assert.equal(p.counts.get(1),3);
  assert.equal(p.latest.get(1),5);
});
test('multiple targets use the first reference once, without duplicating the reply',()=>{
  const p=build([post(1),post(2),post(3,[2,1,2])]);
  assert.equal(p.parents.get(3),2);
  assert.deepEqual(p.children.get(1),[]);
  assert.deepEqual(p.children.get(2),[3]);
  assert.deepEqual(replyIds(post(3,[2,1,2])),[2,1]);
});
test('missing primary parents stay visible and reconnect when history arrives',()=>{
  const orphan=post(4,[1,2]);
  const p=build([post(2),orphan]);
  assert.deepEqual(p.roots,[4,2]);
  assert.equal(p.parents.has(4),false);
  const reconnected=build([post(1),post(2),orphan]);
  assert.equal(reconnected.parents.get(4),1);
  assert.deepEqual(reconnected.children.get(1),[4]);
});
test('filters retain ancestor context, not unrelated siblings',()=>{
  const p=build([post(1),post(2,1),post(3,2),post(4,1),post(5)],[3]);
  assert.deepEqual([...p.visible].sort(),[1,2,3]);
  assert.deepEqual(p.roots,[1]);
  assert.equal(p.matched.has(1),false);
  assert.equal(p.counts.get(1),2);
});
test('invalid and forward references cannot lose messages or create cycles',()=>{
  const p=build([post(1,2),post(2,[2,1,-1,'x']),post(3,'1')]);
  assert.deepEqual(p.roots,[1]);
  assert.deepEqual(p.children.get(1),[2,3]);
  assert.deepEqual(replyIds(post(5,['2','2',null,{},Infinity,0,5,6])),[2]);
});
test('deep histories are processed iteratively, and an empty filter stays empty',()=>{
  const messages=Array.from({length:10000},(_,i)=>post(i+1,i||undefined));
  const p=build(messages,[10000]);
  assert.deepEqual(p.roots,[1]);
  assert.equal(p.counts.get(1),9999);
  assert.equal(p.visible.size,10000);
  assert.equal(build(messages,[]).roots.length,0);
});

test('large conversations only enumerate the requested reply page',()=>{
  const {replies}=require('../src/web/threads.js');
  const messages=[post(1),...Array.from({length:10000},(_,i)=>post(i+2,1))];
  const p=build(messages);
  const first=replies(p,1,20);
  const more=replies(p,1,40);
  assert.equal(first.length,20);
  assert.equal(more.length,40);
  assert.deepEqual(first.map(m=>m.id),Array.from({length:20},(_,i)=>i+2));
  assert.equal(p.counts.get(1),10000); // Counts never require rendering the replies.
  assert.equal(p.rootFor.get(10001),1);
});
test('reply pagination retains traversal and depth without walking the complete chain',()=>{
  const {replies}=require('../src/web/threads.js');
  const messages=Array.from({length:10000},(_,i)=>post(i+1,i||undefined));
  const p=build(messages);
  assert.deepEqual(replies(p,1,3),[{id:2,depth:1},{id:3,depth:2},{id:4,depth:3}]);
  assert.equal(p.rootFor.get(10000),1);
  assert.equal(replies(p,1,0).length,0);
});
