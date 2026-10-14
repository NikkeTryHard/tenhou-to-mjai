(() => {
  const pending = new Map();
  let sequence = 0;
  const post = Worker.prototype.postMessage;
  const terminate = Worker.prototype.terminate;
  const active = new WeakMap();
  globalThis.__koromoGpuReply = (id, nonce) => {
    const task = pending.get(id);
    if (!task) return;
    pending.delete(id);
    active.delete(task.worker);
    if (nonce === null) post.apply(task.worker, task.args);
    else task.worker.dispatchEvent(new MessageEvent('message', {
      data: {found: true, nonce}
    }));
  };
  Worker.prototype.postMessage = function (...args) {
    const data = args[0];
    // Leave instrumentation, RSW, initialization and unknown protocols alone.
    if (data && data.kind === undefined && typeof data.salt === 'string' &&
        typeof data.target === 'string' && data.salt.length <= 128 &&
        /^[0-9a-f]{1,64}$/i.test(data.target)) {
      const id = ++sequence;
      pending.set(id, {worker: this, args});
      active.set(this, id);
      globalThis.__koromoGpu(JSON.stringify({id, salt: data.salt, target: data.target}));
      return;
    }
    return post.apply(this, args);
  };
  Worker.prototype.terminate = function () {
    const id = active.get(this);
    if (id !== undefined) pending.delete(id);
    active.delete(this);
    return terminate.call(this);
  };
})();
