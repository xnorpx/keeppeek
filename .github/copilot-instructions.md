# Copilot Instructions

## Agent Skill Routing

Before handling every request or command, read and apply [the using-agent-skills meta-skill](../.agents/skills/using-agent-skills/SKILL.md). Select and follow every additional skill that matches the work before acting. This routing step is mandatory even when no additional skill applies, and repository instructions override generic skill examples.

## Code Style

- **No redundant comments.** Do not write comments that restate what the code already says.
- **No decorative separators.** Never use comment lines made of dashes (`// ---`), equals (`// ===`), or similar characters to visually separate sections.
- **No section headers in comments.** Do not label code sections with banner-style comments like `// -- Foo --` or `// Types`. Let the code structure (modules, `impl` blocks, functions) speak for itself.
- Comments should only explain _why_ something is done when it is not obvious from the code. If the reason is obvious, skip the comment entirely.
- Doc comments (`///`) on public API items are fine when they add useful context beyond the function signature.
