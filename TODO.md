# Watch the compose file for changes and auto-reload the context

## Implementation Plan

### Overview
Implement file watching for the compose file and automatically reload the compose context when changes are detected. This will allow adding/removing services and updating schedules without restarting the application.

**Simplified approach**: Factor the scheduler setup logic into a separate function. When compose file changes are detected, abort the function cleanly (ensuring no orphan tasks), then restart it to reparse the compose file.

### Steps

1. **Add file watching dependency**
   - Add `notify` crate to `Cargo.toml` for cross-platform file system notifications

2. **Factor scheduler setup into separate function**
   - Extract everything after `main.rs:173` (after `context` creation) into a new function, e.g., `run()`
   - Function signature should accept:
     - `ComposeContext`
     - All necessary args (slack webhooks, run_logs, monitor configs, etc.)
     - `CancellationToken` for aborting when file changes
   - Function should return `Result<()>`

3. **Implement cancellation mechanism**
   - Use `tokio::sync::CancellationToken` to signal when compose file changes
   - Pass cancellation token to `run_scheduler_setup()` function
   - Make all spawned tasks check for cancellation:
     - Modify `run_on_schedule()` to check token periodically or during sleeps
     - Modify `container_monitor::run()` to accept and check token
     - Modify `system_monitor::run()` to accept and check token
     - Modify `certificate_monitor::run()` to accept and check token
     - Modify `scheduler::run_command_on_schedule()` to accept and check token
     - Modify `status_server::run_status_server()` to accept and check token
   - In `run()`, after `scheduler.join_all().await`, check if cancellation was requested
   - Ensure all tasks are properly aborted before function returns

4. **Implement file watcher**
   - Create a file watcher function that watches the compose file
   - Use `notify` crate to watch for changes (CREATE, MODIFY, REMOVE events)
   - Debounce file change events (handle rapid file saves) - wait ~500ms after last event
   - When change detected, trigger cancellation token
   - Wait for `run()` to complete (all tasks aborted)
   - Restart `run()` with fresh context

5. **Update main() function**
   - After creating `context` (line 173), create a cancellation token
   - Spawn file watcher task (watches compose file, triggers cancellation on change)
   - In a loop:
     - Call `run()` with cancellation token
     - If it returns (due to cancellation), log the reload
     - Wait a moment, then restart (create new cancellation token)
   - If `run()` returns an error (not cancellation), propagate it

6. **Update task cancellation in spawned functions**
   - All long-running tasks should periodically check `cancellation_token.is_cancelled()`
   - When cancelled, tasks should exit cleanly (return from their loops)
   - Tasks should not panic on cancellation - just return gracefully

7. **Error handling**
   - Handle file watch errors gracefully (file deleted, permissions, etc.)
   - If file watching fails, continue normally (don't break the application)
   - Log reload attempts and failures
   - If compose file reload fails (parse errors, etc.), keep running with old config, just emit an error to logs

8. **Testing considerations** (theoretical, do not write any code for these)
   - Test with file modifications (add/remove services, change schedules)
   - Test with file deletion/restoration
   - Test debouncing with rapid file saves
   - Verify no orphan tasks remain after cancellation
   - Verify tasks restart cleanly after file change

### Technical Details

- Use `notify::RecommendedWatcher` for platform-specific file watching
- Use `tokio::time::sleep` for debouncing (e.g., 500ms delay after last event)
- Use `tokio::sync::CancellationToken` for clean task cancellation
- Function signature: `async fn run_scheduler_setup(context: ComposeContext, args: &Cli, cancellation_token: CancellationToken) -> Result<()>`
- All spawned tasks should check `cancellation_token.is_cancelled()` at strategic points (between iterations, during sleeps)

### Dependencies to Add
```toml
notify = "8"
```

### Files to Modify
- `Cargo.toml` - add `notify` dependency
- `src/main.rs` - factor scheduler setup into function, add file watcher loop, pass cancellation tokens
- `src/compose.rs` - no changes needed
- `src/container_monitor.rs` - add cancellation token parameter, check for cancellation
- `src/system_monitor.rs` - add cancellation token parameter, check for cancellation
- `src/certificate_monitor.rs` - add cancellation token parameter, check for cancellation
- `src/scheduler.rs` - add cancellation token parameter to `run_command_on_schedule`, check for cancellation
- `src/status_server.rs` - add cancellation token parameter, check for cancellation

### Notes
- Simpler approach: instead of dynamically managing individual tasks, we restart the entire scheduler setup when compose file changes
- This ensures clean state and avoids complex task tracking
- Must ensure all tasks are properly cancelled to avoid orphans
- Tasks should check cancellation token at natural break points (sleep intervals, loop iterations)
- If file watching fails, application should continue normally (graceful degradation)