---
name: land
description: >-
  Land the current project's changes by committing them on a reusable topic
  branch and synchronizing that branch to the local remote. Invoke this skill
  only when the user explicitly requests landing changes; do not invoke it for
  review, preparation, verification, or skill installation.
metadata:
  delta-action: land
---

# Land changes

Use this skill only after an explicit landing request has been made. The
request that invokes this skill already authorizes the landing workflow; do
not ask whether the user wants to land again.

## Project workflow

This repository uses `master` as its integration branch and has historically
landed changes through topic branches and pull requests. For this skill,
follow the user's project-specific workflow instead: commit the current
changes on a dedicated topic branch and synchronize that branch to the
`local` remote only. Do not push `origin`, open a pull request, or merge into
`master`.

## Branch reuse

1. Inspect the worktree status and current branch without changing Git state.
2. If the current branch is already the dedicated landing branch created by
   this skill, reuse it. This makes repeated Land actions in the same worktree
   use the same branch.
3. Otherwise create a dedicated branch using the repository's `AGENTS.md`
   branch naming convention, with a short, lowercase, hyphenated topic inferred
   from the current change. If no repository convention is present, use
   `codex/land/<topic>`. Do not use a generic branch name when the change has a
   clear topic.
4. Never reuse or reset an unrelated existing branch. If the intended branch
   cannot be determined safely, stop and ask the user.

Preserve unrelated work. Do not discard, stash, reset, amend, rebase, or
overwrite unrelated changes. If the worktree contains changes from more than
one task and their scope cannot be separated safely, stop and ask the user.

## Verification and commit

Before committing, run the checks applicable to the current change. For this
Rust workspace, the repository's release workflow confirms the locked release
build command as:

```sh
cargo build --locked --release --package lyrune --target "$target"
```

Source: `.github/workflows/release.yml`, “Build Lyrune” step.

For ordinary changes, run the narrowest relevant tests first and also run:

```sh
cargo fmt --all -- --check
cargo test -p lyrune
git diff --check
```

If a check fails, is pending, or cannot be verified, do not commit or
synchronize the changes. Report the failure and leave the worktree otherwise
unchanged. Do not treat the Release workflow as a required ordinary-change
check: it runs only for version tags (`.github/workflows/release.yml`, `on`
configuration).

Review the final diff for unrelated changes, then create a commit using the
project's required format:

```text
<type>: 中文描述
```

Use a conventional type such as `feat`, `fix`, `refactor`, or `chore`.
Do not amend an existing commit unless the user explicitly requests it.

## Synchronization and conflicts

Synchronize the dedicated branch to the `local` remote after all applicable
checks pass. Use a non-interactive Git command and do not force-push.

If any operation reports a conflict, stop immediately. Do not resolve it
automatically. Tell the user:

- that a conflict occurred;
- which files or paths are involved;
- whether it is small and localized or broad and cross-cutting;
- whether it appears confined to the current change or touches unrelated work.

Wait for the user's instructions about conflict resolution. A conflict is not
a successful landing.

## Outcome

Only report success after verifying that the committed branch and commit are
visible on `local`. Include the branch name, short commit SHA, and the
synchronization result. Do not claim success for a prepared commit or a local
branch that was not synchronized.

When running in a subthread and `report_subthread_status` is available, report
the verified landing result to the parent with `status: "success"` only after
the branch is confirmed on `local`. Use `status: "failure"` for failed checks,
conflicts, or a blocked synchronization, and state that the changes were not
landed. Keep the status title short and the description to one line.
