# Agent Guidelines

Language-specific coding guidelines live in `agent/`:

- **Rust** → `agent/AGENT_rust.md`
- **Python** → `agent/AGENT_python.md`
- **Markdown** -> `agent/AGENT_md.md`

Read the file matching the language you are working in before making changes.

Working on a plugin in `plugin/`? Also read `plugin/AGENT.md`: every plugin ships a
`check_config.py`, and that file defines what it checks and how it reports.
