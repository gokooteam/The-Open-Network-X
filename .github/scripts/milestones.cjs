// Helpers for .github/workflows/milestones.yml, called from
// actions/github-script steps.
//
// MILESTONES.md is the source of truth. sync() mirrors it into GitHub
// milestones (the Milestone box in a PR's sidebar); assign() puts each new
// PR in one. A GitHub milestone's own progress bar counts closed issues and
// PRs, not exit criteria, so the criteria count goes in the title, which is
// what the sidebar shows.

'use strict';

const fs = require('fs');

const ID = /^(M\d+)\b/;

function title(m) {
  const progress = m.done ? '✅' : m.total ? `${m.met}/${m.total}` : '…';
  return `${m.id} · ${progress} · ${m.name}`;
}

function description(m) {
  const state = m.done
    ? `Done${m.date ? ` ${m.date}` : ''}.`
    : m.total
      ? `${m.met} of ${m.total} exit criteria met.`
      : 'Exit criteria not written yet.';
  return [
    `${state} Version: ${m.version.replace(/`/g, '')}.`,
    '',
    'Mirrored from MILESTONES.md by the Milestones workflow, which overwrites this',
    'title and description. Tick exit criteria in MILESTONES.md, not here.',
  ].join('\n');
}

// The GitHub milestone for each ONX milestone ID, matched by title prefix
// ("M4 · …"), so renaming a milestone in MILESTONES.md keeps the same one.
async function byId(github, owner, repo, state) {
  const list = await github.paginate(github.rest.issues.listMilestones, {
    owner,
    repo,
    state,
    per_page: 100,
  });
  const map = new Map();
  for (const ms of list) {
    const m = ID.exec(ms.title);
    if (m && !map.has(m[1])) map.set(m[1], ms);
  }
  return map;
}

// Create or update one GitHub milestone per row of MILESTONES.md. Done ones
// are closed. No due dates: a milestone is done when its criteria are, so a
// due date set by hand is cleared. An open "M<n> …" milestone that is no
// longer in MILESTONES.md is closed, so new PRs aren't put in it.
async function sync({ github, context, core }) {
  const { owner, repo } = context.repo;
  const { milestones } = JSON.parse(fs.readFileSync(process.env.MILESTONES_JSON, 'utf8'));
  const existing = await byId(github, owner, repo, 'all');
  for (const m of milestones) {
    const want = { title: title(m), description: description(m), state: m.done ? 'closed' : 'open' };
    const have = existing.get(m.id);
    if (!have) {
      await github.rest.issues.createMilestone({ owner, repo, ...want });
      core.info(`Created ${want.title}`);
    } else if (
      have.title !== want.title ||
      (have.description || '') !== want.description ||
      have.state !== want.state ||
      have.due_on
    ) {
      const clear = have.due_on ? { due_on: null } : {};
      await github.rest.issues.updateMilestone({ owner, repo, milestone_number: have.number, ...want, ...clear });
      core.info(`Updated ${want.title}`);
    }
  }
  const planned = new Set(milestones.map((m) => m.id));
  for (const [id, ms] of existing) {
    if (!planned.has(id) && ms.state === 'open') {
      await github.rest.issues.updateMilestone({ owner, repo, milestone_number: ms.number, state: 'closed' });
      core.info(`Closed ${ms.title}: no longer in MILESTONES.md`);
    }
  }
}

// Give a PR with no milestone the one its title names ("M5: …") if that is
// still open, or else the current one: the lowest-numbered open milestone. A
// milestone someone set by hand is left alone.
async function assign({ github, context, core }) {
  const { owner, repo } = context.repo;
  const pr = context.payload.pull_request;
  if (pr.milestone) {
    core.info(`#${pr.number} already has milestone ${pr.milestone.title}`);
    return;
  }
  const all = await byId(github, owner, repo, 'all');
  const named = /\b(M\d+)\b/.exec(pr.title || '');
  let target = named && all.get(named[1]);
  // A finished (closed) milestone named in the title falls back to the current one.
  if (target && target.state !== 'open') target = undefined;
  if (!target) {
    target = [...all.entries()]
      .filter(([, ms]) => ms.state === 'open')
      .sort(([a], [b]) => Number(a.slice(1)) - Number(b.slice(1)))
      .map(([, ms]) => ms)[0];
  }
  if (!target) {
    core.info('No ONX milestones on GitHub yet; run the Milestones workflow to create them.');
    return;
  }
  await github.rest.issues.update({ owner, repo, issue_number: pr.number, milestone: target.number });
  core.info(`#${pr.number} → ${target.title}`);
}

module.exports = { sync, assign, title, description };
