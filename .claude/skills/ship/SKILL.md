---
name: ship
description: Create a branch (if necessary), commit, push, and open a PR.
user_invocable: true
---

# Ship

When the user invokes `/ship`, perform the following steps in order:

1. **Create a branch** (if not already on a feature branch):
   - If on `main`, create and check out a new branch with a descriptive name (e.g., `feat/add-auth-middleware`)
   - If already on a non-main branch, stay on it

2. **Commit**:
   - Stage all relevant changed files
   - Write a commit message using [Conventional Commits](https://www.conventionalcommits.org/) format
   - Types: `feat`, `fix`, `chore`, `docs`, `ci`, `test`, `refactor`, `perf`
   - Append `!` for breaking changes

3. **Push**:
   - Push the branch to the remote with `-u` to set upstream tracking

4. **Open a PR**:
   - PR title must be a conventional commit line: `type(optional-scope): description`
   - PR body format:
     ```
     ## Summary
     - bullet points describing what changed

     ## Test plan
     - how to verify the changes
     ```
   - This repo uses release-please with squash-merge; the PR title becomes the
     squash commit message that release-please reads for changelog generation
