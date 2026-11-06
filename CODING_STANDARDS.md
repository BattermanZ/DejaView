# Coding standards

Read at review, by whoever reviews a change. An implementer reads `AGENTS.md`.

Only judgement calls live here: what a reviewer has to look at and decide.
Whatever a machine can decide belongs in `just check` or `just test`, and a
rule that gains a check there leaves this file.

## Every file a change can reach is accounted for

Read each path that creates, renames, links or deletes a file, and list what
it can reach besides the download the plan names. The guards `AGENTS.md`
describes compare paths, so the reviewer asks what a path comparison cannot
see: the same directory through a second mount, a path component swapped
between the check and the rename, a file another process is still writing. A
path that can reach the library, or a file the plan did not name, is a finding
even when no test reaches it.

## DejaView says what it did

When DejaView does something other than what it planned (falls back to a
symlink, skips a pair, stops partway), the report and the summary say so and
why, with the OS error where there was one. A new fallback or skip that leaves
no reason in the report is a finding, and so is a count that includes work
that did not happen. `docs/FINDINGS.md` S3 and C3 are what this costs when it
is missed: a symlink a person never knew about, dangling once Sonarr upgrades
the file.

## An error names the file and the next step

A message a person reads names the path involved and what to do about it, as
the error for a broken config file does (`src/config.rs`). A bare OS error
such as "Permission denied" is a finding.

## Committed prose names a role, never a person

The files in this repository are for anyone who runs DejaView, so none of
them names the person who made a decision or whose setup a finding came from.
Write a role: "the maintainer", or "a person running DejaView". Test data uses
invented names. `AGENTS.local.md` stays on one machine and may say who works
there.
