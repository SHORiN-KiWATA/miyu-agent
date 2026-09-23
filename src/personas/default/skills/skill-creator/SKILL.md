---
name: skill-creator
display_name: 技能创作
summary: 写一份新技能，或安全地改已有的那份
description: Create or safely update Miyu skills. Use when the user asks to create, author, improve or modify a reusable skill, a SKILL.md workflow or skill resources — 写技能、改技能、做一个技能。
compatibility: Miyu built-in skill authoring workflow
---

# Skill Creator

Create focused, reusable Agent Skills that follow the Agent Skills specification.

## Calling manage_skill

`manage_skill` is not on the always-loaded tool list: authoring a skill is a
once-in-a-while action, so it lives behind this skill. Call it through the tool
bridge with `run_command`:

```bash
miyu tool-call manage_skill --stdin <<'JSON'
{"action": "create", "name": "my-skill", "description": "what it does and when to use it"}
JSON
```

`miyu tool-call manage_skill --describe` prints the full parameter contract.
Everything below that says "call `manage_skill`" means this.

## Workflow

1. Ask what the skill should do, when it should trigger, and what it must not do when any of those points are unclear.
2. Prefer instruction-only skills. Add scripts only when deterministic code or an external program is genuinely required.
3. Load the development and skill authoring tools before editing.
4. Use `manage_skill` with `action=create` for a new skill or `action=update` for an existing one. Use the returned absolute `skill_file` and `skill_dir`; never guess a Miyu home path.
5. Use `apply_patch` to edit `SKILL.md` and add optional `scripts/`, `references/`, or `assets/` files below the returned draft directory.
6. Keep `SKILL.md` concise. Put detailed reference material in supporting files and reference those files with paths relative to the skill root.
7. Call `manage_skill` with `action=publish` and the returned draft ID. Publishing performs structural validation and atomically installs the package.
8. Load the published skill with `load_skill` to verify the final instructions and resource manifest.

## Frontmatter

Every skill requires `name` and `description`. The name must match its directory and use lowercase ASCII letters, digits, and single hyphens. The description must explain both what the skill does and when it should be used.

Optional standard fields are `license`, `compatibility`, `metadata`, and `allowed-tools`. `allowed-tools` is compatibility metadata only and never grants Miyu permissions.

## Editing Rules

- Do not write directly into the live skills directory.
- Do not overwrite an existing skill through `action=create`; use `action=update`.
- Skill scripts remain resources. Do not claim that publishing automatically registers them as tools.
- Do not add files or abstractions that the workflow does not need.
