'use strict';
// Shared by the browser and Node tests. A reply can only point to an earlier
// message, so malformed/self/future links cannot create cycles in the tree.
(function (root) {
  function replyIds(message) {
    const value = message.payload?.reply_to;
    const items = Array.isArray(value) ? value : [value];
    return [...new Set(items.map(id => typeof id === 'string' && /^\d+$/.test(id) ? Number(id) : id)
      .filter(id => Number.isSafeInteger(id) && id > 0 && id < message.id))];
  }
  function build(messages, matchedIds) {
    const posts = new Map(messages.map(m => [m.id,m]));
    const matched = new Set(matchedIds === undefined ? posts.keys() : matchedIds);
    const visible = new Set([...matched].filter(id => posts.has(id)));
    const parents = new Map();
    for (const m of posts.values()) {
      const id = replyIds(m)[0];
      if (posts.has(id)) parents.set(m.id,id);
    }
    // Include the ancestors of search/author matches to retain conversation
    // context, without also including unrelated replies by other authors.
    for (const id of [...visible]) {
      let parent = parents.get(id);
      while (parent !== undefined && !visible.has(parent)) {
        visible.add(parent); parent = parents.get(parent);
      }
    }
    const children = new Map([...visible].map(id => [id,[]]));
    const latest = new Map([...visible].map(id => [id,id]));
    const counts = new Map([...visible].map(id => [id,0]));
    const roots = [];
    for (const id of visible) {
      const parent = parents.get(id);
      if (parent === undefined) roots.push(id);
      else children.get(parent).push(id);
    }
    for (const ids of children.values()) ids.sort((a,b) => a-b);
    // Parent IDs are lower than child IDs; this is an iterative postorder even
    // for long chains, with no recursive traversal or quadratic ancestor scans.
    for (const id of [...visible].sort((a,b) => b-a)) {
      const parent = parents.get(id);
      if (parent !== undefined) {
        counts.set(parent,counts.get(parent)+counts.get(id)+1);
        latest.set(parent,Math.max(latest.get(parent),latest.get(id)));
      }
    }
    roots.sort((a,b) => latest.get(b)-latest.get(a));
    const rootFor = new Map();
    for (const id of [...visible].sort((a,b) => a-b)) rootFor.set(id,rootFor.get(parents.get(id)) ?? id);
    return {posts,matched,visible,parents,children,roots,latest,counts,rootFor};
  }
  function replies(plan,root,limit) {
    const result=[];
    const stack=plan.children.get(root).slice().reverse().map(id=>({id,depth:1}));
    while (stack.length && result.length<limit) {
      const item=stack.pop();result.push(item);
      const children=plan.children.get(item.id);
      for (let i=children.length-1;i>=0;i--) stack.push({id:children[i],depth:item.depth+1});
    }
    return result;
  }
  const api = {replyIds,build,replies};
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.SakuraThreads = api;
})(globalThis);
