Read the latest stdout snapshot of a long-running shell command (one that was started with `run_shell_command` `wait_until_complete=false`).

You will need the `command_id` from the original `LongRunningCommandSnapshot`.

The returned snapshot contains:
- `output` — current stdout buffer.
- `is_alt_screen_active` — true when the underlying program is using the terminal's alternate screen (vim, htop, less, full-screen TUIs). When this is true, `output` may not reflect what the user visually sees.
- `command_id` — same id, returned for convenience.

Typical loop:
1. Start a server / watcher with `run_shell_command(wait_until_complete=false)`.
2. Use this tool with a short `delay_seconds` value (usually 5-10 seconds) to poll output until you see a "ready"-style line.
3. Move on to running the next step (tests, requests, etc.) in parallel.

Do not wait for command completion when the command may be slow, verbose, or continuous. For commands like `journalctl`, `find`, `du`, `launchctl print`, build logs, dev servers, watchers, or anything with a large pipeline, prefer short polling snapshots so the agent can keep responding.
