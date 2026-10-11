# Wiki publishing

The repository Markdown is the only editable source for the documentation. The GitHub Wiki is the
primary reading destination, and GitHub Pages remains an automatically generated mdBook mirror.
Changes belong in a reviewed pull request; do not edit generated Wiki pages by hand.

## Page mapping

The generator reads the ordered page list in `book/src/SUMMARY.md`. Each listed Markdown file
becomes a Wiki page named from its file stem. It also publishes the explicit engineering-page list
in `ui/scripts/wiki-manifest.ts`; those pages use the `Engineering-` prefix. Adding a `SUMMARY.md`
entry adds that book page automatically. Links to selected pages become Wiki links. Links to other
repository files point to the immutable source commit.

The generator creates `Home.md` and `_Sidebar.md`. Referenced images are copied beneath
`assets/book/src/` or `assets/docs/`. Mermaid fences and other code fences remain source Markdown.
The generator rejects missing local targets, unsupported mdBook directives, paths outside the
repository, and page-name collisions.

## Local preview and checks

From the repository root, install the UI dependencies and build the Wiki output:

```sh
cd ui
bun install --no-save --registry=https://registry.npmjs.org/
bun run wiki:check
```

The generated files are under `target/wiki/`. Use the Markdown preview in your editor to inspect
`Home.md`, `_Sidebar.md`, changed pages, images, and Mermaid fences. Run `mdbook build book` to
preview the Pages mirror. A local preview does not prove that GitHub renders every Wiki feature;
verify changed Mermaid diagrams and links in the live Wiki after publication.

## Publication and credentials

The `Publish Wiki` workflow runs only for `main` pushes and manual dispatches on `main`. It builds
and validates the output before cloning the separate Wiki Git repository. The write credential is
used only by the final normal `git push` step. The workflow has `contents: read`; the main
repository's `GITHUB_TOKEN` is not used to write the Wiki.

Configure the `wiki-publishing` environment to allow deployments from `main` only, and store
`WIKI_PUBLISH_TOKEN` as an environment secret there.

Before relying on automatic publication, a repository owner must enable the Wiki and create an
initial page so GitHub creates the Wiki Git repository. Confirm that the public Wiki Git remote can
be cloned without credentials, then test a write credential against that remote and configure it as
the `WIKI_PUBLISH_TOKEN` Actions secret. Use a
repository-restricted automation identity and the narrowest permission that actually supports Wiki
Git pushes. Do not assume that the main repository's `GITHUB_TOKEN` or a particular token type can
write the separate Wiki repository. Record the identity owner and expiry in the repository's
credential inventory, and rotate the secret before it expires. Never put the credential in a URL,
workflow log, or repository file.

This workflow is not live-verified until the Wiki is enabled and the owner has confirmed that an
unauthenticated clone and authenticated push to the Wiki remote both work. Missing or denied
credentials, a disabled Wiki, and a concurrent remote update fail the workflow visibly. Publication
never force-pushes.
Generated file conflicts with manually maintained Wiki pages also fail rather than overwrite them.
Create the initial page with a name other than `Home` so the first generated Home page has no
conflict.

## Recovery

If a generated Wiki commit is wrong, clone the Wiki repository, revert that commit, and push the
revert normally. Correct the repository Markdown in a reviewed pull request, merge it to `main`,
and confirm the next publication succeeds. Do not rewrite Wiki history or force-push. If publication
fails because a manual page conflicts with a generated name, move or remove the conflicting manual
page deliberately and rerun the main-branch publisher.
