// Only the private bridge is installed in this QuickJS runtime.
let ordinal = 0;
async function agent(prompt, opts = {}) {
  return await tools.workflow_bridge({op: 'agent', ordinal: ordinal++, prompt, opts});
}
async function parallel(tasks) {
  if (!Array.isArray(tasks)) throw new Error('parallel expects an array of functions');
  return await Promise.all(tasks.map(task => {
    if (typeof task !== 'function') throw new Error('parallel expects functions');
    return task();
  }));
}
async function pipeline(items, ...stages) {
  if (!Array.isArray(items) || stages.some(stage => typeof stage !== 'function'))
    throw new Error('pipeline expects items and stage functions');
  return await Promise.all(items.map(async item => {
    let value = item;
    for (const stage of stages) value = await stage(value);
    return value;
  }));
}
async function phase(title, work) {
  await tools.workflow_bridge({op: 'phase', title});
  if (work !== undefined) {
    if (typeof work !== 'function') throw new Error('phase work must be a function');
    return await work();
  }
}
