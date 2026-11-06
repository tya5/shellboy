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
      paginate: async (method, params) => {
      if (method === 'runs') {
        assert.equal(params.head_sha, context.sha);
        assert.equal(params.status, 'completed');
        return runs;
      }
      return selected;
    }};
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

// #245 docs-only decision
const BEFORE = 'a'.repeat(40);
const docsContext = {...context, payload: {...context.payload, before: BEFORE, after: 'b'.repeat(40)}};
function compareApi(files) {
  return {rest: {repos: {compareCommitsWithBasehead: 'compare'}},
    paginate: async (method, params) => {
      assert.equal(method, 'compare');
      assert.equal(params.basehead, `${BEFORE}...${'b'.repeat(40)}`);
      return [{files: files.map(f => typeof f === 'string' ? {filename: f} : f)}];
    }};
}
const docsOnly = async (files, ctx = docsContext) => (await gate.decideDocsOnly({github: compareApi(files), context: ctx})).docsOnly;
test('documentation-only diff skips cargo checks', async () => {
  assert.equal(await docsOnly(['docs/development/status.md', 'docs/design/x.md', 'README.md', 'docs/assets/a.png']), true);
});
test('any code, script, workflow or nested md file forces full run', async () => {
  for (const path of ['src/lib.rs', 'scripts/ci-reuse.cjs', '.github/workflows/ci.yml', 'Cargo.toml', 'config.example.toml', 'src/notes.md']) {
    assert.equal(await docsOnly(['docs/a.md', path]), false, path);
  }
});
test('embedded guides force full run, including renames out of them', async () => {
  assert.equal(await docsOnly(['docs/guides/user-guide.md']), false);
  assert.equal(await docsOnly(['docs/guides/user-guide.ja.md']), false);
  assert.equal(await docsOnly([{filename: 'docs/old.md', previous_filename: 'docs/guides/agent-guide.md'}]), false);
  assert.ok(gate.embeddedDocPrefixes.includes('docs/guides/'));
});
test('undeterminable diffs run everything', async () => {
  assert.equal(await docsOnly([]), false);
  assert.equal(await docsOnly(new Array(3000).fill('docs/a.md')), false);
  for (const before of [undefined, '0'.repeat(40), 'short']) {
    assert.equal(await docsOnly(['docs/a.md'], {...docsContext, payload: {...docsContext.payload, before}}), false);
  }
  assert.equal(await docsOnly(['docs/a.md'], {...docsContext, eventName: 'workflow_dispatch'}), false);
  assert.equal(await docsOnly(['docs/a.md'], {...docsContext, ref: 'refs/heads/other'}), false);
});
test('entrypoint sets docs_only; API failure and reuse fall back to false', async () => {
  const run = async (github, ctx) => {
    const output = {};
    const core = {setOutput: (k, v) => output[k] = v, info() {},
      summary: {addHeading() {return this;}, addRaw() {return this;}, async write() {}}};
    await gate({github, context: ctx, core});
    return output;
  };
  const noRuns = files => ({...compareApi(files), paginate: async (m, p) =>
    m === 'compare' ? compareApi(files).paginate(m, p) : []});
  const github = files => ({rest: {actions: {listWorkflowRuns: 'runs'}, repos: {compareCommitsWithBasehead: 'compare'}},
    paginate: noRuns(files).paginate});
  assert.deepEqual(await run(github(['docs/a.md']), docsContext), {reuse: 'false', docs_only: 'true'});
  assert.deepEqual(await run(github(['src/a.rs']), docsContext), {reuse: 'false', docs_only: 'false'});
  const failing = {rest: {actions: {listWorkflowRuns: 'runs'}, repos: {compareCommitsWithBasehead: 'compare'}},
    paginate: async m => { if (m === 'compare') throw Error('boom'); return []; }};
  assert.equal((await run(failing, docsContext)).docs_only, 'false');
});
test('every Python test referencing docs/ is in the docs-checks list', () => {
  const fs = require('node:fs'), path = require('node:path');
  const dir = __dirname;
  const referencing = fs.readdirSync(dir).filter(f => f.startsWith('test_') && f.endsWith('.py') &&
    /docs\/(development|validation|assets)|README\.md/.test(fs.readFileSync(path.join(dir, f), 'utf8')));
  for (const f of referencing) assert.ok(gate.docTestFiles.includes(f), f);
  for (const f of gate.docTestFiles) assert.ok(fs.existsSync(path.join(dir, f)), f);
});
