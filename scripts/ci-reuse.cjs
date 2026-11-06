// Only genuine first-attempt native checks qualify; a reuse-only run cannot chain.
const requiredJobs = [
  'check (ubuntu-latest)', 'check (macos-latest)', 'check (windows-latest)',
];

// Docs-only push detection (#245). A path is documentation when it is under docs/
// or is a top-level *.md. Documentation that Rust code or tests read at build or
// test time must still force the full run; keep that list here, in one place.
// Why: src/docs.rs, src/ui/operations.rs and src/frontend/ui_diagnostics.rs
// include_str! the files under docs/guides/ (bundled `shellboy docs` guides and
// user-guide embeds), so editing them changes compiled output and Rust tests.
const embeddedDocPrefixes = ['docs/guides/'];
// Python tests that reference docs/ paths; the docs-checks job runs exactly these
// (workflow reads this list via node). Add a test here when it starts referencing docs/.
const docTestFiles = [
  'test_validation_documents.py', 'test_readme_overview.py', 'test_record_delta.py',
];
const maxComparedFiles = 3000; // compare API listing limit; beyond it the diff is incomplete

function isLightDoc(path) {
  if (embeddedDocPrefixes.some(prefix => path.startsWith(prefix))) return false;
  return path.startsWith('docs/') || /^[^/]+\.md$/.test(path);
}

function classifyFiles(files) {
  if (!Array.isArray(files) || files.length === 0 || files.length >= maxComparedFiles) {
    return {docsOnly: false, reason: 'Changed files undeterminable or empty; executing full checks.'};
  }
  const paths = files.flatMap(file => [file.filename, file.previous_filename]).filter(Boolean);
  const heavy = paths.filter(path => !isLightDoc(path));
  return heavy.length === 0
    ? {docsOnly: true, reason: `Documentation-only push (${files.length} files); running light document checks only.`}
    : {docsOnly: false, reason: `Non-light path ${heavy[0]} changed; executing full checks.`};
}

async function decideDocsOnly({github, context}) {
  if (context.eventName !== 'push' || context.ref !== 'refs/heads/main') {
    return {docsOnly: false, reason: 'Docs-only skipping applies only to main push.'};
  }
  const before = context.payload?.before, after = context.payload?.after || context.sha;
  if (!/^[0-9a-f]{40}$/.test(before || '') || /^0+$/.test(before) || !after) {
    return {docsOnly: false, reason: 'No usable before SHA; executing full checks.'};
  }
  const comparison = await github.paginate(github.rest.repos.compareCommitsWithBasehead, {
    ...context.repo, basehead: `${before}...${after}`, per_page: 100,
  });
  // paginate over compare returns the comparison objects (or merged file pages); normalise.
  const files = comparison.flatMap(item => item.files ? item.files : [item]).filter(item => item.filename);
  return classifyFiles(files);
}

async function decide({github, context}) {
  if (context.eventName !== 'push' || context.ref !== 'refs/heads/main') {
    return {reuse: false, reason: 'Manual checks always execute; reuse applies only to main push.'};
  }
  const repo = {...context.repo};
  const runs = await github.paginate(github.rest.actions.listWorkflowRuns, {
    ...repo, workflow_id: 'ci.yml', head_sha: context.sha, status: 'completed', per_page: 100,
  });
  for (const run of runs) {
    if (run.id === context.runId || run.head_sha !== context.sha ||
        run.status !== 'completed' || run.conclusion !== 'success' || run.run_attempt !== 1 ||
        run.head_repository?.id !== context.payload.repository.id ||
        !['push', 'workflow_dispatch'].includes(run.event)) continue;
    const jobs = await github.paginate(github.rest.actions.listJobsForWorkflowRun, {
      ...repo, run_id: run.id, filter: 'latest', per_page: 100,
    });
    if (requiredJobs.every(name => jobs.filter(job => job.name === name).length === 1 &&
        jobs.find(job => job.name === name).conclusion === 'success')) {
      return {reuse: true, reason: `Reusing first-attempt native checks for ${context.sha}: ${run.html_url}`};
    }
  }
  return {reuse: false, reason: `No qualifying three-OS run for ${context.sha}; executing full checks.`};
}

module.exports = async function(args) {
  let result;
  try {
    result = await decide(args);
  } catch (error) {
    result = {reuse: false, reason: `Actions API evidence unavailable; executing full checks (${error.message}).`};
  }
  let docs = {docsOnly: false, reason: 'Exact-SHA evidence reused; no docs-only decision needed.'};
  if (!result.reuse) {
    try {
      docs = await decideDocsOnly(args);
    } catch (error) {
      docs = {docsOnly: false, reason: `Changed files unavailable; executing full checks (${error.message}).`};
    }
  }
  args.core.setOutput('reuse', String(result.reuse));
  args.core.setOutput('docs_only', String(docs.docsOnly));
  args.core.info(result.reason);
  args.core.info(docs.reason);
  await args.core.summary.addHeading('Exact-SHA CI reuse').addRaw(`${result.reason}\n\n${docs.reason}`).write();
  return {...result, docsOnly: docs.docsOnly};
};
module.exports.decide = decide;
module.exports.requiredJobs = requiredJobs;
module.exports.decideDocsOnly = decideDocsOnly;
module.exports.classifyFiles = classifyFiles;
module.exports.embeddedDocPrefixes = embeddedDocPrefixes;
module.exports.docTestFiles = docTestFiles;
