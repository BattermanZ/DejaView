# Issue tracker: GitHub

Tickets for DejaView live as GitHub issues on `BattermanZ/DejaView`. Use the
`gh` CLI where it is installed, and the session's GitHub tools where it is not.
`gh` infers the repository from `git remote -v` inside a clone.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`, with a
  heredoc for a multi-line body.
- **Read an issue**: two calls, both `--json`:
  `gh issue view <number> --json number,title,state,assignees,labels,body,url`
  then `gh issue view <number> --json comments`. **Read both.** A ticket's
  brief is as often a comment as the body, so the body alone can miss the
  contract. `--json` is also the form that works on gh 2.45, where every other
  form of `gh issue view` prints a one-line "Projects (classic) is being
  deprecated" error in place of the issue: that line is a tooling fault, never
  an answer about the issue.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`,
  with `--label` and `--state` to filter.
- **Comment**: `gh issue comment <number> --body "..."`
- **Label**: `gh issue edit <number> --add-label "..."` and `--remove-label "..."`
- **Claim**: `gh issue edit <number> --add-assignee @me`
- **Close**: `gh issue close <number> --comment "..."`, for a ticket that ends
  with no pull request. A ticket built in code is closed by its pull request
  merging (`AGENTS.md`, "Branching").

## Labels

The triage roles are labels of the same name: the categories `bug` and
`enhancement`, and the states `needs-triage`, `needs-info`, `ready-for-agent`,
`ready-for-human` and `wontfix`. A ticket an agent files for the maintainer to
approve takes `needs-triage`.

**`ready-for-agent` goes on a ticket to build, never on a parent spec.** A spec
whose work is its child tickets (each child names it under `## Parent`) is read
when a child needs it and closed when its last child closes. Labelled, it would
sit first in the queue, and every session would fetch it whole before passing
it over.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external
PRs as feature requests; `/triage` reads this flag.)_

## When a skill says "publish to the issue tracker"

Create a GitHub issue.

## When a skill says "fetch the relevant ticket"

Run both `--json` reads under "Read an issue", and read both.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as
tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes,
  Decisions-so-far and Fog body. `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`gh api`
  on the sub-issues endpoint). Where sub-issues are not enabled, add the child
  to a task list in the map body and put `Part of #<map>` at the top of the
  child body. Labels: `wayfinder:<type>` (`research`, `prototype`, `grilling`
  or `task`). Once claimed, the ticket is assigned to whoever drives it.
- **Blocking**: GitHub's native issue dependencies. Add an edge with
  `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`,
  where `<blocker-db-id>` is the blocker's numeric database id
  (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, not the `#number` or
  `node_id`). GitHub reports `issue_dependencies_summary.blocked_by`, which
  counts open blockers only. Where dependencies are not available, put a
  `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is
  unblocked when every blocker is closed.
- **Frontier query**: list the map's open children, drop any with an open
  blocker or an assignee; the first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me`, the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then
  `gh issue close <n>`, then append a context pointer (gist and link) to the
  map's Decisions-so-far.
