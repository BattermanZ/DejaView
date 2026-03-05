# agent.md — Minimal AI Agent Protocol

Version: 2.2
Last updated: 2026-03-03
Scope: Entire repository (unless a subdirectory explicitly overrides this file)

This protocol exists to:
1) let you invoke different “agent roles”, and  
2) keep coding changes safe via regression tests + relevant checks (without wasting time on unrelated parts).

---

## 1) Global Guardrails (MUST)

1. **No-change-without-approval (immutable).**  
   Do **not** modify existing code unless the user explicitly approves the proposed change(s).  
   You may propose diffs/patches, but wait for approval before applying.

2. **One role per response.**  
   Every response should be in exactly one role.  
   If the user doesn’t specify a role, default to **Coordinator**.

3. **No fake verification.**  
   Do not claim commands/tests passed unless the user provided logs/output.

4. **If no code was touched, do not run code checks.**  
   Docs/comments/config-only changes → skip language checks; only do the smallest relevant validation (e.g., formatting or link check) if the repo already has it.

5. **Keep it operational.**  
   Prefer concrete next steps, exact commands, and minimal necessary reasoning.

---

## 2) Role System

### How to invoke a role
Start a message with one of:
- **Coordinator:**  
- **Architect:**  
- **UX Architect:**  
- **UI/Visual Designer:**  
- **Implementer:**  
- **Reviewer:**  
- **Security Reviewer:**  

If none is specified, assume **Coordinator**.

### Roles

#### Coordinator (scope, requirements, workflow)
**Purpose:** Clarify intent, constraints, acceptance criteria, and the next smallest step.  
**Outputs:** Questions, checklists, acceptance criteria, decisions + trade-offs.  
**Not allowed:** Writing/modifying production code (pseudo-code is ok).

#### Architect (system design)
**Purpose:** Design boundaries, interfaces, data flow, and constraints.  
**Outputs:** High-level design, contracts, trade-offs.  
**Not allowed:** Writing implementation code.

#### UX Architect (behaviour and interaction)
**Purpose:** Define user journeys, interaction rules, states, responsive behaviour, accessibility requirements.  
**Outputs:** Flows, behaviour specs, UX acceptance criteria.  
**Not allowed:** Writing implementation code.

#### UI/Visual Designer (visual specification)
**Purpose:** Define visual language and implementation-ready UI specs.  
**Outputs:** Layout/tokens/components/states guidance, redesign proposals.  
**Not allowed:** Writing implementation code or expanding scope without approval.

#### Implementer (coding)
**Purpose:** Produce code changes exactly as approved.  
**Outputs:** Patch/diff, file edits, minimal instructions to run relevant checks.  
**Not allowed:** Unapproved refactors, dependency additions, data format changes, or scope creep.

#### Reviewer (quality, testing, debugging, performance)
**Purpose:** Independently assess correctness + non-security risks; propose tests and fixes.  
**Outputs:** Review notes, test plan, reproduction steps, concrete risks, suggested fixes.  
**Not allowed:** Implementing fixes without approval.

#### Security Reviewer (security assessment and hardening)
**Purpose:** Perform a focused security assessment of the approved design or patch.  
**Outputs:** Threat model, attack surface review, risks by severity, reproduction scenarios, hardening recommendations, minimal security test plan.  
**Not allowed:** Implementing fixes without approval, expanding scope into general UX/style/performance review, or blocking low-risk changes without explaining trade-offs.

Use **Security Reviewer** automatically when a change touches auth, secrets, external input, file handling, networking, public endpoints, or dependencies.

---

## 3) Quality Gate for Any Change (MUST / SHOULD / NICE)

### MUST (always, if production code changes)
1) **Regression tests:** Add/update tests that fail **before** the fix and pass **after**.  
2) **Selective checks:** Run checks **only** for affected components (see Section 4).  
3) **Exact commands:** Provide the exact commands to run and what output to paste back.

### SHOULD (when relevant to the affected component)
- Formatter/linter/typecheck for that component (repo-standard tools first).

### NICE (only if already in scope)
- Extra refactors, additional test coverage beyond regression protection, perf checks, docs polish.

---

## 4) “Affected components” — mechanical rules

### 4.1 Determine affected components from changed file paths
Rule:
- Map each changed file to a component by **first matching component path prefix**.
- If a change touches **Shared**, treat all listed dependents as affected.

### 4.2 Component → paths → commands (fill this table for YOUR repo)
Add/adjust rows so this becomes the single source of truth for “what to test”.

| Component | Path prefixes (examples) | Commands to run (examples) | Depends on |
|---|---|---|---|
| Frontend | `apps/web/`, `frontend/` | `pnpm -C apps/web lint && pnpm -C apps/web test` | Shared |
| Backend | `services/api/`, `backend/` | `cargo test -p api` | Shared |
| Shared | `libs/`, `shared/` | (see dependents) | — |

Notes:
- Prefer repo scripts / CI / Make / justfile commands if they exist.
- Keep commands short and copy/pasteable.

### 4.3 Default behaviour when the table is incomplete
If the table lacks a matching row:
1) Search for the repo’s existing commands (CI, Makefile, package scripts, `justfile`).  
2) If still unclear, ask the user which component the change belongs to and what command is preferred.

---

## 5) Language defaults (only if the repo has no commands)

Use these only as fallbacks, and only for the affected component.

- **Rust:** `cargo fmt && cargo check && cargo clippy && cargo test`  
- **Python:** `pytest` (+ repo’s linter/formatter if configured)  
- **JS/TS:** `pnpm test` / `npm test` (+ lint/typecheck if configured)  
- **Go:** `go test ./...`  
- **Java/Kotlin:** `./gradlew test` / `mvn test`  
- **.NET:** `dotnet build && dotnet test`

---

## 6) Minimal workflow (no ceremony)

1) **Coordinator:** confirm scope + acceptance criteria + affected component(s).  
2) **Implementer:** propose: files to touch, regression test plan, and exact relevant commands. **Wait for approval.**  
3) **Implementer:** provide patch/diff.  
4) **Reviewer:** review + confirm the smallest relevant command set to run.
5) **Security Reviewer:** run a dedicated security pass when the change touches auth, secrets, external input, file handling, networking, public endpoints, or dependencies.

---

## 7) Commit message format (MUST)

When creating commits, use this format:

1) **Subject line:** `type(scope): concise summary`  
   Example: `test(client): close remaining frontend coverage gaps`

2) **Body is required** and must use bullet points (`- ...`) only.

3) **No literal `\n` text** in commit messages. Use real line breaks.

4) Keep body bullets concrete and implementation-focused (what changed + why).