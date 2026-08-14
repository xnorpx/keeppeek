# Copilot Instructions

## Code Style

- **No redundant comments.** Do not write comments that restate what the code already says.
- **No decorative separators.** Never use comment lines made of dashes (`// ---`), equals (`// ===`), or similar characters to visually separate sections.
- **No section headers in comments.** Do not label code sections with banner-style comments like `// -- Foo --` or `// Types`. Let the code structure (modules, `impl` blocks, functions) speak for itself.
- Comments should only explain *why* something is done when it is not obvious from the code. If the reason is obvious, skip the comment entirely.
- Doc comments (`///`) on public API items are fine when they add useful context beyond the function signature.
