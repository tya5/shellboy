const {test} = require('node:test');
const assert = require('node:assert/strict');
const gate = require('../ci-reuse.cjs');
const context = {eventName: 'push', ref: 'refs/heads/main', sha: 'abc', runId: 42,
  repo: {owner: 'owner', repo: 'repo'}, payload: {repository: {id: 7}}};
const run = {id: 1, head_sha: 'abc', status: 'completed', conclusion: 'success',
  run_attempt: 1, head_repository: {id: 7}, event: 'workflow_dispatch', html_url: 'https://example/run/1'};
const jobs = gate.requiredJobs.map(name => ({name, conclusion: 'success'}));
function api(runs = [run], selected = jobs) {
  return {rest: {actions: {listWorkflowRuns: 'runs', listJobsForWorkflowRun: 'jobs'}},
    paginate: async method => method === 'runs' ? runs : selected};
}
test('exact-SHA complete first-attempt evidence qualifies', async () => {
  assert.equal((await gate.decide({github: api(), context})).reuse, true);
});
test('manual dispatch never reuses', async () => {
  assert.equal((await gate.decide({github: {}, context: {...context, eventName: 'workflow_dispatch'}})).reuse, false);
});
for (const [label, change] of Object.entries({wrongSHA: {head_sha: 'other'}, rerun: {run_attempt: 2},
  fork: {head_repository: {id: 8}}, self: {id: 42}, failure: {conclusion: 'failure'}})) {
  test(`reject ${label}`, async () => assert.equal((await gate.decide({github: api([{...run, ...change}]), context})).reuse, false));
}
for (const conclusion of ['skipped', 'failure', 'cancelled']) {
  test(`reject ${conclusion} native job`, async () => {
    const selected = jobs.map((job, i) => i === 0 ? {...job, conclusion} : job);
    assert.equal((await gate.decide({github: api([run], selected), context})).reuse, false);
  });
}
test('reject incomplete Windows-only evidence', async () => {
  assert.equal((await gate.decide({github: api([run], jobs.slice(2)), context})).reuse, false);
});
test('API failure records fallback output and summary', async () => {
  const output = {}, summary = [];
  const core = {setOutput: (k, v) => output[k] = v, info: s => summary.push(s),
    summary: {addHeading() {return this;}, addRaw(s) {summary.push(s); return this;}, async write() {}}};
  await gate({github: {rest: {actions: {}}, paginate: async () => {throw Error('unavailable');}}, context, core});
  assert.equal(output.reuse, 'false');
  assert.match(summary.join(' '), /executing full checks/);
});
