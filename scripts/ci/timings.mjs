#!/usr/bin/env node
import { execFileSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const seconds = (start, end) => start && end
  ? Math.max(0, Math.round((Date.parse(end) - Date.parse(start)) / 1000)) : null;

export function summarize(run, jobs, now = new Date().toISOString()) {
  const measured = jobs.map(job => {
    // GitHub can populate started_at for a queued job. A runner step is the
    // evidence that execution began. Skipped jobs consumed no runner time.
    const stepStart = job.steps?.find(step => step.started_at)?.started_at;
    const started = stepStart || null;
    return {
      name: job.name, status: job.status, conclusion: job.conclusion,
      runner: job.runner_name || null,
      pre_execution_seconds: job.conclusion === 'skipped' ? null
        : seconds(run.created_at, started || job.completed_at || (run.status === 'completed' ? run.updated_at : now)),
      execution_seconds: started ? seconds(started, job.completed_at || now) : null,
      steps: (job.steps || []).map(step => ({
        name: step.name, status: step.status, conclusion: step.conclusion,
        seconds: step.started_at ? seconds(step.started_at, step.completed_at || now) : null,
      })),
    };
  });
  return {
    id: run.id, workflow: run.name, sha: run.head_sha, branch: run.head_branch,
    event: run.event, status: run.status, conclusion: run.conclusion, url: run.html_url,
    created_at: run.created_at, captured_at: now,
    elapsed_seconds: seconds(run.created_at, run.status === 'completed' ? run.updated_at : now),
    jobs: measured,
  };
}

export function markdown(runs) {
  const fmt = seconds => seconds == null ? '—' : `${Math.floor(seconds / 60)}m${String(seconds % 60).padStart(2, '0')}s`;
  const escape = value => String(value ?? '—').replaceAll('|', '\\|').replaceAll('\n', ' ');
  const lines = ['# CI timing snapshot', '', `Captured: ${runs[0]?.captured_at || new Date().toISOString()}`, '',
    'Pre-execution elapsed includes dependencies, concurrency gates, approvals, and runner queue time; it is not a pure runner-queue measurement. Active runs are partial. Skipped jobs have no execution time. Completed workflow elapsed uses GitHub’s updated_at and may include finalization.', ''];
  for (const run of runs) {
    lines.push(`## ${escape(run.workflow)} · [${run.id}](${run.url})`, '',
      `${run.sha.slice(0, 8)} · ${escape(run.branch)} · ${run.status}/${run.conclusion || 'pending'} · elapsed ${fmt(run.elapsed_seconds)}`, '',
      '| Job | State | Before execution | Execution | Longest step |',
      '| --- | --- | ---: | ---: | --- |');
    for (const job of run.jobs) {
      const slow = job.steps.filter(step => step.seconds != null).sort((a,b) => b.seconds-a.seconds)[0];
      lines.push(`| ${escape(job.name)} | ${job.conclusion || job.status} | ${fmt(job.pre_execution_seconds)} | ${fmt(job.execution_seconds)} | ${slow ? `${escape(slow.name)} (${fmt(slow.seconds)})` : '—'} |`);
    }
    lines.push('');
  }
  return lines.join('\n');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [repo = 'gakonst/nanocodex', limit = '12', output = 'ci-timings', ...ids] = process.argv.slice(2);
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo) || !/^\d+$/.test(limit) || Number(limit) < 1 || Number(limit) > 100 || ids.some(id => !/^\d+$/.test(id))) {
    throw new Error('usage: node scripts/ci/timings.mjs [owner/repo] [1-100 recent runs] [output prefix] [optional run IDs...]');
  }
  const api = path => JSON.parse(execFileSync('gh', ['api', path], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }));
  const runs = ids.length
    ? ids.map(id => api(`repos/${repo}/actions/runs/${id}`))
    : api(`repos/${repo}/actions/runs?per_page=${limit}`).workflow_runs;
  const snapshots = runs.map(run => {
    const jobs = [];
    for (let page = 1; ; page++) {
      const batch = api(`repos/${repo}/actions/runs/${run.id}/jobs?per_page=100&page=${page}`).jobs;
      jobs.push(...batch);
      if (batch.length < 100) break;
    }
    return summarize(run, jobs);
  });
  writeFileSync(`${output}.json`, JSON.stringify(snapshots, null, 2) + '\n');
  writeFileSync(`${output}.md`, markdown(snapshots));
  console.log(`Wrote ${output}.json and ${output}.md (${snapshots.length} runs)`);
}
