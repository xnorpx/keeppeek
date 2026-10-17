# Wiki publishing

## One source, two generated destinations

Edit Markdown in `book/src/` and the selected `docs/` pages through repository pull requests.
Never maintain a second copy in the Wiki editor. The
[Wiki](https://github.com/xnorpx/keeppeek/wiki) is the intended primary reading destination after
the live checks below pass. The [mdBook site](https://xnorpx.github.io/keeppeek/) remains an
automatically generated mirror, built by `.github/workflows/deploy.yml`. Neither destination
requires manual content updates.

`book/src/SUMMARY.md` defines book chapter order and navigation. The Wiki generator in
`ui/scripts/` defines the selected engineering pages explicitly. Book pages use their source
basename without `.md`; engineering pages use `Engineering-` followed by that basename. Names are
case-insensitively unique. New chapters must appear in `SUMMARY.md`; new engineering pages must
be selected in the generator manifest. Never rename a published page casually: inbound links
depend on its name.

`Home.md` introduces the documentation and lists all selected pages. `_Sidebar.md` provides the
same navigation on every Wiki page. Referenced local images are copied into the Wiki and links
are rewritten for GitHub's Wiki routes. Unselected repository files link to the exact source
commit instead of being silently copied. Markdown headings, tables, code, and Mermaid fences
remain Markdown; GitHub renders the diagrams natively.

`.wiki-manifest.json` records source commit, page mapping, and generated file hashes. It defines
the publisher's ownership boundary, not an editable source. Generated files belong under ignored
`target/wiki/`, never `book/src/` or `docs/`.

Generation is bounded to 256 source pages, 2 MiB per Markdown input, 32 MiB per asset, 128 MiB
total, and 1,024 generated files. Oversized input, missing targets, duplicate names, unsafe paths,
symlinks, and unsupported mdBook includes fail validation instead of producing incomplete pages.
The Markdown parser is the exact-pinned `mdast-util-from-markdown` development dependency; source
offsets allow destination-only edits without reformatting untouched Markdown or code.

## Local preview and validation

Use the repository's pinned Bun version (`ui/.bun-version`) and public npm registry:

```sh
cd ui
bun install --no-save --ignore-scripts --registry=https://registry.npmjs.org/
bun run wiki:typecheck
bun run wiki:test
bun run wiki:check
bun run wiki:build
cd ..
mdbook build book
bun run --cwd ui format:markdown:check
./check.sh
```

On Windows, use `.\check.bat` for the last command. Install `mdbook` and `mdbook-mermaid`
for the book build. `wiki:check` validates the real source without retaining its temporary output.
`wiki:build` writes `target/wiki/`; generation finishes in staging before exposing the completed
output. A failed conversion must leave a previous output intact.

Inspect generated Home, sidebar, links, images, tables, and Mermaid source. Use
`mdbook serve book` to preview the book. A local Markdown viewer or mdBook preview does **not**
prove GitHub Wiki rendering: Wiki URLs deliberately target GitHub, not local Markdown files.
The CI **Book** job runs the generator tests and source validation, retains the mdBook check,
and uploads `wiki-preview` for inspection without any Wiki credential.

## Owner setup and credential boundary

Publication is blocked until the repository owner completes these steps:

1. Enable the repository Wiki and create its first page. GitHub only creates the Wiki Git
   repository after the initial page exists. See
   [GitHub's Wiki editing instructions](https://docs.github.com/en/communities/documenting-your-project-with-wikis/adding-or-editing-wiki-pages).
2. Create a dedicated automation identity with access only to `xnorpx/keeppeek`. Prefer an
   expiring fine-grained personal access token restricted to this repository, with **Contents:
   read and write** and no other optional permissions. Do not assume the main repository's
   `GITHUB_TOKEN` can push to the separate Wiki Git repository. GitHub Wiki credential support
   must be tested; if this narrow credential cannot write the Wiki, stop and resolve access
   with the owner rather than broadening it silently.
3. Verify clone and normal push access using that identity in an owner-controlled environment,
   without putting the token in a remote URL, command history, or logs. Record credential type,
   repository restriction, expiry, rotation owner, test date, and sanitized result in the
   reviewed setup evidence. Never record the token itself.
4. Create a GitHub Actions environment named `wiki`. Restrict deployments to `main`, and add
   required reviewers if desired. Store the verified credential as its `WIKI_PUBLISH_TOKEN`
   secret. The repository owner owns expiry monitoring and rotation; test a replacement
   before revoking the old credential.
5. Back up the initialized Wiki. Generated filenames must not collide with manually maintained
   pages. If the placeholder `Home.md` conflicts, deliberately remove it in a reviewed Wiki
   commit after backing it up. Preserve unrelated pages. Do not remove `.wiki-manifest.json`
   after a successful publication.
6. Run **Publish documentation to Wiki** manually on `main`, inspect the run and Wiki commit,
   and complete the live verification below before announcing the Wiki as canonical.

The workflow runs only for pushes to `main` affecting documentation/tooling and manual dispatch
on `main`, in exactly `xnorpx/keeppeek`. Forks, other refs, and pull requests cannot enter its
jobs. It never uses `pull_request_target`. Workflow permissions are `contents: read`.
Dependency installation, tests, and generation run in a separate job without the Wiki secret.
The publish job downloads only that run's validated artifact and installs no project dependencies.
Only its final step receives the environment secret. Git authenticates through a credential
helper; neither the clone URL nor the committed configuration contains the token.

The publisher verifies the manifest and all conflicts before synchronizing owned files into a
fresh temporary Wiki checkout. It removes only obsolete previously generated files and refuses
manual changes to generated content. It makes one commit naming the source SHA, skips unchanged
output, and pushes normally to the cloned Wiki's default branch. It never force-pushes.
Concurrent remote changes cause a visible rejected push, not overwritten history.

## Failure and recovery

Open **Actions → Publish documentation to Wiki → failed job** and inspect the error. Generation,
credential denial, missing Wiki, network failures, conflicts, and rejected pushes fail the run.
No generated source tree or publication commit is written to the main repository.

- **Generation or link error:** fix the named source/manifest path in a repository PR. Inspect the
  CI preview artifact, merge the correction, and let the trusted-main workflow run again.
- **Missing secret, expired credential, disabled or uninitialized Wiki:** complete owner setup or
  rotate the environment secret. Re-run the workflow on `main`; do not expose credentials to a PR.
- **Manual-file collision or edited generated page:** back up the Wiki, move useful edits into a
  source PR, and restore the generated file to its last recorded version. Preserve unrelated files.
  Do not bypass the manifest's integrity checks.
- **Network failure or concurrent push:** inspect the remote first; a lost push response may still
  mean the commit arrived. Re-run on `main` to clone current history and rebuild. Do not retry with
  force-push or reset remote history.
- **Bad published content:** make a reviewed `git revert` of the bad Wiki commit, then correct the
  repository sources through normal review and publish again. Reverting restores the preceding
  ownership manifest too. Never rewrite Wiki history destructively.

## Live acceptance procedure and current status

Initial implementation evidence found that the public Wiki URL redirected to the repository and
the Wiki Git read probe failed authentication. No validated publication credential was available.
These observations do not prove whether the Wiki is disabled or only uninitialized. Live
publication, credential scope, and rendering remain **unverified** until owner setup succeeds;
the issue must remain open.

After merging a harmless documentation change through review:

1. Record its source SHA, successful publication Actions URL, artifact, and resulting Wiki commit
   SHA. Confirm `.wiki-manifest.json` names that source SHA and the change appears in the Wiki.
2. Open Home and follow every sidebar page. Compare all selected `SUMMARY.md` chapters and
   engineering pages against the manifest. Expect no missing or duplicate destinations.
3. Follow converted page links and fragments, including an engineering cross-link. Open every
   copied image. Expect Wiki URLs/assets rather than mdBook-relative `.html` or `.md` paths.
4. At desktop and narrow widths, inspect a long table, code fence, image, and the diagrams in
   **Users and design choices**. Expect readable content and rendered Mermaid, not empty output.
5. Dispatch on a non-main ref and in a fork: expect skipped publication jobs. A PR's Book job
   must upload only a preview and have no Wiki secret. In an owner-controlled test, verify a
   denied credential and a concurrent Wiki update produce failed runs without overwritten content.
6. Record all observed results in the implementing PR's acceptance table. Keep failed or
   unverified outcomes unchecked. A successful local generator is not evidence of publication.
