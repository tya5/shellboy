// Only genuine first-attempt native checks qualify; a reuse-only run cannot chain.
const requiredJobs = [
  'check (ubuntu-latest, all)', 'check (macos-latest, all)',
  'check (windows-latest, 1)', 'check (windows-latest, 2)', 'Windows inventory audit',
];

async function decide({github, context}) {
  if (context.eventName !== 'push' || context.ref !== 'refs/heads/main') {
    return {reuse: false, reason: 'Manual checks always execute; reuse applies only to main push.'};
  }
  const repo = {...context.repo};
  const runs = await github.paginate(github.rest.actions.listWorkflowRuns, {
    ...repo, workflow_id: 'ci.yml', head_sha: context.sha, status: 'success', per_page: 100,
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
  args.core.setOutput('reuse', String(result.reuse));
  args.core.info(result.reason);
  await args.core.summary.addHeading('Exact-SHA CI reuse').addRaw(result.reason).write();
  return result;
};
module.exports.decide = decide;
module.exports.requiredJobs = requiredJobs;
