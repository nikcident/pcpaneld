# Contributing

This is a hobby project, shared as-is. There's no commitment to review timelines or feature roadmaps. That said, bug reports and pull requests are welcome.

## Getting started

See the [README](README.md) for build instructions and [CLAUDE.md](CLAUDE.md) for architecture notes and code standards.

## Before submitting a PR

Run the full check suite and make sure it passes with zero warnings:

```bash
just all
```

This runs clippy, fmt check, build, and tests. If you don't have `just`, the raw commands are in `CLAUDE.md`.

## Reporting issues

If something doesn't work, open an issue with:

- What you expected to happen
- What actually happened
- Output of `pcpaneld info` and `journalctl --user -u pcpaneld -n 50` if relevant
